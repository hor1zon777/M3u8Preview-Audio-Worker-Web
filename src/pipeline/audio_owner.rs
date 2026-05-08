//! pipeline/audio_owner.rs：v3 broker 模式下 audio worker 的本地 FLAC 仓库 +
//! long-poll fetch 循环。
//!
//! 与 v2（audio_complete 上传后即清）的差别：
//!   - audio worker 完成 FLAC 后保留在本地 `audio_storage_dir/<jobId>.flac`
//!   - 同时在 `audio_storage_dir/<jobId>.json` 写一份索引（size / sha256 / format / duration_ms）
//!   - 启动时扫描这个目录重新向服务端 audio-ready 注册
//!   - 后台运行 fetch_loop：long-poll 服务端，收到 fetch 通知就上传，cleanup 通知就删本地
//!
//! 文件命名约定：
//!   - `<dir>/<jobId>.flac`：FLAC 二进制
//!   - `<dir>/<jobId>.json`：索引（与 FLAC 一同 fsync 后写）
//!
//! 同步保证：
//!   - 写时：先写 .flac，再写 .json.tmp，rename .json.tmp → .json，确保索引与文件一致
//!   - 删时：先删 .json，再删 .flac（防 cleanup 半途崩溃残留 FLAC 但元数据丢失）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::api::{ApiClient, ApiError, AudioCompleteMeta};
use crate::state::SharedState;

/// 单个 FLAC 在本地的元数据索引。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioIndexEntry {
    pub job_id: String,
    pub media_id: String,
    pub size: i64,
    pub sha256: String,
    pub format: String,
    pub duration_ms: i64,
    /// 本地 FLAC 绝对路径
    pub flac_path: PathBuf,
    /// 创建时间（unix ms）
    pub created_at_ms: i64,
}

/// 写入索引 + FLAC 已经准备好的产物。调用方负责把 FLAC 写到 entry.flac_path 后
/// 调 save_index 把索引落盘。
pub fn save_index(dir: &Path, entry: &AudioIndexEntry) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let json_path = dir.join(format!("{}.json", entry.job_id));
    let tmp_path = dir.join(format!("{}.json.tmp", entry.job_id));
    let body = serde_json::to_vec_pretty(entry)?;
    std::fs::write(&tmp_path, body)
        .with_context(|| format!("write tmp index {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &json_path)
        .with_context(|| format!("rename {} → {}", tmp_path.display(), json_path.display()))?;
    Ok(())
}

/// 删除指定 jobId 的本地 FLAC + 索引。删除顺序：先 json，再 flac。
pub fn remove_entry(dir: &Path, job_id: &str) -> Result<()> {
    let json_path = dir.join(format!("{}.json", job_id));
    let flac_path = dir.join(format!("{}.flac", job_id));
    if let Err(e) = std::fs::remove_file(&json_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("[audio-owner] remove json {}: {}", json_path.display(), e);
        }
    }
    if let Err(e) = std::fs::remove_file(&flac_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("[audio-owner] remove flac {}: {}", flac_path.display(), e);
        }
    }
    Ok(())
}

/// resolve_storage_dir：根据 settings + app_data_dir 解析最终的 audio_storage_dir。
///
/// 优先级：
///   1. settings.pipeline.audio_storage_dir（用户显式配置）
///   2. app_data_dir/audio_artifacts（默认 fallback）
///
/// 这个函数同时被 runner（写入 FLAC）与 poller（读取本地 pending 数）使用，
/// 必须保证两边解析出的路径一致。
pub fn resolve_storage_dir(state: &SharedState) -> Result<PathBuf> {
    let configured = state
        .settings
        .read()
        .map_err(|_| anyhow!("settings lock poisoned"))?
        .pipeline
        .audio_storage_dir
        .trim()
        .to_string();
    if !configured.is_empty() {
        return Ok(PathBuf::from(configured));
    }
    Ok(state.app_data_dir.join("audio_artifacts"))
}

/// 快速统计 audio_storage_dir 下的未消费 FLAC 文件数。
///
/// 仅 enumerate `.flac` 文件，不读取 / 解析 `.json` 索引——上游 poller 只需要
/// 知道"是否达到 audio_local_max_pending 阈值"，比 [`scan_entries`] 轻量得多。
///
/// 目录不存在时返回 0（首次启动尚未创建目录是合法状态）。
pub fn count_pending_entries(dir: &Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    entries
        .flatten()
        .filter(|ent| {
            ent.path().is_file()
                && ent
                    .file_name()
                    .to_str()
                    .map(|n| n.ends_with(".flac"))
                    .unwrap_or(false)
        })
        .count()
}

/// 扫描 audio_storage_dir 列出所有合法 entry（同时存在 .flac + .json 且 json 能解析的）。
pub fn scan_entries(dir: &Path) -> Result<Vec<AudioIndexEntry>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?;
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|x| x.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.ends_with(".json") {
            continue;
        }
        let body = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("[audio-owner] read {} failed: {}", path.display(), e);
                continue;
            }
        };
        let mut entry: AudioIndexEntry = match serde_json::from_slice(&body) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("[audio-owner] parse {} failed: {}", path.display(), e);
                continue;
            }
        };
        // 路径绝对化必须先于 is_file 检查：早期版本把 normalize 放在 is_file 之后，
        // 当 entry.flac_path 是相对路径（用户 audio_storage_dir 配了相对路径时可能发生）
        // 时 is_file 以进程 CWD 为基准解析，会错判文件不存在导致整条 entry 被静默跳过，
        // 进而触发"fetch requested but no local FLAC found"。
        if entry.flac_path.is_relative() {
            entry.flac_path = dir.join(&entry.flac_path);
        }
        // FLAC 必须存在；不一致就不算合法 entry
        if !entry.flac_path.is_file() {
            tracing::warn!(
                "[audio-owner] index {} references missing FLAC {}",
                path.display(),
                entry.flac_path.display()
            );
            continue;
        }
        // 文件大小校验（不读 sha256，避免启动慢）
        if let Ok(meta) = std::fs::metadata(&entry.flac_path) {
            if meta.len() as i64 != entry.size {
                tracing::warn!(
                    "[audio-owner] index {} size mismatch (json={} disk={}); will skip",
                    entry.job_id,
                    entry.size,
                    meta.len()
                );
                continue;
            }
        }
        out.push(entry);
    }
    Ok(out)
}

/// startup_cleanup_suspicious：启动时扫描 storage_dir，删除"看起来像残品"的 entry。
///
/// 启动期没有 m3u8 上下文，无法做相对时长比对，只能用绝对最小时长阈值兜底。
///
/// 删除条件（满足其一）：
///   1. duration_ms < min_duration_ms（典型场景：lenient ffmpeg 抢救出几秒残品）
///   2. .json 索引存在但 .flac 缺失 / size 不匹配（已经在 scan_entries 里过滤）
///
/// 返回被清理的 entry 数。
///
/// 必须在 fetch_loop 启动**之前**调用，避免 broker 派 fetch 时把残品上传给 subtitle worker。
pub fn startup_cleanup_suspicious(storage_dir: &Path, min_duration_sec: u64) -> usize {
    let entries = match scan_entries(storage_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                "[audio-owner] startup cleanup: scan {} failed: {:#}",
                storage_dir.display(),
                e
            );
            return 0;
        }
    };
    let min_ms = (min_duration_sec.saturating_mul(1000)) as i64;
    let mut removed = 0usize;
    for entry in entries {
        if entry.duration_ms < min_ms {
            tracing::warn!(
                "[audio-owner] startup cleanup: removing suspicious entry job={} duration_ms={} \
                 size={} (< min_duration_ms={}). Likely produced by ffmpeg lenient fallback or \
                 truncated input — refusing to register with server.",
                entry.job_id,
                entry.duration_ms,
                entry.size,
                min_ms
            );
            let _ = remove_entry(storage_dir, &entry.job_id);
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            "[audio-owner] startup cleanup: removed {} suspicious entries from {}",
            removed,
            storage_dir.display()
        );
    }
    removed
}

/// startup_resync：启动时把本地遗留的 FLAC 重新向服务端注册（audio-ready）。
///
/// 服务端可能返回：
///   - 200 OK：任务仍在 audio 阶段（downloading / extracting / encoding_intermediate）
///     且 claimed_by 仍是本 worker → 注册成功
///   - 409 / 410：任务状态已变（已被 stale recovery 回滚到 queued / 被别的 worker 抢走 /
///     已 done / 已 failed）→ 本地文件无主，删除
///   - 其它错误：记日志，下次启动重试
///
/// 注（v3.1）：服务端 audio-ready 校验已放宽到 `stage IN (downloading, extracting,
/// encoding_intermediate)`，避免心跳异步同步带来的 stage 滞后。
/// 但 ownership 校验仍要求 `claimed_by == workerId`：worker 重启后 claimed_by 通常已被
/// stale recovery 清空，这种情况会收到 410，本地文件随之清理。
pub async fn startup_resync(
    storage_dir: &Path,
    client: Arc<ApiClient>,
    worker_id: &str,
) -> Result<usize> {
    let entries = scan_entries(storage_dir)?;
    if entries.is_empty() {
        return Ok(0);
    }
    tracing::info!(
        "[audio-owner] startup resync: {} local FLAC entries to re-register",
        entries.len()
    );
    let mut ok = 0usize;
    for entry in entries {
        let meta = AudioCompleteMeta {
            worker_id: worker_id.to_string(),
            size: entry.size,
            sha256: entry.sha256.clone(),
            format: entry.format.clone(),
            duration_ms: entry.duration_ms,
        };
        match client.audio_ready(&entry.job_id, &meta).await {
            Ok(()) => {
                ok += 1;
                tracing::info!(
                    "[audio-owner] resync ok: job={} size={} sha={}",
                    entry.job_id,
                    entry.size,
                    &entry.sha256[..8.min(entry.sha256.len())]
                );
            }
            Err(ApiError::Server { status, body }) if status == 409 || status == 410 => {
                // 任务状态已变 → 本地文件已经无主，清理
                tracing::info!(
                    "[audio-owner] resync rejected (status={}): job={} body={}; removing local",
                    status,
                    entry.job_id,
                    body
                );
                let _ = remove_entry(storage_dir, &entry.job_id);
            }
            Err(e) => {
                tracing::warn!("[audio-owner] resync failed: job={} err={}", entry.job_id, e);
            }
        }
    }
    Ok(ok)
}

/// fetch_loop 是 audio worker 的核心 long-poll 循环。
///
/// 设计：
///   - 每轮 poll 25 秒；服务端没任务 204，立即 reconnect
///   - 收到 fetch 通知 → 通过 storage_resolver 实时解析当前 storage_dir → 查本地索引
///     → 调 audio_stream 流式上传；若本地找不到 FLAC，调 audio_lost 让服务端回滚
///   - 收到 cleanup 通知 → remove_entry
///   - 网络错误 / 服务端 5xx → 指数退避 1→2→4→...→60s
///
/// **storage_dir 必须每次实时解析**，不能在循环外快照。否则用户在 settings 改了
/// audio_storage_dir 后，runner 已经写入新路径，但 fetch_loop 仍扫旧路径，
/// 表现为 "fetch requested but no local FLAC found" 死循环。
///
/// 退出条件：cancel notify 收到信号（在 lib.rs 关闭时触发）。
///
/// 该 task 与 poller::run（claim 循环）并行运行。
pub async fn fetch_loop(
    storage_resolver: impl Fn() -> Result<PathBuf> + Send + Sync + 'static,
    client_factory: impl Fn() -> Option<(Arc<ApiClient>, String)> + Send + Sync + 'static,
    cancel: Arc<tokio::sync::Notify>,
) {
    const POLL_TIMEOUT_SEC: u32 = 25;
    let mut backoff = Duration::from_secs(1);

    loop {
        // cancel 检查
        if let Ok(()) = tokio::time::timeout(Duration::from_millis(0), cancel.notified()).await {
            return;
        }
        let (client, worker_id) = match client_factory() {
            Some(v) => v,
            None => {
                // 配置不全，等 5s 重试
                tokio::select! {
                    _ = cancel.notified() => return,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                continue;
            }
        };

        // 一次 poll
        match client.audio_fetch_poll(&worker_id, POLL_TIMEOUT_SEC).await {
            Ok(Some(task)) => {
                backoff = Duration::from_secs(1);
                tracing::info!(
                    "[audio-owner] fetch task: action={} job={}",
                    task.action,
                    task.job_id
                );
                // 实时解析当前 storage_dir
                let storage_dir = match storage_resolver() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            "[audio-owner] resolve storage_dir failed: {e:#}; skip task action={} job={}",
                            task.action,
                            task.job_id
                        );
                        continue;
                    }
                };
                if let Err(e) =
                    handle_task(&storage_dir, &client, &worker_id, &task).await
                {
                    tracing::warn!(
                        "[audio-owner] handle task failed: action={} job={} err={:#}",
                        task.action,
                        task.job_id,
                        e
                    );
                }
            }
            Ok(None) => {
                // 204：立即重新 poll，复位退避
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                tracing::warn!("[audio-owner] poll error: {} (backoff {:?})", e, backoff);
                tokio::select! {
                    _ = cancel.notified() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(60));
            }
        }
    }
}

async fn handle_task(
    storage_dir: &Path,
    client: &ApiClient,
    worker_id: &str,
    task: &crate::api::AudioFetchTask,
) -> Result<()> {
    match task.action.as_str() {
        "fetch" => {
            // 查本地索引
            let entries = scan_entries(storage_dir)?;
            let entry = entries.iter().find(|e| e.job_id == task.job_id);
            match entry {
                Some(entry) => {
                    // v3.2 分块上传（绕开 Cloudflare 等 CDN 100 MiB 上限）；服务端
                    // broker 在同一条 io.Pipe 上拼接，subtitle worker 端无感知
                    if let Err(e) = client
                        .audio_stream_chunked(&task.job_id, worker_id, &entry.flac_path)
                        .await
                    {
                        // 流式上传失败：保留本地 FLAC 等服务端重试（不调 audio_lost、
                        // 不删 entry）。服务端 broker 超时后应增加重试次数重新派发
                        // fetch，若超过 maxRetries 才发 cleanup 指令清理本地。
                        tracing::warn!(
                            "[audio-owner] audio_stream_chunked failed: job={} err={:#} \
                             (keeping local FLAC for server retry)",
                            task.job_id,
                            e
                        );
                        return Ok(());
                    }
                    tracing::info!(
                        "[audio-owner] fetch served: job={} size={}",
                        task.job_id,
                        entry.size
                    );
                    Ok(())
                }
                None => {
                    // 找不到 FLAC：增强诊断 + 主动声明 audio-lost 让服务端回滚，
                    // 避免 broker 反复派发同一个 fetch task 把日志刷爆
                    let dir_listing = dir_listing_summary(storage_dir);
                    tracing::warn!(
                        "[audio-owner] fetch requested for job={} but no local FLAC found. \
                         storage_dir={} entries_scanned={} dir_listing=[{}]",
                        task.job_id,
                        storage_dir.display(),
                        entries.len(),
                        dir_listing
                    );

                    // 调 audio-lost：清残留索引（防止下轮 scan 还是这个状态）+ 通知服务端
                    let _ = remove_entry(storage_dir, &task.job_id);
                    let err_msg = format!(
                        "no local FLAC for job={} (storage_dir={}, entries={})",
                        task.job_id,
                        storage_dir.display(),
                        entries.len()
                    );
                    match client.audio_lost(&task.job_id, worker_id, &err_msg).await {
                        Ok(()) => {
                            tracing::info!(
                                "[audio-owner] audio-lost reported: job={} (server will roll back to queued)",
                                task.job_id
                            );
                        }
                        Err(e) => {
                            // 服务端拒绝（owner 已不是本 worker / stage 已变 / job 不存在）—— 都是
                            // 终止状态，本地无须再持有这个 jobId。继续工作即可。
                            tracing::warn!(
                                "[audio-owner] audio-lost rejected (job={}): {e}; broker will time out on its own",
                                task.job_id
                            );
                        }
                    }
                    Ok(())
                }
            }
        }
        "cleanup" => {
            remove_entry(storage_dir, &task.job_id)?;
            tracing::info!("[audio-owner] cleanup done: job={}", task.job_id);
            Ok(())
        }
        other => Err(anyhow!("unknown fetch task action: {}", other)),
    }
}

/// 把 storage_dir 内的文件列出来，方便 fetch 找不到 FLAC 时诊断。
/// 控制总长度避免日志爆炸：最多前 20 个文件名。
fn dir_listing_summary(dir: &Path) -> String {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(d) => d,
        Err(e) => return format!("(read_dir error: {e})"),
    };
    let names: Vec<String> = read_dir
        .flatten()
        .filter_map(|ent| ent.file_name().to_str().map(|s| s.to_string()))
        .take(20)
        .collect();
    if names.is_empty() {
        "<empty>".to_string()
    } else {
        names.join(", ")
    }
}
