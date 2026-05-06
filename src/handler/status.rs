// handler/status.rs：运行时状态、连接测试、注册、重试。

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::api::{ApiClient, RegisterResponse};
use crate::handler::settings::ApiResponse;
use crate::pipeline::audio_owner;
use crate::state::SharedState;

#[derive(Serialize)]
pub struct RuntimeStatus {
    pub registered: bool,
    pub polling_paused: bool,
    pub stale_threshold_sec: u64,
    pub uptime_sec: u64,
    pub current_tasks: Vec<crate::state::CurrentTask>,
    pub max_concurrent_tasks: u32,
    pub server_max_concurrent_tasks: u32,
    pub stats: crate::state::RuntimeStats,
    pub capabilities: Vec<String>,
    pub audio_local_pending: u32,
    pub audio_local_max_pending: u32,
    pub audio_storage_dir: String,
}

/// GET /api/status
pub async fn runtime_status(
    State(state): State<SharedState>,
) -> Json<ApiResponse<RuntimeStatus>> {
    let tasks = state.snapshot_tasks();
    let stats = state.stats.read().unwrap().clone();
    let uptime = state.started_at.elapsed().as_secs();
    let max_pending = state
        .settings
        .read()
        .map(|s| s.pipeline.audio_local_max_pending)
        .unwrap_or(0);

    let (audio_local_pending, audio_storage_dir) =
        match audio_owner::resolve_storage_dir(&state) {
            Ok(dir) => {
                let n = audio_owner::count_pending_entries(&dir) as u32;
                (n, dir.display().to_string())
            }
            Err(e) => {
                tracing::warn!("[runtime-status] resolve audio_storage_dir failed: {e:#}");
                (0, String::new())
            }
        };

    Json(ApiResponse::ok(RuntimeStatus {
        registered: state.registered.load(Ordering::Acquire),
        polling_paused: state.is_polling_paused(),
        stale_threshold_sec: *state.stale_threshold_sec.read().unwrap(),
        uptime_sec: uptime,
        current_tasks: tasks,
        max_concurrent_tasks: state.max_concurrent(),
        server_max_concurrent_tasks: state.server_max_concurrent(),
        stats,
        capabilities: vec!["audio_extract".to_string()],
        audio_local_pending,
        audio_local_max_pending: max_pending,
        audio_storage_dir,
    }))
}

#[derive(Serialize)]
pub struct PingResult {
    pub ok: bool,
    pub message: String,
}

/// POST /api/ping
pub async fn test_connection(
    State(state): State<SharedState>,
) -> Json<ApiResponse<PingResult>> {
    let (base, token, verify_tls, proxy) = {
        let s = state.settings.read().unwrap();
        (
            s.server.base_url.clone(),
            s.server.token.clone(),
            s.server.verify_tls,
            s.network.download_proxy.clone(),
        )
    };
    let client = match ApiClient::new_with_proxy(&base, &token, verify_tls, &proxy) {
        Ok(c) => c,
        Err(e) => {
            return Json(ApiResponse::ok(PingResult {
                ok: false,
                message: format!("配置不完整: {e}"),
            }))
        }
    };
    match client.ping().await {
        Ok(()) => Json(ApiResponse::ok(PingResult {
            ok: true,
            message: "服务器可达".to_string(),
        })),
        Err(e) => Json(ApiResponse::ok(PingResult {
            ok: false,
            message: format!("无法连接: {e}"),
        })),
    }
}

/// POST /api/register
pub async fn register_worker(
    State(state): State<SharedState>,
) -> Json<ApiResponse<RegisterResponse>> {
    let (base, token, verify_tls, worker_id, name, proxy) = {
        let s = state.settings.read().unwrap();
        (
            s.server.base_url.clone(),
            s.server.token.clone(),
            s.server.verify_tls,
            s.worker_id.clone(),
            s.worker_name.clone(),
            s.network.download_proxy.clone(),
        )
    };
    let client = match ApiClient::new_with_proxy(&base, &token, verify_tls, &proxy) {
        Ok(c) => c,
        Err(e) => return Json(ApiResponse::err(format!("{e}"))),
    };
    match client
        .register(&worker_id, &name, env!("CARGO_PKG_VERSION"), "", &["audio_extract"])
        .await
    {
        Ok(resp) => {
            crate::pipeline::poller::apply_register_response(&state, &resp);
            Json(ApiResponse::ok(resp))
        }
        Err(e) => Json(ApiResponse::err(format!("{e}"))),
    }
}

/// POST /api/retry/:mediaId
pub async fn retry_job(
    State(state): State<SharedState>,
    axum::extract::Path(media_id): axum::extract::Path<String>,
) -> Json<ApiResponse<()>> {
    let (base, token, verify_tls, proxy) = {
        let s = state.settings.read().unwrap();
        (
            s.server.base_url.clone(),
            s.server.token.clone(),
            s.server.verify_tls,
            s.network.download_proxy.clone(),
        )
    };
    let client = match ApiClient::new_with_proxy(&base, &token, verify_tls, &proxy) {
        Ok(c) => c,
        Err(e) => return Json(ApiResponse::err(format!("{e}"))),
    };
    match client.retry(&media_id).await {
        Ok(()) => Json(ApiResponse::ok(())),
        Err(e) => Json(ApiResponse::err(format!("{e}"))),
    }
}
