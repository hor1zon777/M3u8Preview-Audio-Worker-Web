// api.ts：HTTP API 客户端（替代 Tauri IPC invoke）。
//
// 每个函数一一对应原 tauri.ts 的 invoke 包装，签名保持一致，页面组件无需改动。

import type {
  Settings,
  RuntimeStatus,
  PingResult,
  ValidateDirResult,
  RegisterResponse,
  LogEntry,
  DoctorReport,
  TaskHistorySummary,
  TaskHistoryRow,
} from './types';

const BASE = import.meta.env.VITE_API_BASE ?? '';

/** 通用 API 封装：解析 {success, data, message} 信封 */
async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(`${BASE}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...init,
  });
  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`${resp.status}: ${text}`);
  }
  const envelope = await resp.json();
  if (!envelope.success) {
    throw new Error(envelope.message ?? 'unknown error');
  }
  return envelope.data as T;
}

// ---- Settings ----

export async function getSettings(): Promise<Settings> {
  return api<Settings>('/api/settings');
}

export async function saveSettings(settings: Settings): Promise<void> {
  await api<void>('/api/settings', {
    method: 'PUT',
    body: JSON.stringify(settings),
  });
}

// ---- 连接 / 注册 ----

export async function testConnection(): Promise<PingResult> {
  return api<PingResult>('/api/ping', { method: 'POST' });
}

export async function registerWorker(): Promise<RegisterResponse> {
  return api<RegisterResponse>('/api/register', { method: 'POST' });
}

// ---- 运行时状态 ----

export async function getRuntimeStatus(): Promise<RuntimeStatus> {
  return api<RuntimeStatus>('/api/status');
}

export async function getRecentLogs(limit = 200): Promise<LogEntry[]> {
  return api<LogEntry[]>(`/api/logs?limit=${limit}`);
}

export async function pausePolling(): Promise<void> {
  await api<void>('/api/pause', { method: 'POST' });
}

export async function resumePolling(): Promise<void> {
  await api<void>('/api/resume', { method: 'POST' });
}

// ---- Doctor ----

export async function doctorProbe(): Promise<DoctorReport> {
  return api<DoctorReport>('/api/doctor');
}

// ---- 目录校验 ----

export async function validateTempDir(path: string): Promise<ValidateDirResult> {
  return api<ValidateDirResult>('/api/validate-dir', {
    method: 'POST',
    body: JSON.stringify({ path }),
  });
}

// openTempDir 在 Web 版不可用（无本地文件系统），保留空壳兼容 import
export async function openTempDir(_path: string): Promise<string> {
  throw new Error('Web 版不支持打开本地目录');
}

// ---- Bootstrap（Web 版不适用，保留空壳兼容） ----

export interface BootstrapStatus {
  binariesDir: string;
  ready: boolean;
  missing: string[];
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

export async function bootstrapStatus(): Promise<BootstrapStatus> {
  return { binariesDir: '', ready: true, missing: [], needCuda: false, needSilero: false };
}

export async function bootstrapDownload(_cuda: boolean, _silero: boolean): Promise<void> {
  // no-op in web version
}

export async function bootstrapProgressSnapshot(): Promise<BootstrapProgress | null> {
  return null;
}

export function onBootstrapProgress(_cb: (p: BootstrapProgress) => void): () => void {
  return () => {}; // no-op
}

// ---- 任务历史 ----

export async function listTaskHistory(
  limit = 50,
  offset = 0,
): Promise<TaskHistorySummary[]> {
  return api<TaskHistorySummary[]>(`/api/history?limit=${limit}&offset=${offset}`);
}

export async function getTaskHistory(
  jobId: string,
): Promise<TaskHistoryRow | null> {
  return api<TaskHistoryRow | null>(`/api/history/${encodeURIComponent(jobId)}`);
}

export async function clearTaskHistory(keepRecent = 0): Promise<number> {
  return api<number>(`/api/history?keep_recent=${keepRecent}`, { method: 'DELETE' });
}

export async function retrySubtitleJob(mediaId: string): Promise<void> {
  await api<void>(`/api/retry/${encodeURIComponent(mediaId)}`, { method: 'POST' });
}

// ---- WebSocket 日志流 ----

export function connectLogStream(onMessage: (entry: LogEntry) => void): WebSocket {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(`${proto}//${location.host}/api/ws/logs`);
  ws.onmessage = (e) => {
    try {
      onMessage(JSON.parse(e.data));
    } catch {
      // ignore parse errors
    }
  };
  return ws;
}

// ---- 事件订阅（Web 版无 Tauri event，保留空壳兼容） ----

type UnlistenFn = () => void;

export function onWorkerEvent<T = unknown>(
  _name: string,
  _cb: (payload: T) => void,
): Promise<UnlistenFn> {
  // Web 版通过轮询 getRuntimeStatus 获取状态变化，不需要事件订阅
  return Promise.resolve(() => {});
}
