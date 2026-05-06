// handler/history.rs：任务历史 CRUD。

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::handler::settings::ApiResponse;
use crate::history::{self, TaskHistoryRow, TaskHistorySummary};
use crate::state::SharedState;

#[derive(Deserialize)]
pub struct ListParams {
    limit: Option<u32>,
    offset: Option<u32>,
}

/// GET /api/history
pub async fn list(
    Query(params): Query<ListParams>,
) -> Json<ApiResponse<Vec<TaskHistorySummary>>> {
    match history::list(params.limit.unwrap_or(50), params.offset.unwrap_or(0)) {
        Ok(rows) => Json(ApiResponse::ok(rows)),
        Err(e) => Json(ApiResponse::err(format!("{e}"))),
    }
}

/// GET /api/history/:jobId
pub async fn get_detail(
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Json<ApiResponse<Option<TaskHistoryRow>>> {
    match history::get(&job_id) {
        Ok(row) => Json(ApiResponse::ok(row)),
        Err(e) => Json(ApiResponse::err(format!("{e}"))),
    }
}

#[derive(Deserialize)]
pub struct ClearParams {
    keep_recent: Option<u32>,
}

/// DELETE /api/history
pub async fn clear(
    Query(params): Query<ClearParams>,
) -> Json<ApiResponse<usize>> {
    match history::clear(params.keep_recent.unwrap_or(0)) {
        Ok(n) => Json(ApiResponse::ok(n)),
        Err(e) => Json(ApiResponse::err(format!("{e}"))),
    }
}
