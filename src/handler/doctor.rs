// handler/doctor.rs：工具可用性探测。

use axum::Json;

use crate::doctor;
use crate::handler::settings::ApiResponse;

/// GET /api/doctor
pub async fn probe() -> Json<ApiResponse<doctor::DoctorReport>> {
    let report = doctor::run().await;
    Json(ApiResponse::ok(report))
}
