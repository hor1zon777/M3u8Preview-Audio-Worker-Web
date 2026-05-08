// config.rs：Settings schema + JSON 文件持久化（Web 版）。
//
// 从 Tauri 版改造：tauri-plugin-store → 直接读写 JSON 文件。
// 路径由 CLI --config 参数指定，默认 /etc/audio-worker/settings.json。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 所有用户可配置项。Default 值用于首次启动 / 字段缺失时的兜底。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
    pub pipeline: AudioPipelineSettings,
    #[serde(default)]
    pub network: NetworkSettings,
    #[serde(default)]
    pub ui: UiSettings,

    /// 首次启动生成的 UUID v4，重启复用
    #[serde(default)]
    pub worker_id: String,

    /// 用户给这台 worker 起的名字（默认主机名）
    #[serde(default)]
    pub worker_name: String,

    /// Web 管理面板鉴权 token。空 = 不需要鉴权。
    ///
    /// 设置后所有 /api/* 请求需要 `Authorization: Bearer <token>` 头。
    /// WebSocket 连接通过 `?token=<token>` 查询参数传递。
    #[serde(default)]
    pub web_auth_token: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server: ServerSettings::default(),
            pipeline: AudioPipelineSettings::default(),
            network: NetworkSettings::default(),
            ui: UiSettings::default(),
            worker_id: String::new(),
            worker_name: detect_default_worker_name(),
            web_auth_token: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSettings {
    pub base_url: String,
    pub token: String,
    pub poll_interval_sec: u64,
    pub heartbeat_interval_sec: u64,
    pub error_backoff_sec: u64,
    pub verify_tls: bool,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_tasks: u32,
}

fn default_max_concurrent() -> u32 {
    1
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            token: String::new(),
            poll_interval_sec: 5,
            heartbeat_interval_sec: 30,
            error_backoff_sec: 5,
            verify_tls: true,
            max_concurrent_tasks: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPipelineSettings {
    #[serde(default)]
    pub m3u8dl_path: String,
    #[serde(default)]
    pub ffmpeg_path: String,
    #[serde(default)]
    pub temp_dir: String,
    #[serde(default)]
    pub audio_storage_dir: String,
    #[serde(default)]
    pub intermediate_audio_format: AudioFormat,
    #[serde(default = "default_flac_compression_level")]
    pub flac_compression_level: u8,
    #[serde(default = "default_flac_timeout_sec")]
    pub flac_timeout_sec: u64,
    #[serde(default = "default_audio_local_max_pending")]
    pub audio_local_max_pending: u32,
    /// 编码后实际 duration 与 m3u8 预期 duration 的容差比例（0~1）。
    ///
    /// 比如 0.10 表示允许 10% 偏差；偏差超过容差时拒绝写入缓存，避免 lenient 模式
    /// 产出的 0.1MB 残品（只解出几秒钟音频）被注册为合法产物。
    /// 当 m3u8 解析失败导致拿不到预期 duration 时跳过此校验。
    #[serde(default = "default_audio_duration_tolerance")]
    pub audio_duration_tolerance: f64,
    /// 启动清理时的最小合法 duration（秒）。
    ///
    /// 启动期没有 m3u8 上下文，无法做相对比对，只能用绝对阈值兜底。
    /// 默认 60s：3-6 秒残品 / 几十秒短片都会被清；真正业务任务通常 ≥ 几分钟。
    #[serde(default = "default_audio_min_duration_sec")]
    pub audio_min_duration_sec: u64,
}

impl Default for AudioPipelineSettings {
    fn default() -> Self {
        Self {
            m3u8dl_path: String::new(),
            ffmpeg_path: String::new(),
            temp_dir: String::new(),
            audio_storage_dir: String::new(),
            intermediate_audio_format: AudioFormat::default(),
            flac_compression_level: default_flac_compression_level(),
            flac_timeout_sec: default_flac_timeout_sec(),
            audio_local_max_pending: default_audio_local_max_pending(),
            audio_duration_tolerance: default_audio_duration_tolerance(),
            audio_min_duration_sec: default_audio_min_duration_sec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    #[default]
    Flac,
    OpusLow,
    Wav,
}

impl AudioFormat {
    pub fn as_protocol_str(&self) -> &'static str {
        match self {
            AudioFormat::Flac => "flac",
            AudioFormat::OpusLow => "opus_24k",
            AudioFormat::Wav => "wav",
        }
    }
}

fn default_flac_compression_level() -> u8 {
    8
}
fn default_flac_timeout_sec() -> u64 {
    600
}
fn default_audio_local_max_pending() -> u32 {
    5
}
fn default_audio_duration_tolerance() -> f64 {
    0.10
}
fn default_audio_min_duration_sec() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSettings {
    #[serde(default)]
    pub github_proxy_enabled: bool,
    #[serde(default)]
    pub github_proxy_url: String,
    #[serde(default)]
    pub download_proxy: String,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            github_proxy_enabled: false,
            github_proxy_url: String::new(),
            download_proxy: String::new(),
        }
    }
}

impl NetworkSettings {
    pub fn apply_github_proxy(&self, url: &str) -> String {
        if !self.github_proxy_enabled {
            return url.to_string();
        }
        let proxy = self.github_proxy_url.trim().trim_end_matches('/');
        if proxy.is_empty() {
            return url.to_string();
        }
        if is_github_url(url) {
            format!("{}/{}", proxy, url)
        } else {
            url.to_string()
        }
    }
}

fn is_github_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("https://raw.githubusercontent.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
        || url.starts_with("http://github.com/")
        || url.starts_with("http://raw.githubusercontent.com/")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    pub minimize_to_tray: bool,
    pub autostart: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray: false,
            autostart: false,
        }
    }
}

/// 从 JSON 文件加载 settings；不存在 / 解析失败时返回 Default。
pub fn load(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("parse settings failed: {e}, falling back to default");
                Settings::default()
            }
        },
        Err(e) => {
            tracing::info!("settings file not found or unreadable ({e}), using default");
            Settings::default()
        }
    }
}

/// 保存 settings 到 JSON 文件。父目录不存在时自动创建。
pub fn save(path: &Path, settings: &Settings) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, json)?;
    Ok(())
}

fn detect_default_worker_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "audio-worker".to_string())
}
