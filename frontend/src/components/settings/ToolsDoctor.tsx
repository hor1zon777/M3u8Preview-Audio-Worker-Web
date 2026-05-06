import { useState } from 'react';
import { Search, CheckCircle2, XCircle, Loader2, Cpu, AlertTriangle } from 'lucide-react';
import { doctorProbe } from '../../lib/api';
import type { DoctorReport, ToolInfo } from '../../lib/types';

/**
 * ToolsDoctor 子组件（audio worker 版）。
 *
 * 一键扫描 PATH / Program Files / 应用资源目录，自动定位 2 个外部二进制：
 *   N_m3u8DL-RE.exe / ffmpeg.exe
 *
 * 与字幕项目相比删除了 whisper-cli 探测分支。
 *
 * 此组件目前不再回写到 Settings.form（audio worker 的 Settings.tsx 自己维护字段），
 * 只显示扫描结果作为诊断信息。
 */
export function ToolsDoctor() {
  const [report, setReport] = useState<DoctorReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleScan = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await doctorProbe();
      setReport(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const allFound = report ? report.m3u8dl.found && report.ffmpeg.found : false;

  return (
    <div className="rounded-md border border-emby-border bg-emby-bg-darker/40 p-3 space-y-3">
      <div className="flex items-center justify-between gap-2">
        <div>
          <div className="text-sm font-medium text-emby-text-primary flex items-center gap-2">
            <Search className="w-4 h-4" />
            自动检测工具
          </div>
          <div className="text-xs text-emby-text-muted mt-0.5">
            扫描 PATH / Program Files / 常见安装位置，自动找 N_m3u8DL-RE / ffmpeg
          </div>
        </div>
        <button
          type="button"
          onClick={handleScan}
          disabled={busy}
          className="btn-secondary text-xs flex items-center gap-1.5 px-3 py-1.5 shrink-0"
        >
          {busy ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <Search className="w-3.5 h-3.5" />}
          {busy ? '扫描中…' : '开始扫描'}
        </button>
      </div>

      {error && (
        <div className="text-xs text-red-400 bg-red-900/20 border border-red-700/40 rounded px-2 py-1.5">
          扫描出错：{error}
        </div>
      )}

      {report && (
        <div className="space-y-1.5">
          <ToolRow info={report.m3u8dl} />
          <ToolRow info={report.ffmpeg} />
          <div className="flex items-center justify-between pt-1.5 border-t border-emby-border">
            <div className="text-xs text-emby-text-muted">
              {allFound ? (
                <span className="text-emby-green">✓ 两个工具都已找到</span>
              ) : (
                <span className="text-yellow-400">部分工具未找到，需要手动填写或安装</span>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function ToolRow({ info }: { info: ToolInfo }) {
  return (
    <div className="text-xs flex items-start gap-2">
      <div className="shrink-0 pt-0.5">
        {info.found ? (
          <CheckCircle2 className="w-3.5 h-3.5 text-emby-green" />
        ) : (
          <XCircle className="w-3.5 h-3.5 text-red-400" />
        )}
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="font-medium text-emby-text-primary">{info.label}</span>
          {info.version && (
            <span className="text-emby-text-muted font-mono">{info.version}</span>
          )}
          {info.backend && (
            <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-emby-green/15 text-emby-green border border-emby-green/30">
              <Cpu className="w-3 h-3" />
              {info.backend}
            </span>
          )}
        </div>
        {info.found ? (
          <div className="font-mono text-emby-text-muted truncate" title={info.path}>
            {info.path}
          </div>
        ) : (
          <pre className="text-red-400/80 whitespace-pre-wrap break-all font-mono text-[11px] leading-snug">
            {info.error || '未找到'}
          </pre>
        )}
        {info.hint && (
          <div className="mt-1 flex items-start gap-1.5 text-yellow-400 bg-yellow-900/15 border border-yellow-700/40 rounded px-2 py-1.5">
            <AlertTriangle className="w-3.5 h-3.5 mt-0.5 shrink-0" />
            <span className="break-words leading-snug">{info.hint}</span>
          </div>
        )}
      </div>
    </div>
  );
}
