import { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import {
  Inbox,
  Cpu,
  CheckCircle2,
  XCircle,
  History,
  Trash2,
  Loader2,
  ChevronRight,
  RotateCcw,
  HardDrive,
  X,
} from 'lucide-react';
import {
  getRuntimeStatus,
  listTaskHistory,
  clearTaskHistory,
  retrySubtitleJob,
  listLocalArtifacts,
  deleteLocalArtifact,
  cancelRunningTask,
} from '../lib/api';
import type { TaskHistorySummary, LocalArtifact } from '../lib/types';
import { TaskDetailDrawer } from '../components/tasks/TaskDetailDrawer';

/**
 * Tasks 页面：当前任务 + 累计统计 + 历史列表 + 详情抽屉。
 *
 * Phase 5：接入了本地 SQLite 历史持久化。
 * 列表项点开打开右侧详情抽屉（阶段时间线 / ASR & MT 预览 / 错误信息）。
 */
export function Tasks() {
  const queryClient = useQueryClient();
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);

  const { data: status } = useQuery({
    queryKey: ['runtime-status'],
    queryFn: getRuntimeStatus,
    refetchInterval: 2000,
  });

  const { data: history, isLoading: historyLoading } = useQuery({
    queryKey: ['task-history'],
    queryFn: () => listTaskHistory(50, 0),
    refetchInterval: 5000,
  });

  const clearMutation = useMutation({
    mutationFn: () => clearTaskHistory(0),
    onSuccess: (deleted) => {
      queryClient.invalidateQueries({ queryKey: ['task-history'] });
      alert(`已清理 ${deleted} 条历史记录`);
    },
    onError: (err) => {
      alert(`清空失败：${err instanceof Error ? err.message : String(err)}`);
    },
  });

  const retryMutation = useMutation({
    mutationFn: (mediaId: string) => retrySubtitleJob(mediaId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['task-history'] });
      queryClient.invalidateQueries({ queryKey: ['runtime-status'] });
    },
    onError: (err) => {
      alert(`重试失败：${err instanceof Error ? err.message : String(err)}`);
    },
  });

  // v4：取消正在跑的任务（abort pipeline + 服务端 worker_capacity 回 queued）
  const cancelRunningMutation = useMutation({
    mutationFn: (jobId: string) => cancelRunningTask(jobId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['runtime-status'] });
      queryClient.invalidateQueries({ queryKey: ['local-artifacts'] });
    },
    onError: (err) => {
      alert(`取消失败：${err instanceof Error ? err.message : String(err)}`);
    },
  });

  // v4：本地 FLAC 暂存列表（已 audio_ready 等待 subtitle worker 拉取）
  const { data: localArtifacts, isLoading: artifactsLoading } = useQuery({
    queryKey: ['local-artifacts'],
    queryFn: listLocalArtifacts,
    refetchInterval: 5000,
  });

  // v4：删除本地 FLAC（默认 notify=true 走 audio-lost 让服务端把任务回 queued）
  const deleteArtifactMutation = useMutation({
    mutationFn: (jobId: string) => deleteLocalArtifact(jobId, true),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['local-artifacts'] });
      queryClient.invalidateQueries({ queryKey: ['runtime-status'] });
    },
    onError: (err) => {
      alert(`删除失败：${err instanceof Error ? err.message : String(err)}`);
    },
  });

  const handleRetry = (mediaId: string) => {
    retryMutation.mutate(mediaId);
  };

  const handleCancelRunning = (jobId: string, label: string) => {
    if (cancelRunningMutation.isPending) return;
    const yes = window.confirm(
      `取消正在处理的任务 "${label}"？\n\n` +
        `服务端会把任务标记为 worker_capacity（不算失败重试）回到队列，` +
        `其他 worker 可以继续处理。本地下载/解码进度会丢弃。`,
    );
    if (yes) cancelRunningMutation.mutate(jobId);
  };

  const handleDeleteArtifact = (artifact: LocalArtifact) => {
    if (deleteArtifactMutation.isPending) return;
    const sizeMB = (artifact.size / 1024 / 1024).toFixed(1);
    const durMin = (artifact.duration_ms / 60000).toFixed(0);
    const yes = window.confirm(
      `删除本地暂存的 FLAC？\n\n` +
        `job: ${artifact.job_id.slice(0, 8)}...\n` +
        `大小: ${sizeMB} MB · 时长: ${durMin} min\n\n` +
        `服务端任务会被标记为 audio_lost，回到 queued 等待其他 audio worker 重跑。\n` +
        `本地 .flac 文件会被永久删除。`,
    );
    if (yes) deleteArtifactMutation.mutate(artifact.job_id);
  };

  const handleClear = () => {
    if (clearMutation.isPending) return;
    const yes = window.confirm('清空所有历史记录？此操作不可恢复。');
    if (yes) clearMutation.mutate();
  };

  return (
    <div className="px-6 py-5 max-w-5xl mx-auto space-y-4 pb-12">
      <SectionTitle icon={<Cpu className="w-4 h-4" />} title="正在处理" />
      {status?.current_tasks && status.current_tasks.length > 0 ? (
        <div className="space-y-2">
          {status.current_tasks.map((task) => (
            <div
              key={task.job_id}
              className="card px-4 py-4 space-y-2 hover:bg-emby-bg-elevated/40 transition-colors flex items-start gap-3"
            >
              <button
                type="button"
                className="flex-1 min-w-0 space-y-2 text-left"
                onClick={() => setSelectedJobId(task.job_id)}
              >
                <div className="text-white font-medium flex items-center gap-2">
                  {task.media_title || task.media_id}
                  <ChevronRight className="w-4 h-4 text-emby-text-muted" />
                </div>
                <div className="text-xs text-emby-text-muted font-mono">
                  job_id: {task.job_id}
                </div>
                <div className="grid grid-cols-2 gap-3 mt-3 text-xs">
                  <Stat label="阶段" value={task.stage} />
                  <Stat label="进度" value={`${task.progress}%`} />
                </div>
              </button>
              {/* v4：取消正在处理的任务（abort pipeline + 服务端 worker_capacity 回 queued） */}
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  handleCancelRunning(task.job_id, task.media_title || task.media_id);
                }}
                disabled={
                  cancelRunningMutation.isPending &&
                  cancelRunningMutation.variables === task.job_id
                }
                title="取消该任务（不计为失败重试，回队列让其他 worker 处理）"
                className="shrink-0 inline-flex items-center gap-1 text-xs px-2.5 py-1 rounded border border-red-700/40 text-red-400 hover:bg-red-900/20 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {cancelRunningMutation.isPending &&
                cancelRunningMutation.variables === task.job_id ? (
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                ) : (
                  <X className="w-3.5 h-3.5" />
                )}
                取消
              </button>
            </div>
          ))}
        </div>
      ) : (
        <EmptyHint
          icon={<Inbox className="w-10 h-10 text-emby-text-muted" />}
          text="当前空闲，等待 claim 下一条任务"
        />
      )}

      {/* v4：本地暂存的 FLAC（已完成 audio_ready，等 subtitle worker 拉取）。
          长时间无 subtitle worker 接手时用户可主动删除节省磁盘 + 让服务端释放任务。 */}
      <SectionTitle
        icon={<HardDrive className="w-4 h-4" />}
        title={`本地暂存 FLAC${
          localArtifacts && localArtifacts.length > 0
            ? ` · ${localArtifacts.length} 条`
            : ''
        }`}
      />
      {artifactsLoading ? (
        <div className="card px-4 py-4 text-center text-xs text-emby-text-muted">
          <Loader2 className="w-3.5 h-3.5 inline animate-spin mr-2" />
          加载本地暂存...
        </div>
      ) : localArtifacts && localArtifacts.length > 0 ? (
        <div className="space-y-2">
          {localArtifacts.map((a) => (
            <ArtifactRow
              key={a.job_id}
              artifact={a}
              onDelete={() => handleDeleteArtifact(a)}
              deleting={
                deleteArtifactMutation.isPending &&
                deleteArtifactMutation.variables === a.job_id
              }
            />
          ))}
        </div>
      ) : (
        <EmptyHint
          icon={<HardDrive className="w-8 h-8 text-emby-text-muted" />}
          text="无暂存。完成 audio_ready 后等待拉取的 FLAC 会显示在这里。"
        />
      )}

      <SectionTitle icon={<CheckCircle2 className="w-4 h-4 text-emby-green" />} title="累计统计" />
      <div className="grid grid-cols-3 gap-3">
        <StatCard label="完成" value={status?.stats.completed ?? 0} color="text-emby-green" />
        <StatCard label="失败" value={status?.stats.failed ?? 0} color="text-red-400" />
        <StatCard label="状态" value={status?.registered ? '在线' : '未注册'} small />
      </div>

      {status?.stats.last_error && (
        <div className="card px-4 py-3 text-xs space-y-1">
          <div className="text-red-400 font-medium flex items-center gap-1.5">
            <XCircle className="w-3.5 h-3.5" />
            最近错误
          </div>
          <pre className="font-mono text-red-300 whitespace-pre-wrap break-words">
            {status.stats.last_error}
          </pre>
        </div>
      )}

      <div className="flex items-center justify-between pt-4">
        <SectionTitle icon={<History className="w-4 h-4" />} title="历史" />
        <button
          type="button"
          onClick={() => void handleClear()}
          disabled={clearMutation.isPending || !history || history.length === 0}
          className="text-xs text-emby-text-muted hover:text-red-400 disabled:opacity-30 flex items-center gap-1"
        >
          {clearMutation.isPending ? (
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
          ) : (
            <Trash2 className="w-3.5 h-3.5" />
          )}
          清空全部历史
        </button>
      </div>

      {historyLoading ? (
        <div className="card px-4 py-8 text-center text-sm text-emby-text-muted">
          <Loader2 className="w-4 h-4 inline animate-spin mr-2" />
          加载中…
        </div>
      ) : history && history.length > 0 ? (
        <ul className="space-y-1.5">
          {history.map((t) => (
            <HistoryRow
              key={t.job_id}
              task={t}
              onClick={() => setSelectedJobId(t.job_id)}
              isSelected={selectedJobId === t.job_id}
              onRetry={() => handleRetry(t.media_id)}
              retrying={retryMutation.isPending && retryMutation.variables === t.media_id}
            />
          ))}
        </ul>
      ) : (
        <EmptyHint
          icon={<History className="w-8 h-8 text-emby-text-muted" />}
          text="暂无历史。完成第一个任务后会出现在这里。"
        />
      )}

      <TaskDetailDrawer
        jobId={selectedJobId}
        onClose={() => setSelectedJobId(null)}
        onRetry={handleRetry}
        retrying={retryMutation.isPending}
      />
    </div>
  );
}

/**
 * v4：本地暂存 FLAC artifact 行。
 *
 * 显示：状态徽章（"等待拉取"）+ jobId + 大小 / 时长 / 暂存时长 + 格式。
 * 操作：删除按钮（默认 notify=true 走 audio-lost 回 queued）。
 */
function ArtifactRow({
  artifact,
  onDelete,
  deleting,
}: {
  artifact: LocalArtifact;
  onDelete: () => void;
  deleting: boolean;
}) {
  const sizeMB = (artifact.size / 1024 / 1024).toFixed(1);
  const durMin = Math.round(artifact.duration_ms / 60000);
  return (
    <div className="card px-3.5 py-2.5 hover:bg-emby-bg-elevated/40 transition-colors flex items-center gap-3">
      <span className="inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded bg-amber-900/20 text-amber-300 border border-amber-700/40 shrink-0">
        <HardDrive className="w-3 h-3" />
        等待拉取
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-sm text-emby-text-primary truncate font-mono">
          {artifact.job_id}
        </div>
        <div className="text-xs text-emby-text-muted flex items-center gap-3 mt-0.5 flex-wrap">
          <span>{sizeMB} MB</span>
          <span>·</span>
          <span>{durMin} min</span>
          <span>·</span>
          <span className="font-mono">{artifact.format}</span>
          <span>·</span>
          <span>{relativeTime(artifact.created_at_ms)}</span>
        </div>
      </div>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onDelete();
        }}
        disabled={deleting}
        title="删除本地 FLAC + 通知服务端 audio-lost 回 queued"
        className="shrink-0 inline-flex items-center gap-1 text-xs px-2.5 py-1 rounded border border-red-700/40 text-red-400 hover:bg-red-900/20 disabled:opacity-40 disabled:cursor-not-allowed"
      >
        {deleting ? (
          <Loader2 className="w-3.5 h-3.5 animate-spin" />
        ) : (
          <Trash2 className="w-3.5 h-3.5" />
        )}
        删除
      </button>
    </div>
  );
}

function HistoryRow({
  task,
  onClick,
  isSelected,
  onRetry,
  retrying,
}: {
  task: TaskHistorySummary;
  onClick: () => void;
  isSelected: boolean;
  onRetry: () => void;
  retrying: boolean;
}) {
  const dur =
    task.finished_at !== null
      ? formatShortDuration(task.finished_at - task.started_at)
      : '--';

  return (
    <li>
      <div
        className={`card px-3.5 py-2.5 hover:bg-emby-bg-elevated/40 transition-colors flex items-center gap-3 ${
          isSelected ? 'border-emby-green/50' : ''
        }`}
      >
        <button
          type="button"
          onClick={onClick}
          className="flex-1 min-w-0 flex items-center gap-3 text-left"
        >
          <div className="shrink-0">
            <StatusBadge status={task.status} />
          </div>
          <div className="flex-1 min-w-0">
            <div className="text-sm text-emby-text-primary truncate">
              {task.media_title || task.media_id}
            </div>
            <div className="text-xs text-emby-text-muted flex items-center gap-3 mt-0.5">
              <span className="font-mono">{relativeTime(task.started_at)}</span>
              <span>·</span>
              <span>{dur}</span>
              {task.segment_count !== null && (
                <>
                  <span>·</span>
                  <span>{task.segment_count} 条字幕</span>
                </>
              )}
              {task.asr_model && (
                <>
                  <span>·</span>
                  <span className="font-mono">{task.asr_model}</span>
                </>
              )}
            </div>
          </div>
          <ChevronRight className="w-4 h-4 text-emby-text-muted shrink-0" />
        </button>
        {task.status === 'failed' && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onRetry();
            }}
            disabled={retrying}
            title="重新提交此任务"
            className="shrink-0 inline-flex items-center gap-1 text-xs px-2.5 py-1 rounded border border-emby-green/40 text-emby-green hover:bg-emby-green/15 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {retrying ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <RotateCcw className="w-3.5 h-3.5" />
            )}
            重试
          </button>
        )}
      </div>
    </li>
  );
}

function StatusBadge({ status }: { status: string }) {
  if (status === 'done') {
    return (
      <span className="inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded bg-emby-green/15 text-emby-green border border-emby-green/30">
        <CheckCircle2 className="w-3 h-3" />
        完成
      </span>
    );
  }
  if (status === 'failed') {
    return (
      <span className="inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded bg-red-900/20 text-red-400 border border-red-700/40">
        <XCircle className="w-3 h-3" />
        失败
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded bg-blue-900/20 text-blue-300 border border-blue-700/40">
      <Loader2 className="w-3 h-3 animate-spin" />
      运行
    </span>
  );
}

function SectionTitle({ icon, title }: { icon: React.ReactNode; title: string }) {
  return (
    <div className="flex items-center gap-2 text-emby-text-secondary text-xs uppercase tracking-wider">
      {icon}
      {title}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-emby-text-muted">{label}</div>
      <div className="text-white font-medium">{value}</div>
    </div>
  );
}

function StatCard({
  label,
  value,
  color = 'text-white',
  small = false,
}: {
  label: string;
  value: number | string;
  color?: string;
  small?: boolean;
}) {
  return (
    <div className="card px-4 py-3">
      <div className="text-xs text-emby-text-secondary">{label}</div>
      <div className={`${small ? 'text-sm' : 'text-2xl'} font-bold tabular-nums ${color}`}>
        {value}
      </div>
    </div>
  );
}

function EmptyHint({ icon, text }: { icon: React.ReactNode; text: string }) {
  return (
    <div className="card px-4 py-12 flex flex-col items-center gap-3 text-emby-text-secondary">
      {icon}
      <div className="text-sm">{text}</div>
    </div>
  );
}

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return `${Math.floor(diff / 1000)}s 前`;
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m 前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h 前`;
  return new Date(ms).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function formatShortDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(1)}s`;
  const min = sec / 60;
  if (min < 60) return `${Math.floor(min)}m${Math.floor(sec % 60)}s`;
  return `${(min / 60).toFixed(1)}h`;
}
