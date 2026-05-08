// client.rs：HTTP 客户端，对接 m3u8-preview-go 的 /api/v1/worker/* 端点（audio worker 版）。
//
// 协议契约：见 m3u8-preview-go/docs/worker-protocol.md
//
// 设计要点：
//   - 每次请求重新构建 reqwest::Client（避免 base_url / token / verify_tls 改动后没生效）
//   - 区分「业务错误」(ApiError 子类) 和「网络错误」(reqwest::Error)，让上层好做退避
//   - 410 Gone 单独识别为 JobLost，让 pipeline 立刻放弃当前任务回到 idle
//
// audio worker 与字幕项目的差异：
//   - register 上报 capability=["audio_extract"]
//   - 不上传 VTT；用 audio-ready 注册 FLAC 元数据（v3 broker 模式，文件留本地）
//   - 通过 audio_fetch_poll long-poll 接收 fetch / cleanup 指令
//   - 收到 fetch 时通过 audio_stream 把本地 FLAC 流式推到服务端 broker

use std::path::Path;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid response: {0}")]
    Decode(String),

    #[error("server returned {status}: {body}")]
    Server { status: u16, body: String },

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("job lost (410 Gone)")]
    JobLost,

    #[error("base_url or token not configured")]
    NotConfigured,
}

/// 服务端 /worker/register 响应体。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegisterResponse {
    #[serde(rename = "workerId")]
    pub worker_id: String,
    #[serde(rename = "serverTime")]
    pub server_time: i64,
    #[serde(rename = "workerStaleThreshold")]
    /// 服务端容忍的心跳间隔（秒）
    pub worker_stale_threshold: u64,
    /// 服务端分配给此 worker 的最大并发任务数。
    /// 未提供时 serde 回退到 0（poller 侧会 fallback 到本地设置）。
    #[serde(rename = "maxConcurrentTasks", default)]
    pub max_concurrent_tasks: u32,
    /// v2：服务端实际接受的 capability 集合，客户端可据此 sanity check。
    #[serde(rename = "acceptedCapabilities", default)]
    pub accepted_capabilities: Vec<String>,
}

/// 服务端 /worker/claim 200 响应中的任务 payload。
///
/// v2 分布式拆分后字段含义：
///   - stage = "audio_extract"  → audio worker 派活，使用 m3u8_url + headers 自行下载
///   - stage = "asr_subtitle"   → subtitle worker 派活，使用 audio_artifact_url 拉 FLAC
///
/// audio worker 只关心 audio_extract 分支；subtitle 字段保留为 Option 仅做防御性反序列化。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaimedJob {
    #[serde(rename = "jobId")]
    pub job_id: String,
    #[serde(rename = "mediaId")]
    pub media_id: String,
    #[serde(rename = "mediaTitle")]
    #[serde(default)]
    pub media_title: Option<String>,
    /// v2 新增：派活类型
    #[serde(default)]
    pub stage: String,

    // audio_extract 阶段使用：
    #[serde(rename = "m3u8Url")]
    #[serde(default)]
    pub m3u8_url: Option<String>,
    /// 服务端按 m3u8 URL 域名注入的下载请求头（User-Agent / Referer 等）。
    /// worker 直连源站时把这些转成 N_m3u8DL-RE 的 --header 参数，避免 403。
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,

    // asr_subtitle 阶段使用（audio worker 不应收到，保留字段做防御性解析）：
    #[serde(rename = "audioArtifactUrl", default)]
    pub audio_artifact_url: Option<String>,
    #[serde(rename = "audioArtifactSize", default)]
    pub audio_artifact_size: Option<i64>,
    #[serde(rename = "audioArtifactSha256", default)]
    pub audio_artifact_sha256: Option<String>,
    #[serde(rename = "audioArtifactFormat", default)]
    pub audio_artifact_format: Option<String>,
    #[serde(rename = "audioArtifactDurationMs", default)]
    pub audio_artifact_duration_ms: Option<i64>,

    #[serde(rename = "sourceLang")]
    pub source_lang: String,
    #[serde(rename = "targetLang")]
    pub target_lang: String,

    /// v4 重试调度信息：让 worker 在日志/UI 上能展示"第 X/N 次重试"。
    #[serde(default)]
    pub attempt: u32,
    #[serde(rename = "maxAttempts", default)]
    pub max_attempts: u32,
}

/// audio_ready 时携带的元数据。
///
/// v3 broker 模式下，audio worker 不再上传文件，只把这些元数据通过 audio-ready 端点
/// 注册到服务端。subtitle worker 拉取时从这里拿 expected_sha256 / size 自校验。
///
/// 类型保留 `AudioCompleteMeta` 名以兼容内部调用方；语义已等同于"audio-ready 元数据"。
#[derive(Debug, Clone, Serialize)]
pub struct AudioCompleteMeta {
    #[serde(rename = "workerId")]
    pub worker_id: String,
    pub size: i64,
    pub sha256: String,
    pub format: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
}

/// audio worker long-poll 拿到的指令。
///
/// Action 取值：
///   - "fetch"  ：subtitle worker 在等 jobId 的 FLAC，请上传
///   - "cleanup"：任务已完成，请删除本地 jobId.flac + 索引项
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioFetchTask {
    pub action: String,
    #[serde(rename = "jobId")]
    pub job_id: String,
}

/// 后端通用响应包装：{success, data?, message?, code?}
///
/// 显式指定 `bound`，避免 derive 默认在泛型 T 上加 `Default` 约束。
/// Option<T> / Option<String> 在字段缺失时自然 fallback 到 None，不需要 `#[serde(default)]`。
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct ApiEnvelope<T> {
    success: bool,
    data: Option<T>,
    message: Option<String>,
    #[allow(dead_code)]
    code: Option<String>,
}

/// HTTP 客户端，每次请求时即时构造（base_url / token 可能动态变更）。
pub struct ApiClient {
    base_url: String,
    token: String,
    http: Client,
}

impl ApiClient {
    pub fn new(base_url: &str, token: &str, verify_tls: bool) -> Result<Self, ApiError> {
        Self::new_with_proxy(base_url, token, verify_tls, "")
    }

    /// 带可选代理的构造器。proxy_url 为空时直连。
    pub fn new_with_proxy(
        base_url: &str,
        token: &str,
        verify_tls: bool,
        proxy_url: &str,
    ) -> Result<Self, ApiError> {
        if base_url.trim().is_empty() || token.trim().is_empty() {
            return Err(ApiError::NotConfigured);
        }
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("M3u8PreviewAudioWorker/", env!("CARGO_PKG_VERSION")));
        if !verify_tls {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let trimmed_proxy = proxy_url.trim();
        if !trimmed_proxy.is_empty() {
            match reqwest::Proxy::all(trimmed_proxy) {
                Ok(proxy) => {
                    builder = builder.proxy(proxy);
                    tracing::info!("[api] using proxy: {}", trimmed_proxy);
                }
                Err(e) => {
                    tracing::warn!(
                        "[api] invalid proxy URL '{}': {}; falling back to direct connection",
                        trimmed_proxy,
                        e
                    );
                }
            }
        }
        let http = builder.build()?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// GET /healthz —— 仅用于 Settings 页的「测试连接」按钮。
    pub async fn ping(&self) -> Result<(), ApiError> {
        let resp = self.http.get(self.url("/healthz")).send().await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ApiError::Server {
                status: resp.status().as_u16(),
                body: resp.text().await.unwrap_or_default(),
            })
        }
    }

    /// register：v2 新增 capabilities 参数（audio worker 传 &["audio_extract"]）
    pub async fn register(
        &self,
        worker_id: &str,
        name: &str,
        version: &str,
        gpu: &str,
        capabilities: &[&str],
    ) -> Result<RegisterResponse, ApiError> {
        let body = serde_json::json!({
            "workerId": worker_id,
            "name": name,
            "version": version,
            "gpu": gpu,
            "capabilities": capabilities,
        });
        let resp = self
            .http
            .post(self.url("/api/v1/worker/register"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        decode_envelope(resp).await
    }

    /// claim：返回 Some(job) 或 None（204 No Content）。
    ///
    /// v4：默认走 long-poll（waitSec=25），让服务端 hold 至有任务才返回。
    /// poller 仍按"返回 None 后 sleep poll_interval"的逻辑跑，长轮询只是把无活
    /// 期的轮询请求合并掉，对调用方完全透明。
    pub async fn claim(&self, worker_id: &str) -> Result<Option<ClaimedJob>, ApiError> {
        self.claim_with_wait(worker_id, 25).await
    }

    /// claim_with_wait 是 claim 的可控 wait 版本。wait_sec=0 退回到短轮询语义。
    pub async fn claim_with_wait(
        &self,
        worker_id: &str,
        wait_sec: u32,
    ) -> Result<Option<ClaimedJob>, ApiError> {
        let body = serde_json::json!({
            "workerId": worker_id,
            "waitSec": wait_sec,
        });
        // HTTP 客户端超时 = wait_sec + 5s 容错；wait_sec=0 时仍给 60s 默认上限
        let req_timeout = if wait_sec == 0 {
            60
        } else {
            (wait_sec + 5) as u64
        };
        let resp = self
            .http
            .post(self.url("/api/v1/worker/claim"))
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(Duration::from_secs(req_timeout))
            .send()
            .await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let job: ClaimedJob = decode_envelope(resp).await?;
        Ok(Some(job))
    }

    pub async fn heartbeat(
        &self,
        job_id: &str,
        worker_id: &str,
        stage: &str,
        progress: u8,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({
            "workerId": worker_id,
            "stage": stage,
            "progress": progress,
        });
        let resp = self
            .http
            .post(self.url(&format!(
                "/api/v1/worker/jobs/{}/heartbeat",
                urlencoding::encode_path(job_id)
            )))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        check_no_data(resp).await
    }

    pub async fn fail(
        &self,
        job_id: &str,
        worker_id: &str,
        err_msg: &str,
    ) -> Result<(), ApiError> {
        self.fail_with_kind(job_id, worker_id, err_msg, "").await
    }

    /// fail_with_kind 是 fail 的扩展版本，附带 errorKind 让服务端按重试策略分流。
    /// kind 见服务端 model.ErrorKind*；空字符串 = 服务端按 unknown / retriable 处理。
    pub async fn fail_with_kind(
        &self,
        job_id: &str,
        worker_id: &str,
        err_msg: &str,
        error_kind: &str,
    ) -> Result<(), ApiError> {
        // err_msg 截断到 2000 字符（服务端会截，但客户端先截可省字节）
        let truncated: String = err_msg.chars().take(2000).collect();
        let body = serde_json::json!({
            "workerId": worker_id,
            "errorMsg": truncated,
            "errorKind": error_kind,
        });
        let resp = self
            .http
            .post(self.url(&format!(
                "/api/v1/worker/jobs/{}/fail",
                urlencoding::encode_path(job_id)
            )))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        check_no_data(resp).await
    }

    /// deregister 优雅下线端点（v4）。worker 关闭前调用，服务端把它持有的 RUNNING
    /// 任务按 ErrorKindWorkerShutdown（neutral 路径，attempt 不增）回滚，避免等
    /// 30s stale recovery。失败仅记录，不阻塞关闭流程。
    pub async fn deregister(&self, worker_id: &str) -> Result<(), ApiError> {
        let body = serde_json::json!({ "workerId": worker_id });
        let resp = self
            .http
            .post(self.url("/api/v1/worker/deregister"))
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        check_no_data(resp).await
    }

    /// audio_ready 通知服务端：本地 FLAC 已就绪，仅注册元数据。
    ///
    /// 与 v2 audio_complete 的差别：不再上传文件 body，只发 JSON。
    /// 对应服务端：POST /api/v1/worker/jobs/:jobId/audio-ready
    pub async fn audio_ready(
        &self,
        job_id: &str,
        meta: &AudioCompleteMeta,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({
            "workerId": meta.worker_id,
            "size": meta.size,
            "sha256": meta.sha256,
            "format": meta.format,
            "durationMs": meta.duration_ms,
        });
        let resp = self
            .http
            .post(self.url(&format!(
                "/api/v1/worker/jobs/{}/audio-ready",
                urlencoding::encode_path(job_id)
            )))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        check_no_data(resp).await
    }

    /// audio_lost 通知服务端：本地 FLAC 已丢失（fetch 时找不到）。
    ///
    /// 服务端会把 audio_worker_id / claimed_by / subtitle_worker_id 全部清空、
    /// stage 回 queued、清空音频元数据，让任意 audio worker 重新跑这条任务。
    /// 否则 broker 会反复派发同一个 fetch task 死循环。
    ///
    /// 对应服务端：POST /api/v1/worker/jobs/:jobId/audio-lost  (v3.1)
    /// 错误码：
    ///   - 410 `WORKER_AUDIO_LOST_NOT_OWNED`：调用方不是当前 audio_worker_id
    ///   - 409 `WORKER_AUDIO_NOT_READY`：stage 不在允许集合中
    ///     （允许：audio_uploaded / asr / translate / writing；
    ///     fetch 派发瞬间 stage 通常是 asr，因为 subtitle worker claim 后已推进）
    pub async fn audio_lost(
        &self,
        job_id: &str,
        worker_id: &str,
        err_msg: &str,
    ) -> Result<(), ApiError> {
        let truncated: String = err_msg.chars().take(2000).collect();
        let body = serde_json::json!({
            "workerId": worker_id,
            "errorMsg": truncated,
        });
        let resp = self
            .http
            .post(self.url(&format!(
                "/api/v1/worker/jobs/{}/audio-lost",
                urlencoding::encode_path(job_id)
            )))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        check_no_data(resp).await
    }

    /// audio_fetch_poll long-poll：等待服务端下发 fetch / cleanup 指令。
    ///
    /// 服务端最多 hold timeout_sec 秒；超时返回 204 让客户端立即重新 poll。
    /// 返回 Some(task) 或 None（无任务，需要立即重新 poll）。
    pub async fn audio_fetch_poll(
        &self,
        worker_id: &str,
        timeout_sec: u32,
    ) -> Result<Option<AudioFetchTask>, ApiError> {
        let body = serde_json::json!({
            "workerId": worker_id,
            "timeoutSec": timeout_sec,
        });
        // HTTP 客户端超时给服务端 hold 时间 + 5s 容错
        let client_timeout = std::cmp::max(timeout_sec, 5).saturating_add(5);
        let resp = self
            .http
            .post(self.url("/api/v1/worker/audio-fetch-poll"))
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(Duration::from_secs(client_timeout as u64))
            .send()
            .await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        // 503 Service Unavailable：服务端 audio-fetch 超时（audio worker 未在 30s 内
        // 推流 FLAC）。视为无任务，让 fetch_loop 立即重新 poll，而非上抛错误触发退避。
        if resp.status() == StatusCode::SERVICE_UNAVAILABLE {
            tracing::debug!(
                "[api] audio_fetch_poll got 503 (server timeout), treating as no-task"
            );
            return Ok(None);
        }
        let task: AudioFetchTask = decode_envelope(resp).await?;
        Ok(Some(task))
    }

    /// audio_stream 收到 fetch 通知后，把本地 FLAC 文件流式推到服务端（v3 单流模式）。
    ///
    /// 服务端把 body 实时 io.Copy 到等待中的 subtitle worker GET response。
    /// 头部：X-Worker-Id 必填；Content-Length 提示服务端预期大小。
    ///
    /// 注意：单次 POST body 受中间路径限制（Cloudflare Free/Pro 100 MiB）。
    /// 走 CDN 时必须用 audio_stream_chunked；仅内网或直连源站时可用本方法。
    /// 当前默认走 chunked，本方法作为内网部署 fallback 保留。
    #[allow(dead_code)]
    pub async fn audio_stream(
        &self,
        job_id: &str,
        worker_id: &str,
        flac_path: &Path,
    ) -> Result<(), ApiError> {
        let metadata = std::fs::metadata(flac_path)?;
        let total_len = metadata.len();
        let file = tokio::fs::File::open(flac_path).await?;
        let stream = ReaderStream::with_capacity(file, 64 * 1024);
        let body = reqwest::Body::wrap_stream(stream);

        let resp = self
            .http
            .post(self.url(&format!(
                "/api/v1/worker/jobs/{}/audio-stream",
                urlencoding::encode_path(job_id)
            )))
            .bearer_auth(&self.token)
            .header("X-Worker-Id", worker_id)
            .header("Content-Type", "audio/flac")
            .header("Content-Length", total_len.to_string())
            .body(body)
            // 大文件上传给足时间（10 分钟），覆盖 ~50MB FLAC 在百兆带宽下的最坏耗时
            .timeout(Duration::from_secs(600))
            .send()
            .await?;
        check_no_data(resp).await
    }

    /// audio_stream_chunked v3.2 分块上传：把本地 FLAC 切成多个 ≤90 MiB 的 chunk
    /// 顺序 POST，绕开 Cloudflare 等 CDN 的 100 MiB body 上限。
    ///
    /// 服务端 broker 在同一条 io.Pipe 上拼接所有 chunk，subtitle worker GET 端
    /// 仍然看到一条连续的 chunked 流，无感知。
    ///
    /// 协议：
    ///   - X-Chunk-Index：0-based 严格递增
    ///   - X-Chunk-Last：仅最后一块设置为 "1"
    ///   - 任意 chunk 失败 → 整段失败（服务端会向 subtitle worker GET 透传错误）
    ///
    /// 每块用 file.seek + take(N) 流式发送，内存占用与单流模式一致（64 KiB 缓冲）。
    pub async fn audio_stream_chunked(
        &self,
        job_id: &str,
        worker_id: &str,
        flac_path: &Path,
    ) -> Result<(), ApiError> {
        // 90 MiB；与服务端 nginx client_max_body_size 95m / Go maxAudioStreamChunkBytes
        // 95 MiB 对齐，保留 5 MiB 给 headers / 编码膨胀
        const CHUNK_SIZE: u64 = 90 * 1024 * 1024;

        let metadata = std::fs::metadata(flac_path)?;
        let total_len = metadata.len();
        if total_len == 0 {
            return Err(ApiError::Server {
                status: 0,
                body: "flac file is empty".into(),
            });
        }

        let mut offset: u64 = 0;
        let mut idx: u32 = 0;
        loop {
            let remaining = total_len - offset;
            let this_chunk = remaining.min(CHUNK_SIZE);
            let is_last = offset + this_chunk >= total_len;

            // v4：单块网络抖动时本地重试 1 次。chunked 协议要求 X-Chunk-Index 严格递增，
            // 服务端 broker 在 abortCoupling 时会向 subtitle worker GET 透传错误，因此
            // 重试只对"chunk 还没真正写入 broker pipe"的连接级失败有意义（reqwest 抛
            // Network 错误时几乎都属此类）。已经被服务端接收的 chunk 不会重传 —— broker
            // 端的 nextChunkIdx 会校验顺序避免重复拼接。
            let mut last_err: Option<ApiError> = None;
            for try_idx in 0..2u8 {
                // 每块独立 open + seek，让 ReaderStream 拿到 owned File（满足 Body::wrap_stream
                // 'static + Send 约束）。tokio fs 句柄轻量，这点开销可忽略。
                let mut file = tokio::fs::File::open(flac_path).await?;
                file.seek(SeekFrom::Start(offset)).await?;
                let take = file.take(this_chunk);
                let stream = ReaderStream::with_capacity(take, 64 * 1024);
                let body = reqwest::Body::wrap_stream(stream);

                let send_result = self
                    .http
                    .post(self.url(&format!(
                        "/api/v1/worker/jobs/{}/audio-stream-chunk",
                        urlencoding::encode_path(job_id)
                    )))
                    .bearer_auth(&self.token)
                    .header("X-Worker-Id", worker_id)
                    .header("X-Chunk-Index", idx.to_string())
                    .header("X-Chunk-Last", if is_last { "1" } else { "0" })
                    .header("Content-Type", "application/octet-stream")
                    .header("Content-Length", this_chunk.to_string())
                    .body(body)
                    // 单 chunk ≤ 90 MiB，2 分钟覆盖百兆带宽最坏耗时
                    .timeout(Duration::from_secs(120))
                    .send()
                    .await;

                match send_result {
                    Ok(resp) => match check_no_data(resp).await {
                        Ok(()) => {
                            last_err = None;
                            break;
                        }
                        Err(ApiError::Server { status, .. }) if status == 409 => {
                            // 409 chunk 乱序：服务端已经把这条 fetch coupling abort 了，
                            // 重试同一 idx 没用 —— 上抛让 runner 走失败路径
                            return Err(ApiError::Server {
                                status,
                                body: format!(
                                    "chunk {} rejected as out-of-order (broker aborted fetch)",
                                    idx
                                ),
                            });
                        }
                        Err(ApiError::Server { status, body })
                            if (500..600).contains(&status) =>
                        {
                            // 5xx 视为可重试
                            last_err = Some(ApiError::Server { status, body });
                            if try_idx == 0 {
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                continue;
                            }
                            break;
                        }
                        Err(e) => return Err(e), // 4xx / Unauthorized 等不重试
                    },
                    Err(e) => {
                        // reqwest 网络 / 超时错误 → 可重试
                        last_err = Some(ApiError::from(e));
                        if try_idx == 0 {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                        break;
                    }
                }
            }
            if let Some(err) = last_err {
                return Err(err);
            }

            offset += this_chunk;
            idx += 1;
            if is_last {
                break;
            }
        }
        Ok(())
    }

    /// retry：让服务端把指定 mediaId 的字幕任务重置为 PENDING。
    /// 服务端复用 admin Retry 逻辑（不存在则按 EnsureJob 创建）。
    pub async fn retry(&self, media_id: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .post(self.url(&format!(
                "/api/v1/worker/media/{}/retry",
                urlencoding::encode_path(media_id)
            )))
            .bearer_auth(&self.token)
            .send()
            .await?;
        check_no_data(resp).await
    }
}

async fn decode_envelope<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<T, ApiError> {
    let status = resp.status();
    if status == StatusCode::GONE {
        return Err(ApiError::JobLost);
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::Unauthorized(body));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::Server {
            status: status.as_u16(),
            body,
        });
    }
    let env: ApiEnvelope<T> = resp
        .json()
        .await
        .map_err(|e| ApiError::Decode(e.to_string()))?;
    if !env.success {
        return Err(ApiError::Server {
            status: status.as_u16(),
            body: env.message.unwrap_or_else(|| "unknown error".to_string()),
        });
    }
    env.data.ok_or_else(|| ApiError::Decode("missing data field".to_string()))
}

async fn check_no_data(resp: reqwest::Response) -> Result<(), ApiError> {
    let status = resp.status();
    if status == StatusCode::GONE {
        return Err(ApiError::JobLost);
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ApiError::Unauthorized(body));
        }
        return Err(ApiError::Server {
            status: status.as_u16(),
            body,
        });
    }
    Ok(())
}

// 极简内联 path-segment 编码，避免再拉一个 urlencoding crate 依赖。
mod urlencoding {
    pub fn encode_path(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            // RFC 3986 unreserved + 一些常见安全字符
            if matches!(
                b,
                b'A'..=b'Z'
                    | b'a'..=b'z'
                    | b'0'..=b'9'
                    | b'-'
                    | b'_'
                    | b'.'
                    | b'~'
            ) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{:02X}", b));
            }
        }
        out
    }
}
