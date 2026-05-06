import { useEffect, useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  X,
  CheckCircle2,
  XCircle,
  Loader2,
  Clock,
  Cpu,
  Languages,
  FileText,
  RotateCcw,
} from 'lucide-react';
import { getTaskHistory } from '../../lib/api';
import type { StageRecord, TaskHistoryRow } from '../../lib/types';

interface TaskDetailDrawerProps {
  jobId: string | null;
  onClose: () => void;
  /** 失败任务点击重试（接收 mediaId） */
  onRetry?: (mediaId: string) => void;
  retrying?: boolean;
}

/**
 * 任务详情抽屉（右侧浮层）。
 *
 * 内容：
 *   - 顶部 banner：状态 + 总耗时
 *   - 元信息：媒体、语言对、模型
 *   - 阶段时间线：堆叠条状图，按 stage 占总时长比例
 *   - ASR 前 5 条原文 + 翻译前 5 条
 *   - 错误信息（如有）
 */
export function TaskDetailDrawer({ jobId, onClose, onRetry, retrying }: TaskDetailDrawerProps) {
  const { data, isLoading } = useQuery({
    queryKey: ['task-history', jobId],
    queryFn: () => (jobId ? getTaskHistory(jobId) : Promise.resolve(null)),
    enabled: !!jobId,
    // 任务可能还在 running，给个低频刷新让用户看到 stages 增长
    refetchInterval: (q) => {
      const row = q.state.data as TaskHistoryRow | null | undefined;
      return row && row.status === 'running' ? 2000 : false;
    },
  });

  // ESC 关闭
  useEffect(() => {
    if (!jobId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [jobId, onClose]);

  if (!jobId) return null;

  return (
    <>
      {/* 背景遮罩 */}
      <div
        className="fixed inset-0 bg-black/40 z-40 animate-in fade-in"
        onClick={onClose}
      />
      {/* 抽屉内容 */}
      <aside className="fixed top-0 right-0 bottom-0 w-full max-w-xl bg-emby-bg-card border-l border-emby-border z-50 flex flex-col shadow-2xl">
        <header className="flex items-center justify-between px-5 py-3 border-b border-emby-border shrink-0">
          <div>
            <h3 className="text-sm font-medium text-emby-text-primary">任务详情</h3>
            <div className="text-xs text-emby-text-muted font-mono mt-0.5">{jobId}</div>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-1.5 rounded hover:bg-emby-bg-elevated text-emby-text-secondary"
          >
            <X className="w-4 h-4" />
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-4 space-y-5">
          {isLoading ? (
            <div className="text-center py-10">
              <Loader2 className="w-5 h-5 inline animate-spin mr-2" />
              加载中…
            </div>
          ) : !data ? (
            <div className="text-center py-10 text-emby-text-muted">未找到该任务</div>
          ) : (
            <DetailContent row={data} onRetry={onRetry} retrying={!!retrying} />
          )}
        </div>
      </aside>
    </>
  );
}

function DetailContent({
  row,
  onRetry,
  retrying,
}: {
  row: TaskHistoryRow;
  onRetry?: (mediaId: string) => void;
  retrying: boolean;
}) {
  const totalMs = row.finished_at
    ? row.finished_at - row.started_at
    : Date.now() - row.started_at;
  const totalDurStr = formatDuration(totalMs);

  return (
    <>
      {/* 状态 banner */}
      <div className={`rounded-md px-3 py-2.5 flex items-center gap-2 ${statusBgClass(row.status)}`}>
        <StatusIcon status={row.status} />
        <div className="flex-1 min-w-0">
          <div className="text-sm font-medium text-emby-text-primary capitalize">
            {row.status === 'done' ? '已完成' : row.status === 'failed' ? '失败' : '运行中'}
          </div>
          <div className="text-xs text-emby-text-muted">总耗时 {totalDurStr}</div>
        </div>
        {row.status === 'failed' && onRetry && (
          <button
            type="button"
            onClick={() => onRetry(row.media_id)}
            disabled={retrying}
            className="shrink-0 inline-flex items-center gap-1.5 text-xs px-3 py-1.5 rounded border border-emby-green/40 text-emby-green hover:bg-emby-green/15 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {retrying ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <RotateCcw className="w-3.5 h-3.5" />
            )}
            重新提交
          </button>
        )}
      </div>

      {/* 元信息 */}
      <Section title="基本信息" icon={<FileText className="w-3.5 h-3.5" />}>
        <KV k="媒体" v={row.media_title || row.media_id} mono={!row.media_title} />
        <KV k="media_id" v={row.media_id} mono />
        <KV k="语言对" v={`${row.source_lang} → ${row.target_lang}`} />
        {row.asr_model && <KV k="ASR 模型" v={row.asr_model} />}
        {row.mt_model && <KV k="MT 模型" v={row.mt_model} />}
        {row.segment_count !== null && <KV k="字幕条数" v={String(row.segment_count)} />}
        {row.vtt_size !== null && <KV k="VTT 大小" v={formatBytes(row.vtt_size)} />}
        <KV k="开始时间" v={new Date(row.started_at).toLocaleString('zh-CN')} />
        {row.finished_at && (
          <KV k="结束时间" v={new Date(row.finished_at).toLocaleString('zh-CN')} />
        )}
      </Section>

      {/* 阶段时间线 */}
      {row.stages.length > 0 && (
        <Section title="阶段时间线" icon={<Clock className="w-3.5 h-3.5" />}>
          <StageTimeline stages={row.stages} />
        </Section>
      )}

      {/* 错误信息 */}
      {row.error_msg && (
        <Section title="错误信息" icon={<XCircle className="w-3.5 h-3.5 text-red-400" />}>
          <pre className="text-xs font-mono text-red-300 whitespace-pre-wrap break-words bg-red-900/15 border border-red-700/40 rounded p-2.5">
            {row.error_msg}
          </pre>
        </Section>
      )}

      {/* ASR 预览 */}
      {row.asr_preview.length > 0 && (
        <Section title={`ASR 预览（前 ${row.asr_preview.length} 条）`} icon={<Cpu className="w-3.5 h-3.5" />}>
          <ul className="space-y-1.5">
            {row.asr_preview.map((t, i) => (
              <li
                key={i}
                className="text-xs px-2.5 py-1.5 bg-emby-bg-darker/50 rounded border border-emby-border/50"
              >
                <span className="text-emby-text-muted mr-2">{i + 1}.</span>
                {t}
              </li>
            ))}
          </ul>
        </Section>
      )}

      {/* 翻译预览 */}
      {row.mt_preview.length > 0 && (
        <Section
          title={`翻译预览（前 ${row.mt_preview.length} 条）`}
          icon={<Languages className="w-3.5 h-3.5" />}
        >
          <ul className="space-y-1.5">
            {row.mt_preview.map((t, i) => (
              <li
                key={i}
                className="text-xs px-2.5 py-1.5 bg-emby-bg-darker/50 rounded border border-emby-border/50"
              >
                <span className="text-emby-text-muted mr-2">{i + 1}.</span>
                {t}
              </li>
            ))}
          </ul>
        </Section>
      )}
    </>
  );
}

function Section({
  title,
  icon,
  children,
}: {
  title: string;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="flex items-center gap-1.5 text-emby-text-secondary text-xs uppercase tracking-wider mb-2">
        {icon}
        {title}
      </div>
      <div className="space-y-1">{children}</div>
    </div>
  );
}

function KV({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="grid grid-cols-[100px_1fr] gap-2 text-xs">
      <span className="text-emby-text-muted">{k}</span>
      <span className={mono ? 'font-mono text-emby-text-primary break-all' : 'text-emby-text-primary'}>
        {v}
      </span>
    </div>
  );
}

function StageTimeline({ stages }: { stages: StageRecord[] }) {
  // 总宽度按所有阶段合计耗时计算
  const total = useMemo(() => {
    return stages.reduce((sum, s) => sum + (s.end_ms - s.start_ms), 0);
  }, [stages]);

  if (total === 0) {
    return <div className="text-xs text-emby-text-muted">阶段时长数据缺失</div>;
  }

  return (
    <div className="space-y-1.5">
      {/* 堆叠条状图 */}
      <div className="flex h-3 rounded overflow-hidden">
        {stages.map((s, i) => {
          const dur = s.end_ms - s.start_ms;
          const pct = (dur / total) * 100;
          return (
            <div
              key={i}
              className={`${stageColor(s.stage)} h-full`}
              style={{ width: `${pct}%` }}
              title={`${s.stage}: ${formatDuration(dur)}`}
            />
          );
        })}
      </div>
      {/* 各阶段图例 + 时长 */}
      <ul className="grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
        {stages.map((s, i) => {
          const dur = s.end_ms - s.start_ms;
          return (
            <li key={i} className="flex items-center gap-2">
              <span className={`inline-block w-2 h-2 rounded ${stageColor(s.stage)}`} />
              <span className="text-emby-text-secondary capitalize">{s.stage}</span>
              <span className="text-emby-text-muted font-mono ml-auto">{formatDuration(dur)}</span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function StatusIcon({ status }: { status: string }) {
  if (status === 'done') return <CheckCircle2 className="w-4 h-4 text-emby-green shrink-0" />;
  if (status === 'failed') return <XCircle className="w-4 h-4 text-red-400 shrink-0" />;
  return <Loader2 className="w-4 h-4 text-blue-400 shrink-0 animate-spin" />;
}

function statusBgClass(status: string): string {
  if (status === 'done') return 'bg-emby-green/15 border border-emby-green/30';
  if (status === 'failed') return 'bg-red-900/20 border border-red-700/40';
  return 'bg-blue-900/20 border border-blue-700/40';
}

function stageColor(stage: string): string {
  switch (stage) {
    case 'prepare':
      return 'bg-zinc-500';
    case 'download':
      return 'bg-blue-500';
    case 'extract':
      return 'bg-cyan-500';
    case 'asr':
      return 'bg-purple-500';
    case 'translate':
      return 'bg-amber-500';
    case 'writing':
      return 'bg-emby-green';
    default:
      return 'bg-zinc-400';
  }
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(1)}s`;
  const min = sec / 60;
  if (min < 60) return `${Math.floor(min)}m ${Math.floor(sec % 60)}s`;
  const h = Math.floor(min / 60);
  const m = Math.floor(min % 60);
  return `${h}h ${m}m`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(2)} MB`;
}
