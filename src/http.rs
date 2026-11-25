/**
 * [INPUT]: 依赖 axum 提供路由与请求体；依赖 advertise、push 协议核心；依赖 Config 的固定账号与体积上限
 * [OUTPUT]: 导出 App、router、serve；实现 info/refs 通告与 upload-pack、receive-pack 的字节转发
 * [POS]: HTTP 传输层；只做认证、路径解析、字节转发与 Content-Type，不承载 Git 语义
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;

use crate::config::Config;
use crate::git::advertise::{self, Service};
use crate::git::fetch;
use crate::git::push;
use crate::git::wire::{write_flush, write_pkt};
use crate::store::{validate_repo_name, Store};

pub struct App {
    pub store: Arc<Store>,
    pub config: Arc<Config>,
}

pub fn router(app: Arc<App>) -> Router {
    let max_bytes = app.config.max_push_bytes;
    Router::new()
        .route("/{repo}/info/refs", get(info_refs))
        .route("/{repo}/git-upload-pack", post(upload_pack))
        .route("/{repo}/git-receive-pack", post(receive_pack))
        .layer(DefaultBodyLimit::max(max_bytes))
        .with_state(app)
}

pub async fn serve(app: Arc<App>, listener: tokio::net::TcpListener) -> Result<(), String> {
    axum::serve(listener, router(app))
        .await
        .map_err(|e| format!("HTTP 服务退出: {e}"))
}

fn repo_name(raw: &str) -> Option<String> {
    let name = raw.strip_suffix(".git").unwrap_or(raw);
    validate_repo_name(name).ok()?;
    Some(name.to_string())
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    match text.split_once(':') {
        Some((user, password)) => {
            user == app.config.git_user && password == app.config.git_password
        }
        None => false,
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"git\"")],
        "需要认证\n",
    )
        .into_response()
}

fn content_type(service: Service, advertisement: bool) -> String {
    let suffix = if advertisement {
        "advertisement"
    } else {
        "result"
    };
    format!("application/x-{}-{suffix}", service.name())
}

async fn info_refs(
    State(app): State<Arc<App>>,
    Path(repo): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&app, &headers) {
        return unauthorized();
    }
    let Some(repo) = repo_name(&repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let service = match query.get("service").map(String::as_str) {
        Some("git-upload-pack") => Service::UploadPack,
        Some("git-receive-pack") => Service::ReceivePack,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };

    let store = app.store.clone();
    let repo_for_worker = repo.clone();
    let advertisement =
        tokio::task::spawn_blocking(move || advertise::refs(&store, &repo_for_worker, service))
            .await
            .map_err(|e| format!("任务失败: {e}"))
            .and_then(|result| result);
    let advertisement = match advertisement {
        Ok(advertisement) => advertisement,
        Err(error) => {
            eprintln!("生成引用通告失败: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut body = Vec::new();
    if let Err(error) = write_pkt(
        &mut body,
        format!("# service={}\n", service.name()).as_bytes(),
    )
    .and_then(|_| write_flush(&mut body))
    {
        eprintln!("写通告前缀失败: {error}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    body.extend_from_slice(&advertisement);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type(service, true))
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn upload_pack(
    State(app): State<Arc<App>>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&app, &headers) {
        return unauthorized();
    }
    let Some(repo) = repo_name(&repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let store = app.store.clone();
    let max = app.config.max_push_bytes;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let result = fetch::upload(&store, &repo, &mut body.as_ref(), &mut out, max);
        (out, result)
    })
    .await;

    match outcome {
        Ok((out, Ok(()))) => Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                content_type(Service::UploadPack, false),
            )
            .body(Body::from(out))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok((_, Err(error))) => {
            eprintln!("拉取处理失败: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("{error}\n")).into_response()
        }
        Err(error) => {
            eprintln!("拉取任务失败: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn receive_pack(
    State(app): State<Arc<App>>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&app, &headers) {
        return unauthorized();
    }
    let Some(repo) = repo_name(&repo) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let store = app.store.clone();
    let max = app.config.max_push_bytes;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let result = push::receive(&store, &repo, &mut body.as_ref(), &mut out, max);
        (out, result)
    })
    .await;

    match outcome {
        Ok((out, Ok(()))) => Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                content_type(Service::ReceivePack, false),
            )
            .body(Body::from(out))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok((_, Err(error))) => {
            eprintln!("推送处理失败: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("{error}\n")).into_response()
        }
        Err(error) => {
            eprintln!("推送任务失败: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
