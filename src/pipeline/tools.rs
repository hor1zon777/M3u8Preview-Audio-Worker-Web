// pipeline/tools.rs：外部二进制（N_m3u8DL-RE / ffmpeg）路径解析（Web 版）。
//
// 解析顺序（首个存在的胜出）：
//   1. settings.pipeline.{m3u8dl_path, ffmpeg_path}（用户显式配置）
//   2. app_data_dir/binaries/（Docker 挂载或本地部署）
//   3. PATH 中查找（Docker 容器中 ffmpeg / N_m3u8DL-RE 通常在 /usr/local/bin/）

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{tool} not found (configured path: {configured:?})")]
    NotFound {
        tool: &'static str,
        configured: Option<String>,
    },
}

/// 已解析的工具路径集合。pipeline 各阶段直接持引用。
#[derive(Debug, Clone)]
pub struct Tools {
    pub m3u8dl: PathBuf,
    pub ffmpeg: PathBuf,
}

impl Tools {
    pub fn resolve(
        m3u8dl_setting: &str,
        ffmpeg_setting: &str,
        _resource_dir: Option<&PathBuf>,
        app_data_dir: Option<&PathBuf>,
    ) -> Result<Self, ToolError> {
        let m3u8dl = resolve_one(
            "N_m3u8DL-RE",
            m3u8dl_setting,
            app_data_dir,
            &["N_m3u8DL-RE"],
        )?;
        let ffmpeg = resolve_one(
            "ffmpeg",
            ffmpeg_setting,
            app_data_dir,
            &["ffmpeg"],
        )?;
        Ok(Self { m3u8dl, ffmpeg })
    }
}

fn resolve_one(
    tool: &'static str,
    setting: &str,
    app_data_dir: Option<&PathBuf>,
    bin_names: &[&str],
) -> Result<PathBuf, ToolError> {
    // 1. 用户显式配置
    let setting = setting.trim();
    if !setting.is_empty() {
        let p = PathBuf::from(setting);
        if p.is_file() {
            return Ok(p);
        }
        return Err(ToolError::NotFound {
            tool,
            configured: Some(setting.to_string()),
        });
    }

    // 2. app_data_dir/binaries/
    if let Some(dir) = app_data_dir {
        for name in bin_names {
            let p = dir.join("binaries").join(name);
            if p.is_file() {
                return Ok(p);
            }
        }
    }

    // 3. PATH 查找
    for name in bin_names {
        if let Some(p) = which(name) {
            return Ok(p);
        }
    }

    Err(ToolError::NotFound {
        tool,
        configured: None,
    })
}

/// 极简 PATH 查找（不依赖第三方 crate）。
fn which(name: &str) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
