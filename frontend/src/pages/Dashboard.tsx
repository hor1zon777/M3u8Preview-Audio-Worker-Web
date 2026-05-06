import { useQuery } from '@tanstack/react-query';
import {
  CheckCircle2,
  XCircle,
  Loader2,
  PauseCircle,
  PlayCircle,
  Server,
  Cpu,
  Clock,
  Activity,
  HardDrive,
} from 'lucide-react';
import {
  getRuntimeStatus,
  getSettings,
  pausePolling,
  resumePolling,
} from '../lib/api';

/**
 * Dashboard：worker 整体运行状态。
 * 卡片：连接状态 / 暂停开关 / 当前任务 / 累计统计。
 */
export function Dashboard() {
  const { data: status, refetch } = useQuery({
    queryKey: ['runtime-status'],
    queryFn: getRuntimeStatus,
    refetchInterval: 2000,
  });

  const { data: settings } = useQuery({
    queryKey: ['settings'],
    queryFn: getSettings,
    staleTime: 30_000,
  });

  const togglePause = async () => {
    if (status?.polling_paused) {
      await resumePolling();
    } else {
      await pausePolling();
    }
    refetch();
  };

  const baseUrl = settings?.server.base_url ?? '';
  const tokenSet = !!settings?.server.token;
  const configured = !!baseUrl && tokenSet;

  return (
    <div className="px-6 py-5 max-w-5xl mx-auto space-y-4">
      {/* 状态摘要 */}
      <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
        <Card title="服务器" icon={<Server className="w-4 h-4" />}>
          {!configured ? (
            <StatusLine icon="warn" text="未配置 base_url 或 token" />
          ) : status?.registered ? (
            <StatusLine icon="ok" text="已注册" sub={baseUrl} />
          ) : (
            <StatusLine icon="warn" text="等待注册…" sub={baseUrl} />
          )}
        </Card>

        <Card title="轮询" icon={<Activity className="w-4 h-4" />}>
          {status?.polling_paused ? (
            <StatusLine icon="paused" text="已暂停" />
          ) : (
            <StatusLine icon="ok" text={`运行中（间隔 ${settings?.server.poll_interval_sec ?? 5}s）`} />
          )}
          <button onClick={togglePause} className="btn-secondary mt-3 text-xs">
            {status?.polling_paused ? (
              <>
                <PlayCircle className="w-3.5 h-3.5" /> 继续
              </>
            ) : (
              <>
                <PauseCircle className="w-3.5 h-3.5" /> 暂停
              </>
            )}
          </button>
        </Card>

        <Card title="运行时长" icon={<Clock className="w-4 h-4" />}>
          <div className="text-2xl text-white tabular-nums">
            {formatUptime(status?.uptime_sec ?? 0)}
          </div>
          <div className="text-xs text-emby-text-muted mt-1">
            心跳超时阈值 {status?.stale_threshold_sec ?? 600}s
          </div>
        </Card>
      </div>

      {/* 当前任务 */}
      <Card
        title={`当前任务${status?.current_tasks?.length ? ` (${status.current_tasks.length}/${status.max_concurrent_tasks})` : ''}`}
        icon={<Cpu className="w-4 h-4 text-blue-400" />}
      >
        {status?.current_tasks && status.current_tasks.length > 0 ? (
          <div className="space-y-4">
            {status.current_tasks.map((task) => (
              <div key={task.job_id} className="space-y-2">
                <div>
                  <div className="text-white font-medium truncate">
                    {task.media_title || task.media_id}
                  </div>
                  <div className="text-xs text-emby-text-muted font-mono">
                    {task.job_id}
                  </div>
                </div>
                <ProgressBar value={task.progress} stage={task.stage} />
                <div className="text-xs text-emby-text-secondary">
                  已用 {formatUptime((Date.now() - task.started_at_ms) / 1000)}
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div className="text-sm text-emby-text-secondary">空闲中</div>
        )}
      </Card>

      {/* 本地暂存（已完成处理但未被字幕 worker 拉走的 FLAC） */}
      <PendingStorageCard
        pending={status?.audio_local_pending ?? 0}
        maxPending={status?.audio_local_max_pending ?? 0}
        storageDir={status?.audio_storage_dir ?? ''}
      />

      {/* 累计统计 */}
      <div className="grid grid-cols-3 gap-3">
        <StatCard label="已完成" value={status?.stats.completed ?? 0} color="text-emby-green" />
        <StatCard label="失败" value={status?.stats.failed ?? 0} color="text-red-400" />
        <StatCard label="Worker ID" value={shortId(settings?.worker_id ?? '')} mono small />
      </div>

      {status?.stats.last_error && (
        <div className="card px-4 py-3 text-xs text-red-300 bg-red-900/20 border-red-700/40">
          <div className="text-red-400 mb-1 font-medium">最近一次错误：</div>
          <pre className="font-mono whitespace-pre-wrap break-words">{status.stats.last_error}</pre>
        </div>
      )}
    </div>
  );
}

// ---- 子组件 ----

function Card({
  title,
  icon,
  children,
}: {
  title: string;
  icon?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="card px-4 py-3">
      <div className="flex items-center gap-2 text-emby-text-secondary text-xs mb-2">
        {icon}
        {title}
      </div>
      {children}
    </div>
  );
}

function StatusLine({
  icon,
  text,
  sub,
}: {
  icon: 'ok' | 'warn' | 'paused' | 'error';
  text: string;
  sub?: string;
}) {
  const iconNode =
    icon === 'ok' ? (
      <CheckCircle2 className="w-4 h-4 text-emby-green" />
    ) : icon === 'warn' ? (
      <Loader2 className="w-4 h-4 text-yellow-400 animate-spin" />
    ) : icon === 'paused' ? (
      <PauseCircle className="w-4 h-4 text-emby-text-muted" />
    ) : (
      <XCircle className="w-4 h-4 text-red-400" />
    );
  return (
    <div className="flex items-start gap-2">
      {iconNode}
      <div className="flex-1 min-w-0">
        <div className="text-sm text-white">{text}</div>
        {sub && (
          <div className="text-xs text-emby-text-muted truncate font-mono mt-0.5">{sub}</div>
        )}
      </div>
    </div>
  );
}

function ProgressBar({ value, stage }: { value: number; stage: string }) {
  return (
    <div>
      <div className="flex items-center justify-between text-xs text-emby-text-secondary mb-1">
        <span>{stage}</span>
        <span className="tabular-nums">{value}%</span>
      </div>
      <div className="h-2 rounded-full bg-emby-bg-elevated overflow-hidden">
        <div
          className="h-full bg-emby-green transition-all"
          style={{ width: `${Math.max(2, Math.min(100, value))}%` }}
        />
      </div>
    </div>
  );
}

/**
 * PendingStorageCard：本地已完成处理、暂存等待 subtitle worker 拉走的 FLAC 数量。
 *
 * 展示规则：
 *   - max=0（不限流）：仅显示数字 + "不限流"，绿色
 *   - max>0 且 pending<max：显示 "N / M"，绿色
 *   - max>0 且 pending>=max：显示 "N / M"，琥珀色，并在 sub 行警告 poller 已暂停 claim
 *   - sub 行始终显示 storage_dir，便于排查 FLAC 落盘位置
 */
function PendingStorageCard({
  pending,
  maxPending,
  storageDir,
}: {
  pending: number;
  maxPending: number;
  storageDir: string;
}) {
  const limited = maxPending > 0;
  const blocked = limited && pending >= maxPending;
  const numberColor = blocked
    ? 'text-amber-400'
    : pending > 0
      ? 'text-blue-300'
      : 'text-white';
  const ratio = limited ? Math.min(1, pending / Math.max(1, maxPending)) : 0;
  return (
    <div className="card px-4 py-3">
      <div className="flex items-center gap-2 text-emby-text-secondary text-xs mb-2">
        <HardDrive className="w-4 h-4 text-blue-400" />
        本地暂存（待字幕 worker 拉取）
      </div>
      <div className="flex items-baseline gap-2">
        <span className={`text-2xl font-bold tabular-nums ${numberColor}`}>{pending}</span>
        <span className="text-sm text-emby-text-secondary tabular-nums">
          {limited ? `/ ${maxPending} 个` : '个 · 不限流'}
        </span>
      </div>
      {/* 进度条：仅限流模式下显示，直观看到距上限多远 */}
      {limited && (
        <div className="mt-2 h-1.5 rounded-full bg-emby-bg-elevated overflow-hidden">
          <div
            className={`h-full transition-all ${blocked ? 'bg-amber-400' : 'bg-blue-400'}`}
            style={{ width: `${Math.max(4, ratio * 100)}%` }}
          />
        </div>
      )}
      <div className="mt-2 text-[11px] text-emby-text-muted leading-snug">
        {blocked ? (
          <span className="text-amber-300">
            已达暂存上限，poller 暂停 claim 新任务，等待字幕 worker 拉走若干个后自动恢复。
          </span>
        ) : pending > 0 ? (
          <span>
            FLAC 已就绪，已通过 audio-ready 注册到服务端；字幕 worker claim 后会触发 fetch 拉走。
          </span>
        ) : (
          <span>暂无积压，所有已完成的音频均已被字幕 worker 拉走或尚未生成。</span>
        )}
        {storageDir && (
          <div className="mt-1 font-mono text-emby-text-muted truncate" title={storageDir}>
            {storageDir}
          </div>
        )}
      </div>
    </div>
  );
}

function StatCard({
  label,
  value,
  color = 'text-white',
  mono = false,
  small = false,
}: {
  label: string;
  value: number | string;
  color?: string;
  mono?: boolean;
  small?: boolean;
}) {
  return (
    <div className="card px-4 py-3">
      <div className="text-xs text-emby-text-secondary">{label}</div>
      <div
        className={`${small ? 'text-sm' : 'text-2xl'} font-bold tabular-nums ${color} ${
          mono ? 'font-mono' : ''
        }`}
      >
        {value}
      </div>
    </div>
  );
}

// ---- helpers ----

function formatUptime(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

function shortId(s: string): string {
  return s ? s.slice(0, 8) + '…' : '—';
}
