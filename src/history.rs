// history.rs：本地任务历史持久化（SQLite）。
//
// 设计：
//   - 单文件 SQLite，路径 <APP_DATA_DIR>/history.db
//   - 一张表 task_history，按 started_at DESC 列表查询
//   - 写入路径：runner 在 pipeline 开始 / 每阶段完成 / 终态时调用对应函数
//   - 用 Mutex<Connection> 保护，rusqlite 自身非线程安全
//   - 不引入 ORM（避免编译开销 + 字段简单）
//
// 字段策略：
//   - stages_json：JSON 数组 [{"stage":"download","start_ms":..,"end_ms":..}]
//   - asr_preview_json / mt_preview_json：JSON 数组（前 N 条文本），方便 UI 抽屉直接 render

use std::sync::Mutex;

use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS task_history (
    job_id TEXT PRIMARY KEY,
    media_id TEXT NOT NULL,
    media_title TEXT,
    source_lang TEXT NOT NULL,
    target_lang TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    status TEXT NOT NULL,
    error_msg TEXT,
    stages_json TEXT,
    asr_model TEXT,
    mt_model TEXT,
    segment_count INTEGER,
    vtt_size INTEGER,
    asr_preview_json TEXT,
    mt_preview_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_task_history_started_at ON task_history(started_at DESC);
"#;

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

/// 在应用启动时调用一次，初始化 SQLite 文件 + schema。
pub fn init(db_dir: &std::path::Path) -> Result<()> {
    if DB.get().is_some() {
        return Ok(());
    }
    let path = db_dir.join("history.db");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
    conn.execute_batch(SCHEMA_SQL).context("init schema")?;
    DB.set(Mutex::new(conn))
        .map_err(|_| anyhow::anyhow!("history DB already initialized"))?;
    tracing::info!("[history] db initialized at {}", path.display());
    Ok(())
}

fn with_conn<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let m = DB.get().context("history DB not initialized")?;
    let guard = m.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
    f(&guard).map_err(Into::into)
}

// === DTO ===

/// 单阶段耗时记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// 历史 row（完整版，给详情抽屉用）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskHistoryRow {
    pub job_id: String,
    pub media_id: String,
    pub media_title: Option<String>,
    pub source_lang: String,
    pub target_lang: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub error_msg: Option<String>,
    pub stages: Vec<StageRecord>,
    pub asr_model: Option<String>,
    pub mt_model: Option<String>,
    pub segment_count: Option<u32>,
    pub vtt_size: Option<u64>,
    pub asr_preview: Vec<String>,
    pub mt_preview: Vec<String>,
}

/// 列表用紧凑版（不含 preview / stages 详情，减少传输量）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskHistorySummary {
    pub job_id: String,
    pub media_id: String,
    pub media_title: Option<String>,
    pub source_lang: String,
    pub target_lang: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub error_msg: Option<String>,
    pub asr_model: Option<String>,
    pub segment_count: Option<u32>,
}

// === API ===

/// 任务开始：插入新行（status=running）。
pub fn insert_started(
    job_id: &str,
    media_id: &str,
    media_title: Option<&str>,
    source_lang: &str,
    target_lang: &str,
    started_at_ms: i64,
) -> Result<()> {
    with_conn(|c| {
        c.execute(
            r#"INSERT OR REPLACE INTO task_history
               (job_id, media_id, media_title, source_lang, target_lang, started_at, status, stages_json)
               VALUES (?, ?, ?, ?, ?, ?, 'running', '[]')"#,
            params![
                job_id,
                media_id,
                media_title,
                source_lang,
                target_lang,
                started_at_ms,
            ],
        )?;
        Ok(())
    })
}

/// 更新阶段时间线（覆写整个 stages_json）。
pub fn update_stages(job_id: &str, stages: &[StageRecord]) -> Result<()> {
    let json = serde_json::to_string(stages).unwrap_or_else(|_| "[]".into());
    with_conn(|c| {
        c.execute(
            "UPDATE task_history SET stages_json = ? WHERE job_id = ?",
            params![json, job_id],
        )?;
        Ok(())
    })
}

/// 任务成功完成。
pub fn mark_done(
    job_id: &str,
    finished_at_ms: i64,
    stages: &[StageRecord],
    asr_model: &str,
    mt_model: &str,
    segment_count: u32,
    vtt_size: u64,
    asr_preview: &[String],
    mt_preview: &[String],
) -> Result<()> {
    let stages_json = serde_json::to_string(stages).unwrap_or_else(|_| "[]".into());
    let asr_json = serde_json::to_string(asr_preview).unwrap_or_else(|_| "[]".into());
    let mt_json = serde_json::to_string(mt_preview).unwrap_or_else(|_| "[]".into());
    with_conn(|c| {
        c.execute(
            r#"UPDATE task_history SET
                 finished_at = ?,
                 status = 'done',
                 stages_json = ?,
                 asr_model = ?,
                 mt_model = ?,
                 segment_count = ?,
                 vtt_size = ?,
                 asr_preview_json = ?,
                 mt_preview_json = ?
               WHERE job_id = ?"#,
            params![
                finished_at_ms,
                stages_json,
                asr_model,
                mt_model,
                segment_count,
                vtt_size,
                asr_json,
                mt_json,
                job_id,
            ],
        )?;
        Ok(())
    })
}

/// 任务失败。
pub fn mark_failed(
    job_id: &str,
    finished_at_ms: i64,
    stages: &[StageRecord],
    error_msg: &str,
) -> Result<()> {
    let stages_json = serde_json::to_string(stages).unwrap_or_else(|_| "[]".into());
    with_conn(|c| {
        c.execute(
            r#"UPDATE task_history SET
                 finished_at = ?,
                 status = 'failed',
                 stages_json = ?,
                 error_msg = ?
               WHERE job_id = ?"#,
            params![finished_at_ms, stages_json, error_msg, job_id],
        )?;
        Ok(())
    })
}

/// 列表查询，按 started_at DESC。
pub fn list(limit: u32, offset: u32) -> Result<Vec<TaskHistorySummary>> {
    let limit = limit.clamp(1, 500);
    with_conn(|c| {
        let mut stmt = c.prepare(
            r#"SELECT job_id, media_id, media_title, source_lang, target_lang,
                      started_at, finished_at, status, error_msg, asr_model, segment_count
               FROM task_history
               ORDER BY started_at DESC
               LIMIT ? OFFSET ?"#,
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], |r| {
            Ok(TaskHistorySummary {
                job_id: r.get(0)?,
                media_id: r.get(1)?,
                media_title: r.get(2)?,
                source_lang: r.get(3)?,
                target_lang: r.get(4)?,
                started_at: r.get(5)?,
                finished_at: r.get(6)?,
                status: r.get(7)?,
                error_msg: r.get(8)?,
                asr_model: r.get(9)?,
                segment_count: r
                    .get::<_, Option<i64>>(10)?
                    .map(|x| x as u32),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// 详情查询。
pub fn get(job_id: &str) -> Result<Option<TaskHistoryRow>> {
    with_conn(|c| {
        c.query_row(
            r#"SELECT job_id, media_id, media_title, source_lang, target_lang,
                      started_at, finished_at, status, error_msg, stages_json,
                      asr_model, mt_model, segment_count, vtt_size,
                      asr_preview_json, mt_preview_json
               FROM task_history WHERE job_id = ?"#,
            params![job_id],
            |r| {
                let stages_json: Option<String> = r.get(9)?;
                let asr_preview_json: Option<String> = r.get(14)?;
                let mt_preview_json: Option<String> = r.get(15)?;
                let stages = stages_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<StageRecord>>(s).ok())
                    .unwrap_or_default();
                let asr_preview = asr_preview_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .unwrap_or_default();
                let mt_preview = mt_preview_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .unwrap_or_default();
                Ok(TaskHistoryRow {
                    job_id: r.get(0)?,
                    media_id: r.get(1)?,
                    media_title: r.get(2)?,
                    source_lang: r.get(3)?,
                    target_lang: r.get(4)?,
                    started_at: r.get(5)?,
                    finished_at: r.get(6)?,
                    status: r.get(7)?,
                    error_msg: r.get(8)?,
                    stages,
                    asr_model: r.get(10)?,
                    mt_model: r.get(11)?,
                    segment_count: r
                        .get::<_, Option<i64>>(12)?
                        .map(|x| x as u32),
                    vtt_size: r
                        .get::<_, Option<i64>>(13)?
                        .map(|x| x as u64),
                    asr_preview,
                    mt_preview,
                })
            },
        )
        .optional()
    })
}

/// 清空历史（带保留最近 keep_recent 条选项）。
pub fn clear(keep_recent: u32) -> Result<usize> {
    with_conn(|c| {
        if keep_recent == 0 {
            c.execute("DELETE FROM task_history", [])
        } else {
            c.execute(
                r#"DELETE FROM task_history
                   WHERE job_id NOT IN (
                     SELECT job_id FROM task_history
                     ORDER BY started_at DESC
                     LIMIT ?
                   )"#,
                params![keep_recent as i64],
            )
        }
    })
}

/// 进程崩溃时未关闭的 running 任务在下次启动时一律标记为 failed。
pub fn recover_orphans(now_ms: i64) -> Result<usize> {
    with_conn(|c| {
        c.execute(
            r#"UPDATE task_history SET status = 'failed',
                 finished_at = ?,
                 error_msg = COALESCE(error_msg, 'worker crashed before completion')
               WHERE status = 'running'"#,
            params![now_ms],
        )
    })
}
