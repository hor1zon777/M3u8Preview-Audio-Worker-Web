import { useEffect, useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import {
  Save,
  TestTube2,
  CheckCircle2,
  XCircle,
  Eye,
  EyeOff,
  Loader2,
  RefreshCw,
} from 'lucide-react';
import {
  getSettings,
  saveSettings,
  testConnection,
  registerWorker,
  validateTempDir,
  getRuntimeStatus,
} from '../lib/api';
import type { Settings as SettingsType, ValidateDirResult } from '../lib/types';
import { ToolsDoctor } from '../components/settings/ToolsDoctor';

/**
 * Settings：audio worker 用户配置。
 *
 * 字段范围（与字幕项目相比大幅精简）：
 *   - server      连接服务器
 *   - worker_id / worker_name
 *   - pipeline    N_m3u8DL-RE / ffmpeg / temp_dir / FLAC 编码参数
 *   - network     GitHub 代理（保留以备将来使用）
 *   - ui          minimize_to_tray / autostart
 */
export function Settings() {
  const queryClient = useQueryClient();
  const { data: initial } = useQuery({
    queryKey: ['settings'],
    queryFn: getSettings,
    staleTime: Infinity,
  });

  // 拉取运行时状态以拿到「服务端 register 响应中的最大并发任务数硬上限」。
  // 该值用于动态约束「最大并发任务」输入框的 max 属性，替代旧版硬编码 8。
  // 0 = 尚未注册或服务端未下发；UI 此时不约束 max（让用户自由输入）。
  const { data: runtimeStatus } = useQuery({
    queryKey: ['runtime-status'],
    queryFn: getRuntimeStatus,
    // 注册成功 / 保存设置后会触发 invalidate，正常 2s 间隔够用
    refetchInterval: 5000,
  });
  const serverMaxConcurrent = runtimeStatus?.server_max_concurrent_tasks ?? 0;

  const [form, setForm] = useState<SettingsType | null>(null);
  useEffect(() => {
    if (initial) setForm(initial);
  }, [initial]);

  const saveMutation = useMutation({
    mutationFn: (s: SettingsType) => saveSettings(s),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['settings'] });
      queryClient.invalidateQueries({ queryKey: ['runtime-status'] });
    },
  });

  const [pingMsg, setPingMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [pingBusy, setPingBusy] = useState(false);
  const handlePing = async () => {
    if (!form) return;
    setPingBusy(true);
    setPingMsg(null);
    try {
      await saveSettings(form);
      const r = await testConnection();
      setPingMsg({ ok: r.ok, text: r.message });
    } catch (e) {
      setPingMsg({ ok: false, text: String(e) });
    } finally {
      setPingBusy(false);
    }
  };

  const [registerBusy, setRegisterBusy] = useState(false);
  const [registerMsg, setRegisterMsg] = useState<string | null>(null);
  const handleRegister = async () => {
    if (!form) return;
    setRegisterBusy(true);
    setRegisterMsg(null);
    try {
      await saveSettings(form);
      const r = await registerWorker();
      const acceptedCaps = r.acceptedCapabilities?.join(',') ?? '';
      setRegisterMsg(
        `已注册：worker_id=${r.workerId.slice(0, 8)}… stale=${r.workerStaleThreshold}s caps=${acceptedCaps}`,
      );
      queryClient.invalidateQueries({ queryKey: ['runtime-status'] });
    } catch (e) {
      setRegisterMsg(String(e));
    } finally {
      setRegisterBusy(false);
    }
  };

  const [showToken, setShowToken] = useState(false);
  const [tempDirStatus, setTempDirStatus] = useState<ValidateDirResult | null>(null);
  const [audioStorageDirStatus, setAudioStorageDirStatus] = useState<ValidateDirResult | null>(null);

  const checkTempDir = async (path: string) => {
    try {
      const r = await validateTempDir(path);
      setTempDirStatus(r);
    } catch (e) {
      setTempDirStatus({ ok: false, message: String(e), resolved_path: path });
    }
  };
  useEffect(() => {
    if (form) checkTempDir(form.pipeline.temp_dir);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form?.pipeline.temp_dir]);

  // audio_storage_dir 校验：留空时显示"将使用默认（APPDATA/audio_artifacts）"；
  // 非空时复用 validateTempDir 校验目录存在 + 可写性
  const checkAudioStorageDir = async (path: string) => {
    const trimmed = path.trim();
    if (trimmed === '') {
      setAudioStorageDirStatus({
        ok: true,
        message: '未指定，将使用 APPDATA/audio_artifacts/',
        resolved_path: '',
      });
      return;
    }
    try {
      const r = await validateTempDir(trimmed);
      setAudioStorageDirStatus(r);
    } catch (e) {
      setAudioStorageDirStatus({ ok: false, message: String(e), resolved_path: trimmed });
    }
  };
  useEffect(() => {
    if (form) checkAudioStorageDir(form.pipeline.audio_storage_dir);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form?.pipeline.audio_storage_dir]);

  if (!form) {
    return (
      <div className="p-8 text-emby-text-secondary flex items-center gap-2">
        <Loader2 className="w-4 h-4 animate-spin" />
        加载中…
      </div>
    );
  }

  return (
    <div className="p-6 space-y-8 max-w-3xl">
      {/* Server */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">服务器</h2>
        <div className="space-y-2">
          <Field label="Server URL">
            <input
              className="input"
              placeholder="https://media.example.com"
              value={form.server.base_url}
              onChange={(e) => setForm({ ...form, server: { ...form.server, base_url: e.target.value } })}
            />
          </Field>

          <Field label="Token">
            <div className="flex gap-2">
              <input
                className="input flex-1"
                type={showToken ? 'text' : 'password'}
                placeholder="mwt_..."
                value={form.server.token}
                onChange={(e) => setForm({ ...form, server: { ...form.server, token: e.target.value } })}
              />
              <button
                className="btn-icon"
                onClick={() => setShowToken((v) => !v)}
                title={showToken ? '隐藏' : '显示'}
              >
                {showToken ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
              </button>
            </div>
          </Field>

          <div className="grid grid-cols-3 gap-3">
            <Field label="Poll 间隔（秒）">
              <input
                type="number"
                className="input"
                min={1}
                value={form.server.poll_interval_sec}
                onChange={(e) =>
                  setForm({
                    ...form,
                    server: { ...form.server, poll_interval_sec: Number(e.target.value) || 5 },
                  })
                }
              />
            </Field>
            <Field label="心跳间隔（秒）">
              <input
                type="number"
                className="input"
                min={5}
                value={form.server.heartbeat_interval_sec}
                onChange={(e) =>
                  setForm({
                    ...form,
                    server: { ...form.server, heartbeat_interval_sec: Number(e.target.value) || 30 },
                  })
                }
              />
            </Field>
            <Field label="错误退避（秒）">
              <input
                type="number"
                className="input"
                min={1}
                value={form.server.error_backoff_sec}
                onChange={(e) =>
                  setForm({
                    ...form,
                    server: { ...form.server, error_backoff_sec: Number(e.target.value) || 5 },
                  })
                }
              />
            </Field>
          </div>

          <div className="grid grid-cols-2 gap-3">
            <Field label="校验 TLS 证书">
              <select
                className="input"
                value={String(form.server.verify_tls)}
                onChange={(e) =>
                  setForm({ ...form, server: { ...form.server, verify_tls: e.target.value === 'true' } })
                }
              >
                <option value="true">是（生产）</option>
                <option value="false">否（开发自签）</option>
              </select>
            </Field>
            <Field label="最大并发任务">
              <input
                type="number"
                className="input"
                min={1}
                // 服务端 register 响应中下发的硬上限作为 max；为 0（未注册 / 未下发）时不约束。
                // 解除掉旧版硬编码 max=8 的限制——具体上限由服务端控制。
                {...(serverMaxConcurrent > 0 ? { max: serverMaxConcurrent } : {})}
                value={form.server.max_concurrent_tasks}
                onChange={(e) => {
                  const raw = Math.max(1, Math.floor(Number(e.target.value) || 1));
                  // 仅当服务端已下发硬上限时才在客户端 clamp，否则信任用户输入；
                  // 服务端会在下次 register 时把多余的部分 clamp 回来（apply_register_response）。
                  const clamped =
                    serverMaxConcurrent > 0 ? Math.min(serverMaxConcurrent, raw) : raw;
                  setForm({
                    ...form,
                    server: {
                      ...form.server,
                      max_concurrent_tasks: clamped,
                    },
                  });
                }}
              />
              <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
                {serverMaxConcurrent > 0 ? (
                  <>
                    服务端允许的最大并发：<strong className="text-emby-text-secondary">{serverMaxConcurrent}</strong>。
                    超出此值会被服务端在下一次 register 时自动 clamp。
                  </>
                ) : (
                  <>
                    尚未注册或服务端未下发并发上限；建议先点「立即注册」拉取硬上限后再设置。
                  </>
                )}
              </div>
            </Field>
          </div>

          <div className="flex gap-3 pt-1">
            <button className="btn-secondary" disabled={pingBusy} onClick={handlePing}>
              {pingBusy ? <Loader2 className="w-4 h-4 animate-spin" /> : <TestTube2 className="w-4 h-4" />}
              测试连接
            </button>
            <button className="btn-secondary" disabled={registerBusy} onClick={handleRegister}>
              {registerBusy ? <Loader2 className="w-4 h-4 animate-spin" /> : <RefreshCw className="w-4 h-4" />}
              立即注册
            </button>
          </div>
          {pingMsg && (
            <div
              className={`text-xs flex items-center gap-1.5 ${
                pingMsg.ok ? 'text-emby-green' : 'text-emby-red'
              }`}
            >
              {pingMsg.ok ? <CheckCircle2 className="w-3.5 h-3.5" /> : <XCircle className="w-3.5 h-3.5" />}
              {pingMsg.text}
            </div>
          )}
          {registerMsg && (
            <div className="text-xs text-emby-text-secondary">{registerMsg}</div>
          )}
        </div>
      </section>

      {/* Worker 标识 */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">Worker 标识</h2>
        <Field label="Worker 名称">
          <input
            className="input"
            value={form.worker_name}
            onChange={(e) => setForm({ ...form, worker_name: e.target.value })}
          />
        </Field>
        <Field label="Worker ID（首次启动自动生成）">
          <input
            className="input font-mono text-xs"
            value={form.worker_id}
            readOnly
          />
        </Field>
      </section>

      {/* 工具 */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">外部工具</h2>
        <ToolsDoctor />
        <Field label="N_m3u8DL-RE 路径（空 = 自动找）">
          <input
            className="input"
            placeholder="留空使用随包默认"
            value={form.pipeline.m3u8dl_path}
            onChange={(e) =>
              setForm({ ...form, pipeline: { ...form.pipeline, m3u8dl_path: e.target.value } })
            }
          />
        </Field>
        <Field label="ffmpeg 路径（空 = 走 PATH）">
          <input
            className="input"
            placeholder="留空使用随包默认"
            value={form.pipeline.ffmpeg_path}
            onChange={(e) =>
              setForm({ ...form, pipeline: { ...form.pipeline, ffmpeg_path: e.target.value } })
            }
          />
        </Field>
      </section>

      {/* 工作目录 / FLAC 参数 */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">工作目录 + FLAC 编码</h2>
        <Field label="暂存目录（空 = 系统 Temp）">
          <div className="flex gap-2">
            <input
              className="input flex-1"
              placeholder="留空使用系统 Temp"
              value={form.pipeline.temp_dir}
              onChange={(e) =>
                setForm({ ...form, pipeline: { ...form.pipeline, temp_dir: e.target.value } })
              }
            />
          </div>
          <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
            中间产物（mp4 切片 / wav）暂存位置，pipeline 结束自动清理。建议指向大盘（4 GB+ 视频不要压系统盘）。
          </div>
          {tempDirStatus && (
            <div
              className={`mt-1 text-xs flex items-center gap-1.5 ${
                tempDirStatus.ok ? 'text-emby-text-secondary' : 'text-emby-red'
              }`}
            >
              {tempDirStatus.ok ? (
                <CheckCircle2 className="w-3.5 h-3.5" />
              ) : (
                <XCircle className="w-3.5 h-3.5" />
              )}
              {tempDirStatus.message}
            </div>
          )}
        </Field>

        <Field label="音频保存目录（空 = APPDATA/audio_artifacts）">
          <div className="flex gap-2">
            <input
              className="input flex-1"
              placeholder="留空使用 /data/audio-artifacts/"
              value={form.pipeline.audio_storage_dir}
              onChange={(e) =>
                setForm({
                  ...form,
                  pipeline: { ...form.pipeline, audio_storage_dir: e.target.value },
                })
              }
            />
          </div>
          <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
            v3 broker 模式 FLAC 永久存储目录（每个任务约 50 MB / 小时）。任务进 DONE 后服务端会通过 long-poll 通道通知本机自动清理。
            建议放与「暂存目录」<strong className="text-emby-text-secondary">同一磁盘</strong>，编码后的 rename 是原子操作（跨盘会回退到 copy + delete，多 50 MB 复制开销）。
          </div>
          {audioStorageDirStatus && (
            <div
              className={`mt-1 text-xs flex items-center gap-1.5 ${
                audioStorageDirStatus.ok ? 'text-emby-text-secondary' : 'text-emby-red'
              }`}
            >
              {audioStorageDirStatus.ok ? (
                <CheckCircle2 className="w-3.5 h-3.5" />
              ) : (
                <XCircle className="w-3.5 h-3.5" />
              )}
              {audioStorageDirStatus.message}
            </div>
          )}
        </Field>

        <Field label="本地暂存上限（0 = 不限流）">
          <input
            type="number"
            className="input"
            min={0}
            value={form.pipeline.audio_local_max_pending}
            onChange={(e) => {
              // 解除旧版 max=100 上限，仅做下界 0 的兜底；用户自行权衡磁盘空间
              const raw = Number(e.target.value);
              const clamped = Number.isFinite(raw) ? Math.max(0, Math.floor(raw)) : 5;
              setForm({
                ...form,
                pipeline: { ...form.pipeline, audio_local_max_pending: clamped },
              });
            }}
          />
          <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
            「音频保存目录」中等待被字幕 worker 拉走的 FLAC 数量达到此值后，<strong className="text-emby-text-secondary">暂停 claim 新任务</strong>，
            直到字幕 worker 拉走若干个降回阈值以下。每个 FLAC 通常 50–200 MB / 小时；默认 5 个 ≈ 1 GB 上限。
            设为 0 表示不限流（仅受磁盘空间限制，不推荐）。
          </div>
        </Field>

        <div className="grid grid-cols-3 gap-3">
          <Field label="中间格式">
            <select
              className="input"
              value={form.pipeline.intermediate_audio_format}
              onChange={(e) =>
                setForm({
                  ...form,
                  pipeline: {
                    ...form.pipeline,
                    intermediate_audio_format: e.target.value as
                      | 'flac'
                      | 'opus_low'
                      | 'wav',
                  },
                })
              }
            >
              <option value="flac">FLAC（默认，无损）</option>
              <option value="opus_low" disabled>
                Opus 24k（未来扩展）
              </option>
              <option value="wav" disabled>
                WAV（未来扩展）
              </option>
            </select>
          </Field>
          <Field label="FLAC 压缩等级 (0~12)">
            <input
              type="number"
              min={0}
              max={12}
              className="input"
              value={form.pipeline.flac_compression_level}
              onChange={(e) =>
                setForm({
                  ...form,
                  pipeline: {
                    ...form.pipeline,
                    flac_compression_level: Math.max(0, Math.min(12, Number(e.target.value) || 8)),
                  },
                })
              }
            />
          </Field>
          <Field label="编码超时（秒）">
            <input
              type="number"
              min={60}
              className="input"
              value={form.pipeline.flac_timeout_sec}
              onChange={(e) =>
                setForm({
                  ...form,
                  pipeline: {
                    ...form.pipeline,
                    flac_timeout_sec: Math.max(60, Number(e.target.value) || 600),
                  },
                })
              }
            />
          </Field>
        </div>
      </section>

      {/* 网络代理 */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">网络代理</h2>
        <Field label="M3U8 下载代理（HTTP / HTTPS / SOCKS5）">
          <input
            className="input"
            placeholder="留空 = 直连。例：http://127.0.0.1:7890 或 socks5://127.0.0.1:1080"
            value={form.network.download_proxy}
            onChange={(e) =>
              setForm({
                ...form,
                network: { ...form.network, download_proxy: e.target.value },
              })
            }
          />
          <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
            仅用于 M3U8 流下载（N_m3u8DL-RE），不影响与服务器的 API 通信。
            支持 HTTP / HTTPS / SOCKS5 协议。留空则直连。
          </div>
        </Field>
        <Field label="GitHub 代理（可选，URL 前缀加速）">
          <div className="grid grid-cols-2 gap-3">
            <select
              className="input"
              value={String(form.network.github_proxy_enabled)}
              onChange={(e) =>
                setForm({
                  ...form,
                  network: { ...form.network, github_proxy_enabled: e.target.value === 'true' },
                })
              }
            >
              <option value="false">关闭</option>
              <option value="true">启用</option>
            </select>
            <input
              className="input"
              placeholder="https://gh-proxy.com"
              value={form.network.github_proxy_url}
              onChange={(e) =>
                setForm({
                  ...form,
                  network: { ...form.network, github_proxy_url: e.target.value },
                })
              }
            />
          </div>
          <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
            将 GitHub release / raw URL 包成代理 URL（prefix style），用于国内加速下载二进制依赖。
          </div>
        </Field>
      </section>

      {/* Web 面板鉴权 */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">Web 面板鉴权</h2>
        <Field label="Access Token（留空 = 不需要鉴权）">
          <input
            type="password"
            className="input"
            placeholder="留空则任何人可访问管理面板"
            value={form.web_auth_token ?? ''}
            onChange={(e) =>
              setForm({ ...form, web_auth_token: e.target.value })
            }
          />
          <div className="mt-1 text-[11px] text-emby-text-muted leading-snug">
            设置后浏览器访问面板需要输入此 Token。修改后需刷新页面重新登录。
          </div>
        </Field>
      </section>

      {/* UI */}
      <section className="space-y-3">
        <h2 className="text-base font-semibold text-white">界面</h2>
        <Field label="关闭主窗口时最小化到托盘">
          <select
            className="input"
            value={String(form.ui.minimize_to_tray)}
            onChange={(e) =>
              setForm({ ...form, ui: { ...form.ui, minimize_to_tray: e.target.value === 'true' } })
            }
          >
            <option value="false">否（直接退出）</option>
            <option value="true">是</option>
          </select>
        </Field>
        <Field label="开机自启（暂未实现）">
          <select
            className="input"
            value={String(form.ui.autostart)}
            onChange={(e) =>
              setForm({ ...form, ui: { ...form.ui, autostart: e.target.value === 'true' } })
            }
          >
            <option value="false">否</option>
            <option value="true">是</option>
          </select>
        </Field>
      </section>

      {/* 操作 */}
      <div className="flex gap-3 sticky bottom-0 py-3 bg-emby-bg-base border-t border-emby-border">
        <button
          className="btn-primary"
          disabled={saveMutation.isPending}
          onClick={() => saveMutation.mutate(form)}
        >
          {saveMutation.isPending ? <Loader2 className="w-4 h-4 animate-spin" /> : <Save className="w-4 h-4" />}
          保存
        </button>
        {saveMutation.isSuccess && (
          <span className="text-xs text-emby-green flex items-center gap-1.5">
            <CheckCircle2 className="w-3.5 h-3.5" />
            已保存
          </span>
        )}
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block text-xs text-emby-text-secondary">
      <span className="block mb-1">{label}</span>
      {children}
    </label>
  );
}
