// state.rs：进程级共享状态（Web 版）。
//
// 从 Tauri 版改造：去掉 AppHandle，新增 config_path 用于 JSON 持久化。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::Instant;

use crate::config::Settings;

/// 当前正在处理的任务（runtime 运行时状态，不持久化）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CurrentTask {
    pub job_id: String,
    pub media_id: String,
    pub media_title: Option<String>,
    pub stage: String,
    pub progress: u8,
    pub started_at_ms: i64,
}

/// 累计统计（进程内存，重启清零）。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RuntimeStats {
    pub completed: u64,
    pub failed: u64,
    pub last_error: Option<String>,
}

/// 应用全局状态。
pub struct AppState {
    /// 配置文件路径（save 时写回）
    pub config_path: PathBuf,

    /// audio_storage_dir 绝对路径（由 settings 解析，供 pipeline 使用）
    pub app_data_dir: PathBuf,

    pub settings: RwLock<Settings>,
    pub worker_id: RwLock<String>,
    pub registered: AtomicBool,
    pub current_tasks: Mutex<HashMap<String, CurrentTask>>,
    pub stats: RwLock<RuntimeStats>,
    pub stale_threshold_sec: RwLock<u64>,
    pub max_concurrent_tasks: AtomicU32,
    pub server_max_concurrent_tasks: AtomicU32,
    pub polling_paused: AtomicBool,
    pub started_at: Instant,
}

impl AppState {
    pub fn new(config_path: PathBuf, app_data_dir: PathBuf, settings: Settings) -> Self {
        Self {
            config_path,
            app_data_dir,
            settings: RwLock::new(settings),
            worker_id: RwLock::new(String::new()),
            registered: AtomicBool::new(false),
            current_tasks: Mutex::new(HashMap::new()),
            stats: RwLock::new(RuntimeStats::default()),
            stale_threshold_sec: RwLock::new(600),
            max_concurrent_tasks: AtomicU32::new(1),
            server_max_concurrent_tasks: AtomicU32::new(0),
            polling_paused: AtomicBool::new(false),
            started_at: Instant::now(),
        }
    }

    pub fn is_polling_paused(&self) -> bool {
        self.polling_paused.load(Ordering::Acquire)
    }

    pub fn set_polling_paused(&self, paused: bool) {
        self.polling_paused.store(paused, Ordering::Release);
    }

    pub fn is_registered(&self) -> bool {
        self.registered.load(Ordering::Acquire)
    }

    pub fn set_registered(&self, v: bool) {
        self.registered.store(v, Ordering::Release);
    }

    pub fn running_task_count(&self) -> usize {
        self.current_tasks.lock().unwrap().len()
    }

    pub fn max_concurrent(&self) -> u32 {
        self.max_concurrent_tasks.load(Ordering::Acquire).max(1)
    }

    pub fn set_max_concurrent(&self, n: u32) {
        self.max_concurrent_tasks.store(n.max(1), Ordering::Release);
    }

    pub fn server_max_concurrent(&self) -> u32 {
        self.server_max_concurrent_tasks.load(Ordering::Acquire)
    }

    pub fn set_server_max_concurrent(&self, n: u32) {
        self.server_max_concurrent_tasks.store(n, Ordering::Release);
    }

    pub fn insert_task(&self, task: CurrentTask) {
        self.current_tasks
            .lock()
            .unwrap()
            .insert(task.job_id.clone(), task);
    }

    pub fn remove_task(&self, job_id: &str) {
        self.current_tasks.lock().unwrap().remove(job_id);
    }

    pub fn update_task_progress(&self, job_id: &str, stage: &str, progress: u8) {
        if let Ok(mut tasks) = self.current_tasks.lock() {
            if let Some(t) = tasks.get_mut(job_id) {
                t.stage = stage.to_string();
                t.progress = progress;
            }
        }
    }

    pub fn snapshot_tasks(&self) -> Vec<CurrentTask> {
        self.current_tasks.lock().unwrap().values().cloned().collect()
    }
}

pub type SharedState = Arc<AppState>;
