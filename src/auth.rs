/**
 * [INPUT]: 依赖 axum 的 HeaderMap 与 Response，依赖 base64 解码 Basic 凭据，依赖 Config 的固定账号
 * [OUTPUT]: 导出 authorized 与 unauthorized；HTTP 数据面与管理面共用同一组账号
 * [POS]: HTTP 认证的唯一实现；不解析路由，不接触 Git 语义
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;

use crate::config::Config;

pub fn authorized(config: &Config, headers: &HeaderMap) -> bool {
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
        Some((user, password)) => user == config.git_user && password == config.git_password,
        None => false,
    }
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"git\"")],
        "需要认证\n",
    )
        .into_response()
}
