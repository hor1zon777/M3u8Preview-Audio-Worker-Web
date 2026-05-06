// handler/control.rs：暂停 / 恢复轮询。

use axum::extract::State;
use axum::Json;

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
