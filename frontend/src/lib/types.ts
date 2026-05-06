// types.ts：与 Rust 端 commands.rs 输入输出 mirror（audio worker 版）。
//
// 不引入 codegen（避免链路过长）；改字段时记得两边同步。

export interface ServerSettings {
  base_url: string;
  token: string;
  poll_interval_sec: number;
  heartbeat_interval_sec: number;
  error_backoff_sec: number;
  verify_tls: boolean;
  /** 最大并发任务数（1=串行，>1=多任务并行） */
  max_concurrent_tasks: number;
}

/** audio pipeline 子配置：仅 ffmpeg / N_m3u8DL-RE 路径 + temp_dir + FLAC 参数。 */
export interface AudioPipelineSettings {
  m3u8dl_path: string;
  ffmpeg_path: string;
  /** 中间产物（mp4 切片 / wav）暂存目录；空 = 系统 Temp */
  temp_dir: string;
  /**
   * v3 broker 模式 FLAC 永久存储目录；空 = APPDATA/.../audio_artifacts/。
   * 任务进 DONE 后服务端通过 long-poll 通道通知 audio worker 自动清理对应 .flac + .json 索引。
   */
  audio_storage_dir: string;
  /** 中间音频格式枚举字符串：flac / opus_low / wav */
  intermediate_audio_format: 'flac' | 'opus_low' | 'wav';
  /** FLAC 压缩等级 0..=12，默认 8 */
  flac_compression_level: number;
  /** FLAC 编码超时（秒），默认 600 */
  flac_timeout_sec: number;
  /**
   * audio_storage_dir 中允许暂存的最大未消费 FLAC 数（默认 5）。
   * ≥ 此阈值时 poller 暂停 claim 新任务，等 subtitle worker 拉走若干个再恢复。
   * 0 = 不限流（仅受磁盘空间限制，不推荐）。
   */
  audio_local_max_pending: number;
}

export interface UiSettings {
  minimize_to_tray: boolean;
  autostart: boolean;
}

/** 网络设置：GitHub 代理 + M3U8 下载代理 */
export interface NetworkSettings {
  github_proxy_enabled: boolean;
  github_proxy_url: string;
  /** M3U8 流下载代理地址（HTTP/HTTPS/SOCKS5），仅用于 N_m3u8DL-RE 下载。留空 = 直连。 */
  download_proxy: string;
}

export interface Settings {
  server: ServerSettings;
  pipeline: AudioPipelineSettings;
  network: NetworkSettings;
  ui: UiSettings;
  worker_id: string;
  worker_name: string;
  /** Web 管理面板鉴权 token。空 = 不需要鉴权。 */
  web_auth_token: string;
}

export interface CurrentTask {
  job_id: string;
  media_id: string;
  media_title?: string | null;
  stage: string;
  progress: number;
  started_at_ms: number;
}

export interface RuntimeStats {
  completed: number;
  failed: number;
  last_error?: string | null;
}

export interface RuntimeStatus {
  registered: boolean;
  polling_paused: boolean;
  stale_threshold_sec: number;
  uptime_sec: number;
  current_tasks: CurrentTask[];
  max_concurrent_tasks: number;
  /**
   * 服务端在 register 响应中下发的最大并发任务数硬上限。
   * UI「最大并发任务」输入框的 max 直接来源于该字段（替代旧的硬编码 8）。
   * 0 = 尚未注册或服务端未下发该字段，UI 此时不约束 max。
   */
  server_max_concurrent_tasks: number;
  stats: RuntimeStats;
  /** audio worker 自报 capability，固定 ["audio_extract"] */
  capabilities: string[];
  /** 已完成处理、暂存等待 subtitle worker 拉走的本地 FLAC 数量 */
  audio_local_pending: number;
  /** 本地暂存上限阈值；0 = 不限流，> 0 时达到该值后 poller 暂停 claim 新任务 */
  audio_local_max_pending: number;
  /** 解析后的 audio_storage_dir 绝对路径（Settings 里的「音频保存目录」），空字符串表示解析失败 */
  audio_storage_dir: string;
}

export interface PingResult {
  ok: boolean;
  message: string;
}

export interface ValidateDirResult {
  ok: boolean;
  message: string;
  /** 实际生效路径（输入为空时回填系统 Temp） */
  resolved_path: string;
}

export interface RegisterResponse {
  workerId: string;
  serverTime: number;
  workerStaleThreshold: number;
  maxConcurrentTasks: number;
  /** v2 服务端实际接受的 capability 集合 */
  acceptedCapabilities: string[];
}

export interface LogEntry {
  ts: number;
  level: string;
  target: string;
  message: string;
}

// Doctor 探测结果（与 Rust 端 doctor.rs ToolInfo / DoctorReport mirror）
export interface ToolInfo {
  key: string;
  label: string;
  path: string;
  found: boolean;
  version: string;
  backend: string;
  error: string;
  hint?: string;
}

/** audio worker 只需要 N_m3u8DL-RE + ffmpeg。 */
export interface DoctorReport {
  m3u8dl: ToolInfo;
  ffmpeg: ToolInfo;
}

// === 任务历史 ===

export interface StageRecord {
  stage: string;
  start_ms: number;
  end_ms: number;
}

export interface TaskHistorySummary {
  job_id: string;
  media_id: string;
  media_title: string | null;
  source_lang: string;
  target_lang: string;
  started_at: number;
  finished_at: number | null;
  /** running / done / failed */
  status: string;
  error_msg: string | null;
  /** audio worker 这里是 "audio_extract"（占位，标识来自 audio worker） */
  asr_model: string | null;
  /** audio worker 不计算字幕段，固定 0 */
  segment_count: number | null;
}

export interface TaskHistoryRow {
  job_id: string;
  media_id: string;
  media_title: string | null;
  source_lang: string;
  target_lang: string;
  started_at: number;
  finished_at: number | null;
  status: string;
  error_msg: string | null;
  stages: StageRecord[];
  asr_model: string | null;
  /** audio worker 这里是 audio format 字符串（"flac" / "opus_24k" / "wav"） */
  mt_model: string | null;
  segment_count: number | null;
  /** audio worker 这里是 FLAC 文件大小（字节） */
  vtt_size: number | null;
  asr_preview: string[];
  mt_preview: string[];
}

// === Bootstrap（audio worker 不再下载 CUDA / silero，但保留类型给 UI） ===

export interface BootstrapStatus {
  /** app_data_dir/binaries 实际路径，用于 UI 展示 */
  binariesDir: string;
  /** true = 必需小件齐全 */
  ready: boolean;
  /** 缺失的二进制清单（文件名） */
  missing: string[];
  /** 兼容字段：audio worker 永远 false */
  needCuda: boolean;
  needSilero: boolean;
}

export interface BootstrapProgress {
  task: string;
  bytesDone: number;
  bytesTotal: number;
  message: string;
  finished: boolean;
  error: string;
}
