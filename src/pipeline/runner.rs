// pipeline/runner.rs：audio worker 流水线，把 3 个阶段串起来 + 周期性 heartbeat。
//
// 流程（与 distributed-worker.md §6.2.3 一致）：
//   queued (1%)
//     → downloading (5~50%, N_m3u8DL-RE 拉 m3u8)
//     → extracting  (50~70%, ffmpeg 把 mp4/ts → 16 kHz mono WAV)
//     → encoding_intermediate (70~99%, ffmpeg WAV → FLAC + SHA256 + duration)
//     → audio_complete 上传成功
//
// audio worker 不调用服务端 complete 端点（那是 subtitle worker 上传 VTT 用的）；
// 上传 FLAC 走 client.audio_complete()，服务端把 stage 推到 audio_uploaded。
//
// heartbeat：spawn 一个独立 tokio task，每 heartbeat_interval_sec 秒把当前 stage+progress
// 发到服务器；run_pipeline 结束时 cancel。
//
// 错误归因：每个阶段失败都加阶段前缀，方便从服务器侧 errorMsg 一眼看出哪一步挂了。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tempfile::TempDir;
use tokio::task::JoinHandle;

use crate::api::{ApiClient, AudioCompleteMeta, ClaimedJob};
use crate::config::AudioFormat;
use crate::history::{self, StageRecord};
use crate::state::SharedState;

use super::{
    audio_owner, downloader, extractor,
    intermediate::{encode_flac, probe_duration_ms, sha256_and_size},
    tools::Tools,
};

/// 当前 stage + progress，与 heartbeat task 共享。
#[derive(Debug, Clone)]
struct Phase {
    stage: String,
    progress: u8,
}

/// pipeline 中累积的可观测数据。无论成功失败都会写到 history。
#[derive(Debug, Default)]
struct PipelineCtx {
    stages: Vec<StageRecord>,
    flac_size: u64,
    duration_ms: i64,
    sha256: String,
    format: String,
}

/// 主入口：跑完整 audio pipeline，成功时返回 (FLAC 字节数, sha256, duration_ms)；
/// 失败返回带阶段前缀的错误。
///
/// 内部会启动 heartbeat 后台 task。
pub async fn run_pipeline(
    state: SharedState,
    client: Arc<ApiClient>,
    job: ClaimedJob,
) -> Result<(u64, String, i64)> {
    // 防御性校验：stage 必须是 audio_extract（服务端不会派错，但防御性兜底）
    if !job.stage.is_empty() && job.stage != "audio_extract" {
        return Err(anyhow!(
            "audio worker received unexpected stage: {} (expected audio_extract)",
            job.stage
        ));
    }
    let m3u8_url = job
        .m3u8_url
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("audio worker job missing m3u8Url"))?;

    let started_at = chrono::Utc::now().timestamp_millis();
    tracing::info!(
        "[runner] entering audio pipeline for job={} media={} m3u8={}",
        job.job_id,
        job.media_id,
        truncate_url(&m3u8_url, 200)
    );

    // 落库：开始
    if let Err(e) = history::insert_started(
        &job.job_id,
        &job.media_id,
        job.media_title.as_deref(),
        &job.source_lang,
        &job.target_lang,
        started_at,
    ) {
        tracing::warn!("[history] insert_started failed: {e}");
    }

    let phase = Arc::new(RwLock::new(Phase {
        stage: "queued".to_string(),
        progress: 1,
    }));
    // v4：心跳取消信号必须用持久化标志（AtomicBool）。
    //
    // 历史 bug：v3 用 tokio::sync::Notify + notify_waiters()，但 notify_waiters() 是
    // 一次性事件，对"还没进入 .notified() await"的任务无效。心跳任务在两次 sleep 之间
    // 短暂离开 select! 的 cancel arm，notify_waiters 这时被调用就完全丢失，导致心跳
    // 在 audio_ready 之后还会继续 fire 30s（用户日志中两次 410 间隔正是 30s 心跳周期）。
    //
    // AtomicBool 是持久化的：只要 set true，下一次 sleep 醒来检查就一定会看到。
    let cancel = Arc::new(AtomicBool::new(false));

    // 启动 heartbeat task
    let hb_settings = state.settings.read().unwrap().clone();
    let heartbeat = spawn_heartbeat(
        client.clone(),
        state.worker_id.read().unwrap().clone(),
        job.job_id.clone(),
        Arc::clone(&phase),
        Arc::clone(&cancel),
        hb_settings.server.heartbeat_interval_sec.max(5),
        *state.stale_threshold_sec.read().unwrap(),
    );

    // 跑流水线（pipeline_inner 会在 audio_ready 成功后立即 cancel 心跳，
    // 避免服务端清空 claimed_by 后心跳收到 410）
    let mut ctx = PipelineCtx::default();
    let result = pipeline_inner(
        &state,
        client.clone(),
        &job,
        &m3u8_url,
        &phase,
        Arc::clone(&cancel),
        &mut ctx,
    )
    .await;

    if let Err(e) = &result {
        tracing::warn!("[runner] job {} aborted: {e:#}", job.job_id);
    }

    // 兜底 cancel + join heartbeat（pipeline_inner 提前成功 cancel 时是 no-op）
    cancel.store(true, Ordering::Release);
    let _ = heartbeat.await;

    // 落库：终态
    let now = chrono::Utc::now().timestamp_millis();
    match &result {
        Ok((size, _sha, _dur)) => {
            // history schema 沿用字幕项目字段：
            //   asr_model = "audio_extract"（占位，标识此条来自 audio worker）
            //   mt_model  = format（flac / opus_24k / ...）
            //   segment_count = 0（audio worker 没有字幕段概念）
            //   vtt_size = flac size
            if let Err(e) = history::mark_done(
                &job.job_id,
                now,
                &ctx.stages,
                "audio_extract",
                &ctx.format,
                0,
                *size,
                &[],
                &[],
            ) {
                tracing::warn!("[history] mark_done failed: {e}");
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if let Err(he) = history::mark_failed(&job.job_id, now, &ctx.stages, &msg) {
                tracing::warn!("[history] mark_failed failed: {he}");
            }
        }
    }

    result
}

fn truncate_url(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

async fn pipeline_inner(
    state: &SharedState,
    client: Arc<ApiClient>,
    job: &ClaimedJob,
    m3u8_url: &str,
    phase: &Arc<RwLock<Phase>>,
    cancel: Arc<AtomicBool>,
    ctx: &mut PipelineCtx,
) -> Result<(u64, String, i64)> {
    // 0. 准备工具 + 临时工作目录
    tracing::info!("[runner] step 0/3: resolving tools");
    let stage_start = chrono::Utc::now().timestamp_millis();
    let settings = state.settings.read().unwrap().clone();
    let app_data_dir = state.app_data_dir.clone();
    let tools = Tools::resolve(
        &settings.pipeline.m3u8dl_path,
        &settings.pipeline.ffmpeg_path,
        None,
        Some(&app_data_dir),
    )
    .map_err(|e| {
        tracing::warn!("[runner] tools resolve failed: {e}");
        anyhow!("tools: {e}")
    })?;
    tracing::info!(
        "[runner] tools resolved: m3u8dl={} ffmpeg={}",
        tools.m3u8dl.display(),
        tools.ffmpeg.display()
    );
    push_stage(ctx, &job.job_id, "prepare", stage_start);

    let workdir: TempDir = create_workdir(&settings.pipeline.temp_dir).context("create workdir")?;
    let workdir_path = workdir.path().to_path_buf();
    tracing::info!(
        "[runner] job {} workdir = {}",
        job.job_id,
        workdir_path.display()
    );

    // 1. 下载（占 5~50%）
    set_phase(phase, "downloading", 5);
    update_state_task(state, &job.job_id, "downloading", 5);
    tracing::info!("[runner] step 1/3: downloading m3u8");
    if !job.headers.is_empty() {
        let keys: Vec<&str> = job.headers.keys().map(|s| s.as_str()).collect();
        tracing::info!("[runner] download headers from server: {:?}", keys);
    }
    let stage_start = chrono::Utc::now().timestamp_millis();
    let media_path = downloader::download(&tools, m3u8_url, &workdir_path, &job.headers, &settings.network.download_proxy)
        .await
        .map_err(|e| anyhow!("download: {e:#}"))?;
    set_phase(phase, "downloading", 50);
    update_state_task(state, &job.job_id, "downloading", 50);
    push_stage(ctx, &job.job_id, "download", stage_start);
    tracing::info!(
        "[runner] downloaded media: {} ({} bytes)",
        media_path.display(),
        std::fs::metadata(&media_path).map(|m| m.len()).unwrap_or(0)
    );

    // 2. 抽音（占 50~70%）
    set_phase(phase, "extracting", 50);
    update_state_task(state, &job.job_id, "extracting", 50);
    tracing::info!("[runner] step 2/3: extracting WAV");
    let stage_start = chrono::Utc::now().timestamp_millis();
    let wav_path = extractor::extract_wav(&tools, &media_path, &workdir_path)
        .await
        .map_err(|e| anyhow!("extract: {e:#}"))?;
    set_phase(phase, "extracting", 70);
    update_state_task(state, &job.job_id, "extracting", 70);
    push_stage(ctx, &job.job_id, "extract", stage_start);
    tracing::info!(
        "[runner] extracted WAV: {} ({} bytes)",
        wav_path.display(),
        std::fs::metadata(&wav_path).map(|m| m.len()).unwrap_or(0)
    );

    // 3. FLAC 编码 + 注册 audio-ready（占 70~99%）
    //    v3 broker 模式：FLAC 写到 audio_storage_dir 永久保存，audio-ready 只发元数据；
    //    后续 subtitle worker 拉取时，audio_owner::fetch_loop 会通过 audio-stream 流式上传。
    set_phase(phase, "encoding_intermediate", 70);
    update_state_task(state, &job.job_id, "encoding_intermediate", 70);
    tracing::info!("[runner] step 3/3: encoding FLAC → audio_storage_dir + audio-ready");
    let stage_start = chrono::Utc::now().timestamp_millis();

    // 3.1 FLAC 编码（写到 audio_storage_dir/<jobId>.flac）
    let flac_format_str = settings
        .pipeline
        .intermediate_audio_format
        .as_protocol_str()
        .to_string();
    if settings.pipeline.intermediate_audio_format != AudioFormat::Flac {
        return Err(anyhow!(
            "intermediate_audio_format={} not yet implemented; only flac is supported",
            flac_format_str
        ));
    }
    let storage_dir = audio_owner::resolve_storage_dir(state)?;
    std::fs::create_dir_all(&storage_dir)
        .with_context(|| format!("ensure audio_storage_dir {}", storage_dir.display()))?;
    let final_flac_path = storage_dir.join(format!("{}.flac", job.job_id));

    // 编码到 work_dir 临时位置，再 rename 到 storage_dir（保证读 .flac 时永远是完整文件）
    let tmp_flac = encode_flac(
        &tools,
        &wav_path,
        &workdir_path,
        settings.pipeline.flac_compression_level,
        Duration::from_secs(settings.pipeline.flac_timeout_sec.max(60)),
    )
    .await
    .map_err(|e| anyhow!("encode: {e:#}"))?;
    set_phase(phase, "encoding_intermediate", 80);
    update_state_task(state, &job.job_id, "encoding_intermediate", 80);

    // 3.2 SHA256 + size + duration（并行）
    let (size_sha, dur) = tokio::join!(
        sha256_and_size(&tmp_flac),
        probe_duration_ms(&tools, &tmp_flac),
    );
    let (size_bytes, sha256) = size_sha.map_err(|e| anyhow!("sha256: {e:#}"))?;
    let duration_ms = dur.map_err(|e| anyhow!("duration probe: {e:#}"))?;
    tracing::info!(
        "[runner] flac meta: size={} sha256={} duration_ms={}",
        size_bytes,
        &sha256[..8.min(sha256.len())],
        duration_ms
    );

    // 3.3 移动 FLAC 到 audio_storage_dir + 写索引
    //
    // 注意：fs::rename 在 Windows 跨驱动器会返回 ERROR_NOT_SAME_DEVICE (os error 17)，
    // 用户常见 case：temp_dir 在 E:、audio_storage_dir 在 %APPDATA%（C:）。
    // 用 move_file 自带 copy+delete fallback。
    if final_flac_path.exists() {
        let _ = std::fs::remove_file(&final_flac_path);
    }
    move_file(&tmp_flac, &final_flac_path).with_context(|| {
        format!(
            "move flac {} → {}",
            tmp_flac.display(),
            final_flac_path.display()
        )
    })?;
    let entry = audio_owner::AudioIndexEntry {
        job_id: job.job_id.clone(),
        media_id: job.media_id.clone(),
        size: size_bytes,
        sha256: sha256.clone(),
        format: flac_format_str.clone(),
        duration_ms,
        flac_path: final_flac_path.clone(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    audio_owner::save_index(&storage_dir, &entry).with_context(|| "save audio index")?;
    set_phase(phase, "encoding_intermediate", 90);
    update_state_task(state, &job.job_id, "encoding_intermediate", 90);

    // 3.4 注册 audio-ready（仅元数据，不上传文件）
    let worker_id = state.worker_id.read().unwrap().clone();
    let meta = AudioCompleteMeta {
        worker_id: worker_id.clone(),
        size: size_bytes,
        sha256: sha256.clone(),
        format: flac_format_str.clone(),
        duration_ms,
    };

    // 同步把服务端 stage 推到 encoding_intermediate，避免心跳异步推送来不及导致
    // audio-ready 被服务端以 "stage extracting/downloading does not allow audio-ready"（409）
    // 拒绝。
    //
    // 背景：心跳后台任务默认间隔 30s（settings.server.heartbeat_interval_sec），
    //   而本流水线从 set_phase("extracting") → set_phase("encoding_intermediate") →
    //   FLAC 编码 → audio_ready 通常 < 30s，心跳极易在两次 stage 切换之间被读到旧值。
    //   这里在 audio_ready 之前同步发一次心跳，保证服务端 stage 已对齐。
    //
    // 失败策略：仅记 warn，不直接 fail。服务端从 v3.x 起也允许 audio_ready 从 audio
    // 阶段集合（downloading / extracting / encoding_intermediate）转入 audio_uploaded，
    // 心跳失败仍有兜底。
    if let Err(e) = client
        .heartbeat(&job.job_id, &worker_id, "encoding_intermediate", 99)
        .await
    {
        tracing::warn!(
            "[runner] pre-audio_ready heartbeat failed (will rely on server-side leniency): {e}"
        );
    }

    if let Err(e) = client.audio_ready(&job.job_id, &meta).await {
        // 注册失败：保留本地 FLAC + 索引，等下次 audio worker 启动时 startup_resync
        return Err(anyhow!("audio_ready: {e}"));
    }

    // v4：audio_ready 成功 → 服务端清空 claimed_by → 心跳如果继续 fire 会拿 410。
    // 立刻 cancel 心跳任务避免噪音 + 误导日志。
    // 后续 set_phase / push_stage 仅本地状态，不走网络。
    cancel.store(true, Ordering::Release);

    set_phase(phase, "encoding_intermediate", 99);
    update_state_task(state, &job.job_id, "encoding_intermediate", 99);
    push_stage(ctx, &job.job_id, "encode_and_register", stage_start);

    ctx.flac_size = size_bytes as u64;
    ctx.duration_ms = duration_ms;
    ctx.sha256 = sha256.clone();
    ctx.format = flac_format_str;

    // workdir 在 TempDir drop 时自动清理（mp4 / wav 都在 workdir 内）；
    // FLAC 已经移到 audio_storage_dir，由 audio_owner cleanup 通知后才删
    Ok((size_bytes as u64, sha256, duration_ms))
}

/// move_file：跨驱动器安全的文件移动。
///
/// fs::rename 在以下情况会报错：
///   - Windows ERROR_NOT_SAME_DEVICE (os error 17)：源 / 目标在不同盘
///   - Linux EXDEV (errno 18)：跨文件系统
///
/// 实测 case：用户 temp_dir 配在 E:\tmp，audio_storage_dir 默认在 %APPDATA% (C:)。
///
/// 策略：先尝试 rename（同盘原子，最快）；失败且是跨设备错误时 fallback 到
/// copy + remove。其它错误（权限不足等）原样返回。
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device_error(&e) => {
            tracing::info!(
                "[runner] cross-device rename detected, falling back to copy: {} → {}",
                src.display(),
                dst.display()
            );
            std::fs::copy(src, dst)?;
            // copy 成功后删源文件；若 remove 失败仅记日志（目标已 OK，下次清 temp 会处理）
            if let Err(e) = std::fs::remove_file(src) {
                tracing::warn!(
                    "[runner] copy ok but remove src failed: {} ({})",
                    src.display(),
                    e
                );
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// 判断 io::Error 是否是"跨设备"错误。
///
/// 标准库 `ErrorKind::CrossesDevices` 在 Rust 1.85+ 才稳定；为兼容更老版本
/// 直接看 raw_os_error：Windows=17 (ERROR_NOT_SAME_DEVICE)，Unix=18 (EXDEV)。
fn is_cross_device_error(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(17) | Some(18) => true,
        _ => false,
    }
}

fn set_phase(phase: &Arc<RwLock<Phase>>, stage: &str, progress: u8) {
    let mut p = phase.write().unwrap();
    p.stage = stage.to_string();
    p.progress = progress;
}

fn update_state_task(state: &SharedState, job_id: &str, stage: &str, progress: u8) {
    state.update_task_progress(job_id, stage, progress);
}

fn push_stage(ctx: &mut PipelineCtx, _job_id: &str, label: &str, start_ms: i64) {
    let now = chrono::Utc::now().timestamp_millis();
    ctx.stages.push(StageRecord {
        stage: label.to_string(),
        start_ms,
        end_ms: now,
    });
}

/// 创建 pipeline 工作目录。空 base = 用系统 temp。
/// 用 m3u8-audio-worker- 前缀避免和字幕项目混在一起。
fn create_workdir(temp_base: &str) -> Result<TempDir> {
    let prefix = "m3u8-audio-worker-";
    if temp_base.trim().is_empty() {
        Ok(tempfile::Builder::new().prefix(prefix).tempdir()?)
    } else {
        let base = PathBuf::from(temp_base);
        std::fs::create_dir_all(&base)
            .with_context(|| format!("ensure temp_dir: {}", base.display()))?;
        Ok(tempfile::Builder::new().prefix(prefix).tempdir_in(&base)?)
    }
}

/// 心跳后台 task：周期上报当前 stage + progress；fail/done 时由 main task 通过
/// `cancel.store(true)` 通知退出。
///
/// v4：cancel 用 `Arc<AtomicBool>` 而不是 `tokio::sync::Notify`。
/// 原因：Notify::notify_waiters() 不留存许可，对"未在 .notified() await 的 waiter"
/// 完全无效。心跳任务在两次 sleep 之间短暂离开 select! 的 cancel arm 时调用
/// notify_waiters 会丢信号。AtomicBool 是持久化标志，下次 sleep 醒来读到 true
/// 一定退出。
///
/// 单次心跳调用走 `tokio::time::timeout` 包一层，避免请求被 30s+ 慢响应卡住，
/// 进而错过取消窗口。
fn spawn_heartbeat(
    client: Arc<ApiClient>,
    worker_id: String,
    job_id: String,
    phase: Arc<RwLock<Phase>>,
    cancel: Arc<AtomicBool>,
    interval_sec: u64,
    stale_threshold_sec: u64,
) -> JoinHandle<()> {
    // clamp 心跳间隔 ≤ stale_threshold/2，留 buffer 给网络抖动
    let max_safe = stale_threshold_sec.saturating_div(2).max(1);
    let actual = interval_sec.min(max_safe).max(1);
    if actual != interval_sec {
        tracing::debug!(
            "[runner] heartbeat interval clamped {}s → {}s (stale_threshold={}s)",
            interval_sec,
            actual,
            stale_threshold_sec
        );
    }
    tokio::spawn(async move {
        let dur = Duration::from_secs(actual);
        // 心跳轮询：用粗粒度 sleep + 细粒度 cancel 检查双保险。
        // 把 dur 拆成 ≤500ms 的 tick，sleep 期间也能快速响应 cancel。
        let tick = Duration::from_millis(500);
        loop {
            // 等一个心跳周期，期间每 500ms 检查一次 cancel
            let mut waited = Duration::ZERO;
            while waited < dur {
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                let step = tick.min(dur - waited);
                tokio::time::sleep(step).await;
                waited += step;
            }
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let snap = phase.read().unwrap().clone();
            // 单次心跳给 10s 上限，避免被慢服务端卡到下一个心跳周期
            let send = client.heartbeat(&job_id, &worker_id, &snap.stage, snap.progress);
            match tokio::time::timeout(Duration::from_secs(10), send).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // 410 / job lost 时不再继续刷心跳——服务端已认定 ownership 失效
                    if matches!(e, crate::api::ApiError::JobLost) {
                        tracing::debug!(
                            "[runner] heartbeat got 410 (claimed_by cleared / job lost), \
                             stop heartbeating job={}",
                            job_id
                        );
                        return;
                    }
                    tracing::warn!("[runner] heartbeat failed: {e}");
                }
                Err(_) => {
                    tracing::warn!("[runner] heartbeat timed out (>10s), will retry next tick");
                }
            }
        }
    })
}
