// handler/control.rs：暂停 / 恢复轮询 + 任务取消（v4）。

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use crate::api::ApiClient;
use crate::handler::settings::ApiResponse;
use crate::state::SharedState;

/// POST /api/pause
pub async fn pause_polling(
    State(state): State<SharedState>,
) -> Json<ApiResponse<()>> {
    state.set_polling_paused(true);
    Json(ApiResponse::ok(()))
}

/// POST /api/resume
pub async fn resume_polling(
    State(state): State<SharedState>,
) -> Json<ApiResponse<()>> {
    state.set_polling_paused(false);
    Json(ApiResponse::ok(()))
}

#[derive(Serialize)]
pub struct CancelResult {
    /// 是否成功 abort 了正在跑的 spawned task。
    /// false = 任务已经结束 / 不存在。后续仍会发 fail 给服务端兜底。
    pub aborted: bool,
}

/// POST /api/tasks/{jobId}/cancel
///
/// 取消正在跑的任务并清理：
///   1. abort 对应 job_id 的 spawned pipeline task（在下一个 await 点中止）
///   2. 从 current_tasks 移除
///   3. 调服务端 fail_with_kind(worker_capacity) 让任务按 neutral 路径回 queued，
///      attempt 不增——相当于"我现在不想跑这条，让别的 worker 来"
pub async fn cancel_task(
    State(state): State<SharedState>,
    Path(job_id): Path<String>,
) -> Json<ApiResponse<CancelResult>> {
    if job_id.trim().is_empty() {
        return Json(ApiResponse::err("missing jobId"));
    }

    // 1) abort 正在跑的 pipeline（如果还在跑）
    let aborted = state.abort_running(&job_id);

    // 2) 从 current_tasks 移除（abort 后 spawn 的 task 不会再走 remove_task 路径）
    state.remove_task(&job_id);

    // 3) 通知服务端：用 worker_capacity（neutral）让 attempt 不增、回 queued
    let (base, token, verify_tls, worker_id) = {
        let s = state.settings.read().unwrap();
        let wid = state.worker_id.read().unwrap().clone();
        (
            s.server.base_url.clone(),
            s.server.token.clone(),
            s.server.verify_tls,
            wid,
        )
    };
    if !base.is_empty() && !token.is_empty() && !worker_id.is_empty() {
        if let Ok(c) = ApiClient::new(&base, &token, verify_tls) {
            if let Err(e) = c
                .fail_with_kind(
                    &job_id,
                    &worker_id,
                    "cancelled by user via UI",
                    "worker_capacity",
                )
                .await
            {
                tracing::info!(
                    "[control] cancel fail report rejected (server may have already cleaned up): {e}"
                );
            }
        }
    }

    tracing::info!("[control] cancelled task job={} aborted={}", job_id, aborted);
    Json(ApiResponse::ok(CancelResult { aborted }))
}
