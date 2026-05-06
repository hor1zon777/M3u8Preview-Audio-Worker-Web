// main.rs：audio worker Web 版入口。
//
// 启动流程：
//   1. 解析 CLI 参数（--config, --port）
//   2. 初始化 tracing + log_bus
//   3. 构建 Axum Router + AppState
//   4. 初始化 SQLite 历史
//   5. 启动 poller + audio_owner 后台 task
//   6. 启动 Axum HTTP server

mod api;
mod app;
mod config;
mod doctor;
mod handler;
mod history;
mod log_bus;
mod pipeline;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::api::ApiClient;
use crate::pipeline::audio_owner;
use crate::state::AppState;

#[derive(Parser)]
#[command(name = "audio-worker", about = "M3u8Preview Audio Worker (Web)")]
struct Cli {
    /// 配置文件路径
    #[arg(long, default_value = "/etc/audio-worker/settings.json")]
    config: PathBuf,

    /// HTTP 监听端口
    #[arg(long, default_value_t = 3900)]
    port: u16,
}

#[tokio::main]
async fn main() {
    // 1. 初始化日志
    init_tracing();
    log_bus::init();

    // 2. 解析 CLI
    let cli = Cli::parse();
    tracing::info!("audio-worker-web starting, config={:?}, port={}", cli.config, cli.port);

    // 3. 构建 Router + State
    let (router, state) = app::build(cli.config, cli.port);

    // 4. 初始化 SQLite 历史
    if let Err(e) = history::init(&state.app_data_dir) {
        tracing::warn!("history DB init failed: {e}; history features will be unavailable");
    }

    // 5. 启动 poller 后台 task
    let poller_state = state.clone();
    tokio::spawn(async move {
        pipeline::poller::run(poller_state).await;
    });

    // 6. 启动 audio_owner fetch_loop 后台 task
    let state_for_resolver = state.clone();
    let state_for_loop = state.clone();
    let cancel_fetch = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(async move {
        audio_owner::fetch_loop(
            move || audio_owner::resolve_storage_dir(&state_for_resolver),
            move || build_client_and_worker_id(&state_for_loop),
            cancel_fetch,
        )
        .await;
    });

    // 7. 启动 Axum server
    let addr = format!("0.0.0.0:{}", cli.port);
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, router)
        .await
        .expect("server error");
}

/// 从 state 构造 ApiClient + 当前 worker_id。
/// 配置不全时返回 None（fetch_loop 会等下一轮重试）。
fn build_client_and_worker_id(state: &Arc<AppState>) -> Option<(Arc<ApiClient>, String)> {
    let s = state.settings.read().ok()?;
    let base = s.server.base_url.trim().to_string();
    let token = s.server.token.trim().to_string();
    let verify_tls = s.server.verify_tls;
    let worker_id = s.worker_id.trim().to_string();
    let proxy = s.network.download_proxy.clone();
    drop(s);
    if base.is_empty() || token.is_empty() || worker_id.is_empty() {
        return None;
    }
    match ApiClient::new_with_proxy(&base, &token, verify_tls, &proxy) {
        Ok(c) => Some((Arc::new(c), worker_id)),
        Err(_) => None,
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,audio_worker_web=debug"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_target(false)
                .with_ansi(true),
        )
        .with(log_bus::LogBusLayer)
        .init();
}
