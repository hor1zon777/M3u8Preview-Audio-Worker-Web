import { useEffect, useState } from 'react';
import { NavLink, Route, Routes, Navigate } from 'react-router-dom';
import {
  LayoutDashboard,
  ListChecks,
  SlidersHorizontal,
  ScrollText,
  Activity,
  Lock,
  Loader2,
} from 'lucide-react';
import { Dashboard } from './pages/Dashboard';
import { Tasks } from './pages/Tasks';
import { Settings } from './pages/Settings';
import { Logs } from './pages/Logs';
import {
  authCheck,
  validateToken,
  getToken,
  setToken,
} from './lib/api';

const NAV: Array<{ path: string; label: string; icon: React.ComponentType<{ className?: string }> }> = [
  { path: '/dashboard', label: '总览', icon: LayoutDashboard },
  { path: '/tasks', label: '任务', icon: ListChecks },
  { path: '/settings', label: '设置', icon: SlidersHorizontal },
  { path: '/logs', label: '日志', icon: ScrollText },
];

/** 鉴权门控：检查是否需要登录，需要时显示登录页 */
function AuthGate({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<'checking' | 'login' | 'ok'>('checking');
  const [input, setInput] = useState('');
  const [error, setError] = useState('');

  useEffect(() => {
    (async () => {
      try {
        const check = await authCheck();
        if (!check.required) {
          setState('ok');
          return;
        }
        // 需要鉴权：检查已有 token 是否有效
        if (getToken()) {
          const valid = await validateToken();
          if (valid) {
            setState('ok');
            return;
          }
        }
        setState('login');
      } catch {
        // auth/check 失败（后端未启动等），放行让页面自己报错
        setState('ok');
      }
    })();
  }, []);

  const handleLogin = async () => {
    setError('');
    const trimmed = input.trim();
    if (!trimmed) {
      setError('请输入 Token');
      return;
    }
    setToken(trimmed);
    const valid = await validateToken();
    if (valid) {
      setState('ok');
    } else {
      setToken('');
      setError('Token 无效');
    }
  };

  if (state === 'checking') {
    return (
      <div className="h-full flex items-center justify-center bg-emby-bg-base">
        <Loader2 className="w-6 h-6 animate-spin text-emby-text-muted" />
      </div>
    );
  }

  if (state === 'login') {
    return (
      <div className="h-full flex items-center justify-center bg-emby-bg-base">
        <div className="w-80 space-y-4 p-6 rounded-lg bg-emby-bg-card border border-emby-border">
          <div className="flex items-center gap-2 text-white font-semibold">
            <Lock className="w-5 h-5 text-emby-green" />
            <span>Audio Worker</span>
          </div>
          <p className="text-xs text-emby-text-muted">
            此面板已启用鉴权，请输入 Access Token
          </p>
          <input
            type="password"
            className="input w-full"
            placeholder="Access Token"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleLogin()}
            autoFocus
          />
          {error && (
            <p className="text-xs text-emby-red">{error}</p>
          )}
          <button
            className="btn-primary w-full"
            onClick={handleLogin}
          >
            登录
          </button>
        </div>
      </div>
    );
  }

  return <>{children}</>;
}

export default function App() {
  return (
    <AuthGate>
      <div className="h-full flex flex-col bg-emby-bg-base">
        {/* 顶部导航条 */}
        <header className="flex items-center px-4 py-3 border-b border-emby-border bg-emby-bg-card">
          <div className="flex items-center gap-2 text-emby-green font-semibold mr-6">
            <Activity className="w-5 h-5" />
            <span>M3u8PreviewAudioWorker</span>
            <span className="ml-2 px-1.5 py-0.5 text-[10px] font-normal rounded bg-emby-green/20 text-emby-green border border-emby-green/30">
              audio_extract
            </span>
          </div>
          <nav className="flex items-center gap-1">
            {NAV.map(({ path, label, icon: Icon }) => (
              <NavLink
                key={path}
                to={path}
                className={({ isActive }) =>
                  `px-3 py-1.5 text-sm rounded-md flex items-center gap-1.5 transition-colors ${
                    isActive
                      ? 'bg-emby-bg-elevated text-white'
                      : 'text-emby-text-secondary hover:bg-emby-bg-elevated/60 hover:text-white'
                  }`
                }
              >
                <Icon className="w-4 h-4" />
                {label}
              </NavLink>
            ))}
          </nav>
        </header>

        {/* 内容区 */}
        <main className="flex-1 overflow-auto">
          <Routes>
            <Route path="/" element={<Navigate to="/dashboard" replace />} />
            <Route path="/dashboard" element={<Dashboard />} />
            <Route path="/tasks" element={<Tasks />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/logs" element={<Logs />} />
          </Routes>
        </main>
      </div>
    </AuthGate>
  );
}
