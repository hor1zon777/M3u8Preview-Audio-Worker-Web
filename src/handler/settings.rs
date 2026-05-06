// handler/settings.rs：配置读写 + 目录校验。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::state::SharedState;

/// API 响应包装。
#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub data: Option<T>,
    pub message: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            message: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            message: Some(msg.into()),
        }
    }
}

/// GET /api/settings
pub async fn get(
    State(state): State<SharedState>,
) -> Json<ApiResponse<config::Settings>> {
    let mut s = state.settings.read().unwrap().clone();
    // 首次启动：注入 worker_id
    if s.worker_id.is_empty() {
        s.worker_id = uuid::Uuid::new_v4().to_string();
        if let Err(e) = config::save(&state.config_path, &s) {
            tracing::warn!("save initial worker_id failed: {e}");
        }
        *state.settings.write().unwrap() = s.clone();
        *state.worker_id.write().unwrap() = s.worker_id.clone();
    }
    Json(ApiResponse::ok(s))
}

/// PUT /api/settings
pub async fn save(
    State(state): State<SharedState>,
    Json(settings): Json<config::Settings>,
) -> Json<ApiResponse<()>> {
    let (need_re_register, max_concurrent_changed, prev_max_concurrent) = {
        let prev = state.settings.read().unwrap();
        let connection_changed = prev.server.base_url != settings.server.base_url
            || prev.server.token != settings.server.token
            || prev.worker_id != settings.worker_id
            || prev.worker_name != settings.worker_name;
        let max_changed =
            prev.server.max_concurrent_tasks != settings.server.max_concurrent_tasks;
        (
            connection_changed || max_changed,
            max_changed,
            prev.server.max_concurrent_tasks,
        )
    };

    if let Err(e) = config::save(&state.config_path, &settings) {
        return Json(ApiResponse::err(format!("保存失败: {e}")));
    }
    *state.worker_id.write().unwrap() = settings.worker_id.clone();
    let new_max_concurrent = settings.server.max_concurrent_tasks;
    *state.settings.write().unwrap() = settings;

    if need_re_register {
        state.set_registered(false);
    }

    if max_concurrent_changed && new_max_concurrent > prev_max_concurrent {
        state.set_max_concurrent(new_max_concurrent);
    }

    Json(ApiResponse::ok(()))
}

/// POST /api/validate-dir
#[derive(Deserialize)]
pub struct ValidateDirRequest {
    pub path: String,
}

#[derive(Serialize)]
pub struct ValidateDirResult {
    pub ok: bool,
    pub message: String,
    pub resolved_path: String,
}

pub async fn validate_dir(
    Json(req): Json<ValidateDirRequest>,
) -> Json<ApiResponse<ValidateDirResult>> {
    let trimmed = req.path.trim();
    if trimmed.is_empty() {
        let sys = std::env::temp_dir();
        return Json(ApiResponse::ok(ValidateDirResult {
            ok: true,
            message: "未指定，将使用系统 Temp".to_string(),
            resolved_path: sys.display().to_string(),
        }));
    }
    let p = std::path::Path::new(trimmed);
    if !p.exists() {
        return Json(ApiResponse::ok(ValidateDirResult {
            ok: false,
            message: format!("目录不存在: {}", trimmed),
            resolved_path: trimmed.to_string(),
        }));
    }
    if !p.is_dir() {
        return Json(ApiResponse::ok(ValidateDirResult {
            ok: false,
            message: format!("路径不是目录: {}", trimmed),
            resolved_path: trimmed.to_string(),
        }));
    }
    // 写入探测
    let probe = p.join(format!(".probe-{}", uuid::Uuid::new_v4()));
    match std::fs::write(&probe, b"probe") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Json(ApiResponse::ok(ValidateDirResult {
                ok: true,
                message: format!("可用: {}", trimmed),
                resolved_path: trimmed.to_string(),
            }))
        }
        Err(e) => Json(ApiResponse::ok(ValidateDirResult {
            ok: false,
            message: format!("不可写: {}", e),
            resolved_path: trimmed.to_string(),
        })),
    }
}
