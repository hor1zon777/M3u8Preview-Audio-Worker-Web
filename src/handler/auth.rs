// handler/auth.rs：Web 面板 Bearer token 鉴权中间件。
//
// 逻辑：
//   - 从 state.settings 读取 web_auth_token
//   - 空 token → 鉴权关闭，所有请求放行
//   - 非空 → 校验 Authorization: Bearer <token>
//   - WebSocket 通过 ?token=<token> 查询参数传递（浏览器无法自定义 WS 握手头）

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::handler::settings::ApiResponse;
use crate::state::SharedState;

/// 鉴权中间件：拦截所有 /api/* 请求。
pub async fn auth_middleware(
    State(state): State<SharedState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let expected = {
        let s = state.settings.read().unwrap();
        s.web_auth_token.clone()
    };

    // token 为空 → 鉴权关闭
    if expected.is_empty() {
        return next.run(request).await;
    }

    // 检查路径：WebSocket 端点用 query param，其它用 Authorization header
    let path = request.uri().path().to_string();
    let authorized = if path == "/api/ws/logs" {
        // WebSocket：从 query param ?token=xxx 读取
        let query = request.uri().query().unwrap_or("");
        extract_query_token(query) == Some(expected.as_str())
    } else {
        // 普通 HTTP：从 Authorization: Bearer xxx 读取
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            == Some(expected.as_str())
    };

    if authorized {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// 从 URL query string 中提取 token 参数。
fn extract_query_token(query: &str) -> Option<&str> {
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == "token" {
                return Some(value);
            }
        }
    }
    None
}

/// GET /api/auth/check — 公开端点，前端用它判断是否需要登录。
///
/// 响应：
///   - { required: false } → 不需要鉴权
///   - { required: true, has_token: true } → 需要鉴权，且已配置 token
///   - { required: true, has_token: false } → 需要鉴权，但未配置（异常状态）
#[derive(Serialize)]
pub struct AuthCheckResult {
    pub required: bool,
    pub has_token: bool,
}

pub async fn auth_check(State(state): State<SharedState>) -> Json<ApiResponse<AuthCheckResult>> {
    let token = state.settings.read().unwrap().web_auth_token.clone();
    Json(ApiResponse::ok(AuthCheckResult {
        required: !token.is_empty(),
        has_token: !token.is_empty(),
    }))
}
