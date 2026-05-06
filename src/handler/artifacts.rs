// handler/artifacts.rs：本地 FLAC artifact 管理（v4）。
//
// audio_storage_dir 下保存的是已 audio_ready、等待 subtitle worker 拉取的 FLAC。
// 长时间无 subtitle worker 接手时占盘，用户可以通过 UI 主动删除：
//
//   GET    /api/artifacts            列出当前所有暂存（按 created_at 倒序）
//   DELETE /api/artifacts/{jobId}    删除单条；可选 ?notify=true 走服务端 audio-lost
//                                    把任务回 queued + 清空音频元数据

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::handler::settings::ApiResponse;
use crate::pipeline::audio_owner;
use crate::state::SharedState;

/// 本地 FLAC artifact（"等待 subtitle worker 拉取"的 FLAC 列表项）。
#[derive(Debug, Clone, Serialize)]
pub struct LocalArtifact {
    pub job_id: String,
    pub media_id: String,
    pub size: i64,
    pub sha256: String,
    pub format: String,
    pub duration_ms: i64,
    pub flac_path: String,
    pub created_at_ms: i64,
}

/// GET /api/artifacts
///
/// 列出 audio_storage_dir 下所有合法的 FLAC artifact。返回按 created_at_ms 倒序。
/// 目录不存在 / 空 / 全部损坏时返回空数组（不报错）。
pub async fn list(State(state): State<SharedState>) -> Json<ApiResponse<Vec<LocalArtifact>>> {
    let dir = match audio_owner::resolve_storage_dir(&state) {
        Ok(p) => p,
        Err(e) => return Json(ApiResponse::err(format!("resolve storage dir: {e}"))),
    };
    let entries = match audio_owner::scan_entries(&dir) {
        Ok(v) => v,
        Err(e) => return Json(ApiResponse::err(format!("scan entries: {e}"))),
    };
    let mut out: Vec<LocalArtifact> = entries
        .into_iter()
        .map(|e| LocalArtifact {
            job_id: e.job_id,
            media_id: e.media_id,
            size: e.size,
            sha256: e.sha256,
            format: e.format,
            duration_ms: e.duration_ms,
            flac_path: e.flac_path.to_string_lossy().to_string(),
            created_at_ms: e.created_at_ms,
        })
        .collect();
    out.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
    Json(ApiResponse::ok(out))
}

#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    /// notify=true 时同步调服务端 audio-lost，让任务回 queued + 清空音频元数据，
    /// 避免 subtitle worker 后续 GET /audio 死等。默认 true。
    #[serde(default = "default_true")]
    pub notify: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
pub struct DeleteResult {
    /// 是否实际删除了本地文件。false = 索引或文件不存在（不算错误，幂等清理）。
    pub deleted: bool,
}

/// DELETE /api/artifacts/{jobId}?notify=true
///
/// 流程：
///   1. notify=true → 调 client.audio_lost；服务端拒绝（404/410/409）也忽略，仍清本地
///   2. audio_owner::remove_entry 删本地 .flac + .json
pub async fn delete(
    State(state): State<SharedState>,
    Path(job_id): Path<String>,
    Query(params): Query<DeleteParams>,
) -> Json<ApiResponse<DeleteResult>> {
    if job_id.trim().is_empty() {
        return Json(ApiResponse::err("missing jobId"));
    }
    let dir = match audio_owner::resolve_storage_dir(&state) {
        Ok(p) => p,
        Err(e) => return Json(ApiResponse::err(format!("resolve storage dir: {e}"))),
    };

    // 1) 通知服务端（best-effort）
    if params.notify {
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
            match ApiClient::new(&base, &token, verify_tls) {
                Ok(c) => {
                    if let Err(e) = c
                        .audio_lost(&job_id, &worker_id, "user deleted local artifact via UI")
                        .await
                    {
                        tracing::info!(
                            "[artifacts] audio_lost rejected (will still delete locally): {e}"
                        );
                    }
                }
                Err(e) => tracing::warn!("[artifacts] build api client failed: {e}"),
            }
        }
    }

    // 2) 删除本地文件
    let flac_path = dir.join(format!("{}.flac", job_id));
    let existed = flac_path.is_file();
    if let Err(e) = audio_owner::remove_entry(&dir, &job_id) {
        return Json(ApiResponse::err(format!("remove entry: {e}")));
    }
    if existed {
        tracing::info!("[artifacts] deleted local artifact job={}", job_id);
    }
    Json(ApiResponse::ok(DeleteResult { deleted: existed }))
}
