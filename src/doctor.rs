// doctor.rs：工具可用性探测（Web 版简化）。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ToolInfo {
    pub key: String,
    pub label: String,
    pub path: String,
    pub found: bool,
    pub version: String,
    pub backend: String,
    pub error: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub m3u8dl: ToolInfo,
    pub ffmpeg: ToolInfo,
}

/// 探测 N_m3u8DL-RE 和 ffmpeg 的可用性。
pub async fn run() -> DoctorReport {
    DoctorReport {
        m3u8dl: probe_tool("m3u8dl", "N_m3u8DL-RE", &["N_m3u8DL-RE"]).await,
        ffmpeg: probe_tool("ffmpeg", "ffmpeg", &["ffmpeg"]).await,
    }
}

async fn probe_tool(key: &str, label: &str, names: &[&str]) -> ToolInfo {
    let mut info = ToolInfo {
        key: key.to_string(),
        label: label.to_string(),
        path: String::new(),
        found: false,
        version: String::new(),
        backend: String::new(),
        error: String::new(),
        hint: None,
    };

    // 尝试 which 查找
    for name in names {
        if let Some(p) = which(name) {
            info.path = p.display().to_string();
            info.found = true;

            // 获取版本
            if let Ok(output) = tokio::process::Command::new(&p)
                .arg("--version")
                .output()
                .await
            {
                let ver = String::from_utf8_lossy(&output.stdout);
                let ver = ver.trim();
                if !ver.is_empty() {
                    info.version = ver.lines().next().unwrap_or("").to_string();
                }
            }
            return info;
        }
    }

    info.error = format!("{label} not found in PATH");
    info.hint = Some(format!(
        "Install {label} and ensure it's in PATH, or configure the path in Settings."
    ));
    info
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
