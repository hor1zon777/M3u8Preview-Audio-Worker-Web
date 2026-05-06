// app.rs：Axum Router 构建。

use std::path::PathBuf;
use std::sync::Arc;

use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::config;
use crate::handler;
use crate::handler::auth::auth_middleware;
use crate::state::{AppState, SharedState};

/// 构建 Axum Router + AppState。
pub fn build(config_path: PathBuf, port: u16) -> (Router, SharedState) {
    let mut settings = config::load(&config_path);

    // 首次启动：生成 worker_id
    if settings.worker_id.is_empty() {
        settings.worker_id = uuid::Uuid::new_v4().to_string();
        if let Err(e) = config::save(&config_path, &settings) {
            tracing::warn!("save initial worker_id failed: {e}");
        }
    }

    let app_data_dir = config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();

    let state: SharedState = Arc::new(AppState::new(config_path, app_data_dir, settings));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // 静态前端目录：Docker 中为 /srv/audio-worker/static，本地开发为 ./static
    let static_dir = std::env::var("STATIC_DIR")
        .unwrap_or_else(|_| "/srv/audio-worker/static".to_string());
    let static_dir = PathBuf::from(&static_dir);
    let fallback = if static_dir.join("index.html").exists() {
        ServeFile::new(static_dir.join("index.html"))
    } else {
        tracing::warn!("static dir not found at {:?}, frontend will not be available", static_dir);
        ServeFile::new(PathBuf::from("/dev/null"))
    };

    // 公开路由（不需要鉴权）
    let public_routes = Router::new()
        .route("/api/auth/check", get(handler::auth::auth_check));

    // 受保护路由（需要鉴权中间件）
    let protected_routes = Router::new()
        .route("/api/settings", get(handler::settings::get).put(handler::settings::save))
        .route("/api/validate-dir", post(handler::settings::validate_dir))
        .route("/api/status", get(handler::status::runtime_status))
        .route("/api/ping", post(handler::status::test_connection))
        .route("/api/register", post(handler::status::register_worker))
        .route("/api/retry/{mediaId}", post(handler::status::retry_job))
        .route("/api/pause", post(handler::control::pause_polling))
        .route("/api/resume", post(handler::control::resume_polling))
        // v4：主动取消正在跑的任务（abort + 服务端 worker_capacity 回 queued）
        .route("/api/tasks/{jobId}/cancel", post(handler::control::cancel_task))
        // v4：本地 FLAC artifact 列表 + 单条删除
        .route("/api/artifacts", get(handler::artifacts::list))
        .route("/api/artifacts/{jobId}", delete(handler::artifacts::delete))
        .route("/api/logs", get(handler::logs::get_recent))
        .route("/api/ws/logs", get(handler::logs::ws_stream))
        .route("/api/doctor", get(handler::doctor::probe))
        .route("/api/history", get(handler::history::list).delete(handler::history::clear))
        .route("/api/history/{jobId}", get(handler::history::get_detail));

    let app = Router::new()
        .merge(public_routes)
        // auth 中间件只应用于受保护路由，不影响 /api/auth/check
        .merge(protected_routes.layer(middleware::from_fn_with_state(state.clone(), auth_middleware)))
        // 静态前端（不受鉴权中间件保护）
        .fallback_service(ServeDir::new(static_dir).fallback(fallback))
        .layer(cors)
        .with_state(state.clone());

    tracing::info!("API routes registered, static dir configured");
    (app, state)
}
