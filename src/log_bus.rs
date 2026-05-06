// log_bus.rs：日志环形缓冲 + WebSocket 广播（Web 版）。
//
// 从 Tauri 版改造：
//   - 保留环形缓冲（Mutex<VecDeque>，最大 2000 条）
//   - 新增 tokio::sync::broadcast channel（容量 512）用于 WebSocket 实时推送
//   - tracing Layer 实现：同时写入 ring buffer + broadcast
//
// v4 顺序保证：
//   每条日志带一个全局单调递增 `seq: u64`（AtomicU64::fetch_add）。
//   背景：subprocess 的 stdout / stderr 在两个并发 tokio task 中各自 tracing，
//   `ts` 毫秒精度对同一毫秒内的多条日志无法区分先后；前端 React Query 整体替换
//   日志数组时同毫秒的相对顺序受 ECMAScript Array sort stability 与 key 选择
//   影响，会出现"闪烁 / 跳序"。
//   引入 seq 后：后端 push 即 fetch_add，全局严格有序；前端按 seq 排序 + 用 seq
//   做 React key，行身份稳定。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;
use tracing_subscriber::Layer;

const RING_CAPACITY: usize = 2000;
const BROADCAST_CAPACITY: usize = 512;

/// 单条日志记录。
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    /// 全局单调递增序号（AtomicU64 分配）。
    /// 前端按此字段排序，避免毫秒精度 ts 对同毫秒多条日志排序不稳定。
    pub seq: u64,
    pub ts: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

/// 全局日志总线。
pub struct LogBus {
    ring: Mutex<VecDeque<LogEntry>>,
    tx: broadcast::Sender<LogEntry>,
    next_seq: AtomicU64,
}

impl LogBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            ring: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            tx,
            next_seq: AtomicU64::new(1),
        }
    }

    /// 推送一条日志到 ring buffer + broadcast。
    /// seq 在这里分配（AtomicU64 单调递增），保证全局严格有序。
    fn push(&self, ts: i64, level: String, target: String, message: String) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let entry = LogEntry {
            seq,
            ts,
            level,
            target,
            message,
        };
        {
            let mut ring = self.ring.lock().unwrap();
            if ring.len() >= RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(entry.clone());
        }
        // broadcast 失败（无订阅者）是正常的，忽略
        let _ = self.tx.send(entry);
    }

    /// 获取最近 N 条日志快照。
    /// 按 ring 内的入队顺序（即 seq 升序）返回。
    pub fn snapshot(&self, limit: usize) -> Vec<LogEntry> {
        let ring = self.ring.lock().unwrap();
        let skip = ring.len().saturating_sub(limit);
        ring.iter().skip(skip).cloned().collect()
    }

    /// 订阅 WebSocket 广播流。
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }
}

/// tracing Layer 实现：将日志写入 LogBus。
pub struct LogBusLayer;

impl<S> Layer<S> for LogBusLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // 使用全局 LOG_BUS
        let metadata = event.metadata();
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        // 安全：LOG_BUS 在 main 中初始化，始终可用
        if let Some(bus) = LOG_BUS.get() {
            bus.push(
                chrono::Utc::now().timestamp_millis(),
                metadata.level().to_string(),
                metadata.target().to_string(),
                visitor.0,
            );
        }
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
            // 去掉 Debug 输出的外层引号
            if self.0.starts_with('"') && self.0.ends_with('"') {
                self.0 = self.0[1..self.0.len() - 1].to_string();
            }
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

/// 全局 LogBus 单例。
static LOG_BUS: std::sync::OnceLock<LogBus> = std::sync::OnceLock::new();

/// 初始化全局 LogBus（main 中调用一次）。
pub fn init() {
    LOG_BUS.get_or_init(LogBus::new);
}

/// 获取全局 LogBus 引用。
pub fn bus() -> &'static LogBus {
    LOG_BUS.get().expect("LogBus not initialized")
}

/// 获取最近 N 条日志快照。
pub fn snapshot(limit: usize) -> Vec<LogEntry> {
    bus().snapshot(limit)
}

/// 订阅 WebSocket 广播。
pub fn subscribe() -> broadcast::Receiver<LogEntry> {
    bus().subscribe()
}
