import { useEffect, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  ScrollText,
  Pause,
  Play,
  Trash2,
  Copy,
  CheckCheck,
} from 'lucide-react';
import { getRecentLogs } from '../lib/api';
import type { LogEntry } from '../lib/types';

const LEVEL_COLORS: Record<string, string> = {
  ERROR: 'text-red-400',
  WARN: 'text-yellow-400',
  INFO: 'text-emby-green',
  DEBUG: 'text-blue-400',
  TRACE: 'text-emby-text-muted',
};

/**
 * Logs：实时滚动日志面板。
 *
 * 实现：定时轮询 get_recent_logs(500)，本地按 ts 去重；
 * 用户可以暂停自动滚动 / 清空当前视图（不影响 Rust 端环缓冲）。
 *
 * 复制能力：
 *  - 顶部「复制全部」：把 visible 拼成纯文本写入剪贴板
 *  - 每行 hover 出现复制图标：单行 OneLine 文本到剪贴板
 *  - 整段选择 Ctrl+C：select-text 容器 + textContent 自然支持，多行格式会丢失但内容完整
 *  - 双击行：选中整行（浏览器默认行为）
 */
export function Logs() {
  const [autoScroll, setAutoScroll] = useState(true);
  const [filter, setFilter] = useState('');
  // 清空：记录清空时间戳，仅展示 ts >= clearTs 的日志。
  // 这样既能立即清屏，新进来的日志也会自然显示，符合常见日志面板语义。
  const [clearTs, setClearTs] = useState(0);
  const [copyAllOk, setCopyAllOk] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  const { data: logs = [] } = useQuery({
    queryKey: ['logs'],
    queryFn: () => getRecentLogs(500),
    refetchInterval: 1000,
  });

  useEffect(() => {
    if (autoScroll && containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [logs, autoScroll]);

  const visible = logs.filter((e) => {
    if (e.ts < clearTs) return false;
    if (!filter) return true;
    const lf = filter.toLowerCase();
    return (
      e.message.toLowerCase().includes(lf) ||
      e.target.toLowerCase().includes(lf) ||
      e.level.toLowerCase().includes(lf)
    );
  });

  const handleCopyAll = async () => {
    if (visible.length === 0) return;
    // Windows 剪贴板/记事本只识别 CRLF；同时把 message 内嵌的换行归一化，
    // 避免 Rust tracing 多行 stdout 导致行边界错乱。
    const text = visible.map(formatLine).join('\r\n');
    try {
      await navigator.clipboard.writeText(text);
      setCopyAllOk(true);
      setTimeout(() => setCopyAllOk(false), 1500);
    } catch (e) {
      console.error('clipboard write failed', e);
    }
  };

  return (
    <div className="px-6 py-5 max-w-6xl mx-auto h-full flex flex-col">
      <div className="flex items-center gap-2 mb-3">
        <ScrollText className="w-4 h-4 text-emby-text-secondary" />
        <h1 className="text-sm font-medium text-white">日志</h1>
        <span className="text-xs text-emby-text-muted">
          最近 {logs.length} 条 · 显示 {visible.length} 条
        </span>
        <div className="ml-auto flex items-center gap-2">
          <input
            type="text"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="过滤..."
            className="px-2 py-1 text-xs bg-emby-bg-input border border-emby-border rounded text-white placeholder-emby-text-muted focus:outline-none focus:ring-2 focus:ring-emby-green"
          />
          <button
            onClick={handleCopyAll}
            disabled={visible.length === 0}
            className="btn-secondary text-xs disabled:opacity-40 disabled:cursor-not-allowed"
            title={`复制当前显示的 ${visible.length} 条日志到剪贴板`}
          >
            {copyAllOk ? (
              <CheckCheck className="w-3.5 h-3.5 text-emby-green" />
            ) : (
              <Copy className="w-3.5 h-3.5" />
            )}
            {copyAllOk ? '已复制' : '复制全部'}
          </button>
          <button
            onClick={() => setAutoScroll((s) => !s)}
            className="btn-secondary text-xs"
            title={autoScroll ? '暂停自动滚动' : '恢复自动滚动'}
          >
            {autoScroll ? <Pause className="w-3.5 h-3.5" /> : <Play className="w-3.5 h-3.5" />}
            {autoScroll ? '暂停' : '滚动'}
          </button>
          <button
            onClick={() => setClearTs(Date.now())}
            className="btn-secondary text-xs"
            title="清空当前视图（不影响 Rust 端缓冲，新日志会继续显示）"
          >
            <Trash2 className="w-3.5 h-3.5" />
            清空
          </button>
        </div>
      </div>

      <div
        ref={containerRef}
        className="card flex-1 overflow-auto px-3 py-2 font-mono text-xs select-text"
        style={{ minHeight: 0 }}
      >
        {visible.length === 0 ? (
          <div className="text-center text-emby-text-muted py-12">暂无日志</div>
        ) : (
          visible.map((entry, idx) => <LogRow key={`${entry.ts}-${idx}`} entry={entry} />)
        )}
      </div>

      <p className="text-xs text-emby-text-muted mt-2">
        提示：单行 hover 显示复制按钮；整段选中后 Ctrl+C 也可复制。
      </p>
    </div>
  );
}

function LogRow({ entry }: { entry: LogEntry }) {
  const colorCls = LEVEL_COLORS[entry.level] ?? 'text-emby-text-secondary';
  const [ok, setOk] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(formatLine(entry));
      setOk(true);
      setTimeout(() => setOk(false), 1500);
    } catch (e) {
      console.error('clipboard write failed', e);
    }
  };

  return (
    <div className="group relative flex gap-2 leading-relaxed py-0.5 hover:bg-emby-bg-elevated/30 rounded">
      <span className="text-emby-text-muted shrink-0 tabular-nums">
        {formatTime(entry.ts)}
      </span>
      <span className={`shrink-0 w-12 ${colorCls}`}>{entry.level}</span>
      <span
        className="text-emby-text-muted shrink-0 max-w-[180px] truncate"
        title={entry.target}
      >
        {entry.target}
      </span>
      <span className="text-emby-text-primary break-all flex-1">{entry.message}</span>
      <button
        type="button"
        onClick={handleCopy}
        title="复制此行"
        className="shrink-0 opacity-0 group-hover:opacity-100 transition-opacity p-1 -my-1 rounded hover:bg-emby-bg-elevated text-emby-text-muted hover:text-emby-text-primary"
      >
        {ok ? (
          <CheckCheck className="w-3 h-3 text-emby-green" />
        ) : (
          <Copy className="w-3 h-3" />
        )}
      </button>
    </div>
  );
}

/** 把一条日志格式化成单行纯文本（用于剪贴板）。
 *  注意 message 字段可能内嵌换行（subprocess 多行 stdout），需要折成单行字面量，
 *  否则 join 出来行边界错乱、单行复制也会跨行。 */
function formatLine(entry: LogEntry): string {
  const oneLineMsg = entry.message.replace(/\r\n|\n|\r/g, ' ⏎ ');
  return `${formatTime(entry.ts)} ${entry.level.padEnd(5)} ${entry.target} ${oneLineMsg}`;
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${d.getMilliseconds().toString().padStart(3, '0')}`;
}
