/**
 * [INPUT]: 依赖 axum 的路由、提取器与 JSON 响应；依赖 auth 的 Basic 认证；依赖 Store 的仓库生命周期方法
 * [OUTPUT]: 导出 routes()，提供 /api/repos 的列表、创建、详情、修改与删除接口
 * [POS]: 管理面 HTTP 层；与 http.rs 的 Git 数据面共用监听、固定账号与 App 状态，不承载 Git 协议语义
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth;
use crate::http::App;
use crate::store::{validate_repo_name, RepoInfo, RepoUpdateError, Store};

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/repos", get(list_repos).post(create_repo))
        .route(
            "/api/repos/{repo}",
            get(get_repo).patch(update_repo).delete(delete_repo),
        )
}

#[derive(Serialize)]
struct RepoSummary {
    name: String,
    created_at: String,
}

impl From<RepoInfo> for RepoSummary {
    fn from(info: RepoInfo) -> RepoSummary {
        RepoSummary {
            name: info.name,
            created_at: info.created_at,
        }
    }
}

#[derive(Serialize)]
struct RefEntry {
    name: String,
    oid: String,
}

#[derive(Serialize)]
struct RepoDetail {
    name: String,
    created_at: String,
    refs: Vec<RefEntry>,
}

#[derive(Deserialize)]
struct CreateRepoRequest {
    name: String,
}

#[derive(Deserialize)]
struct UpdateRepoRequest {
    new_name: String,
}

#[derive(Deserialize)]
struct DeleteRepoRequest {
    confirm: String,
}

fn detail(store: &Store, repo: &str) -> Result<Option<RepoDetail>, String> {
    let Some(info) = store.get_repo(repo)? else {
        return Ok(None);
    };
    let refs = store
        .list_refs(&info.name)?
        .into_iter()
        .map(|(name, oid)| RefEntry { name, oid })
        .collect();
    Ok(Some(RepoDetail {
        name: info.name,
        created_at: info.created_at,
        refs,
    }))
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn ensure_auth(app: &App, headers: &HeaderMap) -> Option<Response> {
    if auth::authorized(&app.config, headers) {
        None
    } else {
        Some(auth::unauthorized())
    }
}

async fn list_repos(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(response) = ensure_auth(&app, &headers) {
        return response;
    }
    let store = app.store.clone();
    match tokio::task::spawn_blocking(move || store.list_repos()).await {
        Ok(Ok(repos)) => {
            let summaries: Vec<RepoSummary> = repos.into_iter().map(RepoSummary::from).collect();
            (StatusCode::OK, Json(summaries)).into_response()
        }
        Ok(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("任务失败: {join}"),
        ),
    }
}

async fn get_repo(
    State(app): State<Arc<App>>,
    Path(repo): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = ensure_auth(&app, &headers) {
        return response;
    }
    if let Err(message) = validate_repo_name(&repo) {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let store = app.store.clone();
    match tokio::task::spawn_blocking(move || detail(&store, &repo)).await {
        Ok(Ok(Some(repo))) => (StatusCode::OK, Json(repo)).into_response(),
        Ok(Ok(None)) => error(StatusCode::NOT_FOUND, "仓库不存在"),
        Ok(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("任务失败: {join}"),
        ),
    }
}

async fn create_repo(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(response) = ensure_auth(&app, &headers) {
        return response;
    }
    let Ok(request) = serde_json::from_slice::<CreateRepoRequest>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "请求体必须是 {\"name\": \"仓库名\"}",
        );
    };
    if let Err(message) = validate_repo_name(&request.name) {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let store = app.store.clone();
    let name = request.name;
    match tokio::task::spawn_blocking(move || {
        let created = store.create_repo(&name)?;
        if created {
            detail(&store, &name)?
                .ok_or_else(|| "仓库创建后立即读取失败".to_string())
                .map(Some)
        } else {
            Ok(None)
        }
    })
    .await
    {
        Ok(Ok(Some(repo))) => (StatusCode::CREATED, Json(repo)).into_response(),
        Ok(Ok(None)) => error(StatusCode::CONFLICT, "仓库已存在"),
        Ok(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("任务失败: {join}"),
        ),
    }
}

async fn update_repo(
    State(app): State<Arc<App>>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(response) = ensure_auth(&app, &headers) {
        return response;
    }
    if let Err(message) = validate_repo_name(&repo) {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let Ok(request) = serde_json::from_slice::<UpdateRepoRequest>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "请求体必须是 {\"new_name\": \"新仓库名\"}",
        );
    };

    let store = app.store.clone();
    let repo_for_update = repo.clone();
    let new_name = request.new_name.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        store.update_repo(&repo_for_update, Some(new_name.as_str()))
    })
    .await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(RepoUpdateError::NotFound)) => return error(StatusCode::NOT_FOUND, "仓库不存在"),
        Ok(Err(RepoUpdateError::Conflict)) => {
            return error(StatusCode::CONFLICT, "目标仓库名已存在")
        }
        Ok(Err(RepoUpdateError::InvalidName(message))) => {
            return error(StatusCode::BAD_REQUEST, message)
        }
        Ok(Err(RepoUpdateError::Database(message))) => {
            return error(StatusCode::INTERNAL_SERVER_ERROR, message)
        }
        Err(join) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("任务失败: {join}"),
            )
        }
    }

    let store = app.store.clone();
    let target = request.new_name;
    match tokio::task::spawn_blocking(move || detail(&store, &target)).await {
        Ok(Ok(Some(repo))) => (StatusCode::OK, Json(repo)).into_response(),
        Ok(Ok(None)) => error(StatusCode::NOT_FOUND, "仓库不存在"),
        Ok(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("任务失败: {join}"),
        ),
    }
}

async fn delete_repo(
    State(app): State<Arc<App>>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(response) = ensure_auth(&app, &headers) {
        return response;
    }
    if let Err(message) = validate_repo_name(&repo) {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let Ok(request) = serde_json::from_slice::<DeleteRepoRequest>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "请求体必须是 {\"confirm\": \"仓库名\"}",
        );
    };
    let name = repo.strip_suffix(".git").unwrap_or(repo.as_str());
    if request.confirm != name {
        return error(
            StatusCode::BAD_REQUEST,
            format!("确认名不匹配：请把 confirm 设为 {name}"),
        );
    }
    let store = app.store.clone();
    let repo_for_delete = repo.clone();
    match tokio::task::spawn_blocking(move || store.delete_repo(&repo_for_delete)).await {
        Ok(Ok(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(false)) => error(StatusCode::NOT_FOUND, "仓库不存在"),
        Ok(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("任务失败: {join}"),
        ),
    }
}
