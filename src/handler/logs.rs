// handler/logs.rs：日志查询 + WebSocket 实时推送。

use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::handler::settings::ApiResponse;
use crate::log_bus::{self, LogEntry};

#[derive(Deserialize)]
pub struct LogParams {
    limit: Option<usize>,
}

/// GET /api/logs
pub async fn get_recent(
    Query(params): Query<LogParams>,
) -> Json<ApiResponse<Vec<LogEntry>>> {
    let entries = log_bus::snapshot(params.limit.unwrap_or(200));
    Json(ApiResponse::ok(entries))
}

/// GET /api/ws/logs — WebSocket 实时日志流。
pub async fn ws_stream(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_ws)
}

async fn handle_ws(socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures_util::SinkExt;
    use futures_util::StreamExt;

    let mut rx = log_bus::subscribe();
    let (mut sender, mut receiver) = socket.split();

    // 并行处理：发送日志 + 接收客户端消息（用于检测断开）
    loop {
        tokio::select! {
            // 从 broadcast channel 接收日志并发送到 WebSocket
            result = rx.recv() => {
                match result {
                    Ok(entry) => {
                        let json = serde_json::to_string(&entry).unwrap_or_default();
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("[ws/logs] lagged, skipped {} messages", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 客户端消息（忽略内容，只检测断开）
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // 忽略 ping/pong/text
                }
            }
        }
    }
}
