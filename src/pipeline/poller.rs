// pipeline/poller.rs：audio worker 主轮询循环。
//
// 状态机（每轮迭代）：
//   1. 若 polling_paused → sleep 5s 直到取消暂停
//   2. 若 base_url / token 未配置 → sleep 10s（用户可能正在 Settings 页填写）
//   3. 若 not registered → 调 register 并上报 capability=["audio_extract"]；失败按退避 sleep 后重试
//   4. 若已满负载（running >= max_concurrent）→ sleep 2s 等槽位空出
//   5. 若本地未消费 FLAC ≥ audio_local_max_pending → sleep 30s 等 subtitle worker 拉走
//   6. claim 一次：
//        - 200 + job：spawn handle_job 到后台 → 立即 continue 尝试填下一个槽
//        - 204：sleep poll_interval
//        - 401/403：清 registered，sleep 退避（用户可能改了 token）
//        - 网络错误：sleep 退避

use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::api::{ApiClient, ApiError, ClaimedJob, RegisterResponse};
use crate::state::{CurrentTask, SharedState};

use super::{audio_owner, runner};

/// audio worker 自报的 capability。
pub const WORKER_CAPABILITIES: &[&str] = &["audio_extract"];

/// 本地暂存上限触发限流后，等待 subtitle worker 拉走 FLAC 的复检间隔。
const LOCAL_PENDING_RECHECK_SEC: u64 = 30;

/// poll loop 入口。一旦启动就永远跑，靠 polling_paused 软开关控制。
pub async fn run(state: SharedState) {
    tracing::info!("audio worker poll loop started");
    let mut consec_errors: u32 = 0;

    loop {
        // 1. 暂停态
        if state.is_polling_paused() {
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        // 2. 配置完整性
        let (base_url, token, verify_tls, poll_interval, error_backoff) = {
            let s = state.settings.read().expect("settings poisoned");
            (
                s.server.base_url.clone(),
                s.server.token.clone(),
                s.server.verify_tls,
                s.server.poll_interval_sec,
                s.server.error_backoff_sec,
            )
        };
        if base_url.is_empty() || token.is_empty() {
            sleep(Duration::from_secs(10)).await;
            continue;
        }

        // 3. 构造 client
        let client = match ApiClient::new(&base_url, &token, verify_tls) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("api client build failed: {e}");
                sleep(Duration::from_secs(10)).await;
                continue;
            }
        };

        // 4. 确保 registered
        if !state.is_registered() {
            match register_self(&state, &client).await {
                Ok(()) => consec_errors = 0,
                Err(e) => {
                    tracing::warn!("register failed: {e}");
                    consec_errors = consec_errors.saturating_add(1);
                    let backoff = backoff_seconds(error_backoff, consec_errors);
                    sleep(Duration::from_secs(backoff)).await;
                    continue;
                }
            }
        }

        // 5. 检查并发槽位
        let running = state.running_task_count();
        let max = state.max_concurrent() as usize;
        if running >= max {
            sleep(Duration::from_secs(2)).await;
            continue;
        }

        // 6. 检查本地暂存上限
        //
        // 防止 subtitle worker 处理速度跟不上 audio worker 时，audio_storage_dir 持续累积
        // FLAC 把磁盘打爆。阈值 0 = 不限流；> 0 时本地未消费 .flac 数 ≥ 阈值就跳过本轮 claim，
        // 等 audio_owner::fetch_loop 收到 cleanup 通知删掉若干文件后，下一轮自然恢复。
        //
        // 此检查在并发槽位检查之后：即使本地 pending 已满，正在跑的任务仍允许走完（FLAC 写
        // 入 storage_dir 是 pipeline 末段动作，不会被这里影响）。
        let max_pending = state
            .settings
            .read()
            .map(|s| s.pipeline.audio_local_max_pending)
            .unwrap_or(0);
        if max_pending > 0 {
            match audio_owner::resolve_storage_dir(&state) {
                Ok(storage_dir) => {
                    let pending = audio_owner::count_pending_entries(&storage_dir);
                    if pending >= max_pending as usize {
                        tracing::info!(
                            "[poller] local pending FLAC ({}) >= limit ({}); pausing claim for {}s",
                            pending,
                            max_pending,
                            LOCAL_PENDING_RECHECK_SEC
                        );
                        sleep(Duration::from_secs(LOCAL_PENDING_RECHECK_SEC)).await;
                        continue;
                    }
                }
                Err(e) => {
                    // app_data_dir 取不到属极端情况；记 warn 但不阻塞 claim，
                    // 让用户至少能正常工作（最坏情况退化为无限流，与设置 0 等价）
                    tracing::warn!("[poller] resolve audio_storage_dir failed: {e}; skip pending check");
                }
            }
        }

        // 7. claim（网络错误时重试，反向代理偶发丢包不应阻塞整个 worker）
        let worker_id = state.worker_id.read().unwrap().clone();
        let client_arc = Arc::new(client);
        let claim_result = claim_with_retry(&client_arc, &worker_id, 3).await;
        match claim_result {
            Ok(Some(job)) => {
                consec_errors = 0;

                // 关键：在 spawn 之前同步把 task 写入 current_tasks 表。
                //
                // 历史 bug：早期版本把 insert_task 放在 spawned handle_job 内，
                // 导致 poller continue 后立刻进入下一轮 step 5 读 running_task_count()
                // 时新 task 还没落表，计数偏小 → 触发 over-claim 或 under-claim。
                // 同步插入后 running_task_count 与实际并发任务数严格一致。
                let task = CurrentTask {
                    job_id: job.job_id.clone(),
                    media_id: job.media_id.clone(),
                    media_title: job.media_title.clone(),
                    stage: "queued".to_string(),
                    progress: 0,
                    started_at_ms: chrono::Utc::now().timestamp_millis(),
                };
                state.insert_task(task.clone());

                let state_clone = state.clone();
                let client_clone = client_arc.clone();
                let job_id_for_handle = job.job_id.clone();
                let join_handle = tokio::spawn(async move {
                    handle_job(&state_clone, client_clone, job).await;
                });
                // v4：把 spawned task 的 abort handle 注册到 state，
                // 让 HTTP handler 可以通过 POST /api/tasks/:job_id/cancel 中止此任务。
                state.register_running_handle(job_id_for_handle, join_handle.abort_handle());
                continue;
            }
            Ok(None) => {
                consec_errors = 0;
                sleep(Duration::from_secs(poll_interval)).await;
            }
            Err(ApiError::Unauthorized(msg)) => {
                tracing::warn!("claim unauthorized, will re-register: {msg}");
                state.set_registered(false);
                sleep(Duration::from_secs(error_backoff)).await;
            }
            Err(e) => {
                tracing::warn!("claim error: {e}");
                consec_errors = consec_errors.saturating_add(1);
                let backoff = backoff_seconds(error_backoff, consec_errors);
                sleep(Duration::from_secs(backoff)).await;
            }
        }
    }
}

/// claim 带网络错误重试。反向代理偶发丢包 / TLS 握手超时不应让 worker 整轮退避。
///
/// 仅对网络错误重试；业务错误（401/403/JobLost）和 204 无任务直接返回。
/// 每次重试间隔 2s，最多 `max_retries` 次。
async fn claim_with_retry(
    client: &ApiClient,
    worker_id: &str,
    max_retries: u8,
) -> Result<Option<ClaimedJob>, ApiError> {
    let mut last_err: Option<ApiError> = None;
    for attempt in 0..=max_retries {
        match client.claim(worker_id).await {
            Ok(result) => return Ok(result),
            Err(ApiError::Network(e)) => {
                tracing::warn!(
                    "[poller] claim network error (attempt {}/{}): {}",
                    attempt + 1,
                    max_retries + 1,
                    e
                );
                last_err = Some(ApiError::Network(e));
                if attempt < max_retries {
                    sleep(Duration::from_secs(2)).await;
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or(ApiError::NotConfigured))
}

async fn register_self(state: &SharedState, client: &ApiClient) -> Result<(), ApiError> {
    let (worker_id, name) = {
        let s = state.settings.read().unwrap();
        (s.worker_id.clone(), s.worker_name.clone())
    };
    if worker_id.is_empty() {
        return Err(ApiError::NotConfigured);
    }
    let resp = client
        .register(
            &worker_id,
            &name,
            env!("CARGO_PKG_VERSION"),
            "", // GPU 字段：audio worker 不依赖 GPU，留空
            WORKER_CAPABILITIES,
        )
        .await?;
    apply_register_response(state, &resp);
    Ok(())
}

/// 把 register 响应里的关键字段（stale_threshold / max_concurrent / worker_id）写回 state，
/// 并设置 `registered = true`。
///
/// 抽取成公共函数是为了让 poller 主循环 (register_self) 与 UI 上「注册到服务器」按钮
/// (commands::register_worker) 走同一套逻辑——历史 bug：UI 注册按钮只调
/// `set_registered(true)` 没调 `set_max_concurrent`，导致用户在 settings 把并发数调到
/// >1 然后手动点注册按钮，state 中的 max_concurrent 仍然停留在 AppState::new 的默认值 1，
/// 表现为「服务端有排队任务、并发配置 >1，audio 却只跑一个任务」。
pub fn apply_register_response(state: &SharedState, resp: &RegisterResponse) {
    {
        let mut t = state.stale_threshold_sec.write().unwrap();
        *t = resp.worker_stale_threshold;
    }

    // 并发数：服务端 > 0 时用服务端值，否则用本地设置
    let local_max = state.settings.read().unwrap().server.max_concurrent_tasks;
    let server_max = resp.max_concurrent_tasks;
    let effective = if server_max > 0 { server_max } else { local_max };
    state.set_max_concurrent(effective);

    // 同步保存服务端硬上限到 state，供前端 UI 通过 RuntimeStatus 拿到后
    // 解除「最大并发任务」输入框的硬编码 8 限制。注意保存的是原始值，
    // 0 = 服务端未下发，前端据此回退到不约束策略。
    state.set_server_max_concurrent(server_max);

    // 防御性校验：服务端接受的 capability 必须包含 audio_extract
    if !resp.accepted_capabilities.is_empty()
        && !resp
            .accepted_capabilities
            .iter()
            .any(|c| c == "audio_extract")
    {
        tracing::warn!(
            "[poller] server accepted_capabilities = {:?}, audio_extract NOT in list — token may be misconfigured",
            resp.accepted_capabilities
        );
    }
    tracing::info!(
        "registered to server, stale_threshold={}s, max_concurrent={} (server={}, local={}), accepted_caps={:?}",
        resp.worker_stale_threshold,
        effective,
        server_max,
        local_max,
        resp.accepted_capabilities,
    );

    if !resp.worker_id.is_empty() {
        *state.worker_id.write().unwrap() = resp.worker_id.clone();
    }
    state.set_registered(true);
}

/// 处理一个已 claim 的 audio_extract 任务。
///
/// 与字幕项目的差异：
///   - run_pipeline 返回 (size, sha256, duration_ms)，不再是 VTT 字节流
///   - 上传走 client.audio_complete()（在 runner 内部完成），handle_job 不再调 client.complete
///   - 失败时仍调 client.fail()，由服务端按 stage 决定回滚
async fn handle_job(state: &SharedState, client: Arc<ApiClient>, job: ClaimedJob) {
    let worker_id = state.worker_id.read().unwrap().clone();
    tracing::info!(
        "claimed job {} (media={}, stage={}, lang={}→{})",
        job.job_id,
        job.media_id,
        job.stage,
        job.source_lang,
        job.target_lang,
    );

    // 注：CurrentTask 已经由 poller::run 在 spawn 之前同步插入到 current_tasks 表，
    // 此处不再重复 insert_task / emit("worker://task-started")，避免与 poller 侧
    // running_task_count 形成 race。

    // 跑流水线（runner 内部直接调 audio_complete）
    let pipeline_result = runner::run_pipeline(state.clone(), client.clone(), job.clone()).await;

    match pipeline_result {
        Ok((size, sha, dur)) => {
            tracing::info!(
                "job {} audio_extract done: size={} sha={} dur_ms={}",
                job.job_id,
                size,
                &sha[..8.min(sha.len())],
                dur
            );
            let mut stats = state.stats.write().unwrap();
            stats.completed = stats.completed.saturating_add(1);
        }
        Err(e) => {
            let err_msg = format!("{e:#}");
            tracing::warn!("job {} pipeline failed: {err_msg}", job.job_id);
            // v4：按错误信息特征推断 errorKind，让服务端按重试 / 终止策略分流。
            // 推断规则保守——unknown/未匹配一律走 retriable。
            let kind = classify_audio_error(&err_msg);
            tracing::info!(
                "[runner] job {} fail kind={} (retriable={})",
                job.job_id,
                kind,
                !is_permanent_kind(kind)
            );
            match client
                .fail_with_kind(&job.job_id, &worker_id, &err_msg, kind)
                .await
            {
                Ok(()) | Err(ApiError::JobLost) => {}
                Err(e2) => tracing::warn!("fail report itself failed: {e2}"),
            }
            let mut stats = state.stats.write().unwrap();
            stats.failed = stats.failed.saturating_add(1);
            stats.last_error = Some(err_msg);
        }
    }

    // 从任务表移除
    state.remove_task(&job.job_id);
    // 清理 abort handle 注册（task 已结束，不再可取消）
    let _ = state.take_running_handle(&job.job_id);
}

/// 指数退避：每次 error 翻倍，封顶 60s。
fn backoff_seconds(base: u64, consec_errors: u32) -> u64 {
    let n = consec_errors.min(6); // 2^6 = 64
    let scaled = base.saturating_mul(1u64 << n);
    scaled.min(60)
}

/// classify_audio_error 把 anyhow 拼出来的错误字符串映射到服务端 ErrorKind 枚举。
///
/// audio worker pipeline 错误源主要是 m3u8DL-RE / ffmpeg / reqwest，错误信息里
/// 通常带可识别关键词。匹配不到一律返回 unknown（服务端按 retriable 处理）。
///
/// 完整 ErrorKind 列表见 m3u8-preview-go/internal/model/enum.go。
fn classify_audio_error(msg: &str) -> &'static str {
    let lower = msg.to_lowercase();
    // permanent
    if lower.contains("404") || lower.contains("not found") {
        return "audio_source_404";
    }
    if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized") {
        return "auth_invalid_token";
    }
    if lower.contains("invalid") && lower.contains("config") {
        return "config_invalid";
    }
    // retriable
    if lower.contains("timeout") || lower.contains("timed out") {
        return "network_timeout";
    }
    if lower.contains("502") || lower.contains("503") || lower.contains("504") {
        return "audio_source_temporary";
    }
    if lower.contains("connection") && (lower.contains("reset") || lower.contains("refused")) {
        return "network_timeout";
    }
    "unknown"
}

/// is_permanent_kind 与服务端 ClassifyErrorKind 保持同语义；仅供日志展示用。
fn is_permanent_kind(kind: &str) -> bool {
    matches!(
        kind,
        "auth_invalid_token"
            | "audio_source_404"
            | "flac_sha256_mismatch"
            | "whisper_model_missing"
            | "whisper_empty_transcription"
            | "translate_quota_exceeded"
            | "config_invalid"
    )
}
