import { NavLink, Route, Routes, Navigate } from 'react-router-dom';
import {
  LayoutDashboard,
  ListChecks,
  SlidersHorizontal,
  ScrollText,
  Activity,
} from 'lucide-react';
import { Dashboard } from './pages/Dashboard';
import { Tasks } from './pages/Tasks';
import { Settings } from './pages/Settings';
import { Logs } from './pages/Logs';

const NAV: Array<{ path: string; label: string; icon: React.ComponentType<{ className?: string }> }> = [
  { path: '/dashboard', label: '总览', icon: LayoutDashboard },
  { path: '/tasks', label: '任务', icon: ListChecks },
  { path: '/settings', label: '设置', icon: SlidersHorizontal },
  { path: '/logs', label: '日志', icon: ScrollText },
];

export default function App() {
  return (
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
  );
}
