# M3u8Preview Audio Worker (Web)

M3u8Preview 音频提取 Worker 的 **Linux Web 版**，基于 Axum HTTP Server + React 前端，支持 Docker 一键部署。

> 原版为 Tauri 2 桌面应用（仅 Windows），本项目将其改造为可在 Linux 服务器上运行的 Web 服务，通过浏览器访问管理界面。

## 功能

- 从 m3u8-preview-go 服务端领取音频提取任务
- 通过 N_m3u8DL-RE 下载 M3U8 流（支持 HTTP/HTTPS/SOCKS5 代理）
- 通过 ffmpeg 提取音频并编码为 FLAC
- 注册元数据到服务端，供 subtitle worker 拉取
- Web 管理界面：运行时总览 / 任务历史 / 实时日志 / 配置管理

## 快速开始

### Docker Compose（推荐）

```bash
git clone <repo-url> && cd m3u8-preview-audio-worker-web

# 创建数据目录
mkdir -p data/config data/audio data/temp

# 复制并编辑配置
cp config/settings.example.json data/config/settings.json
# 编辑 data/config/settings.json，填入服务器地址和 token

# 构建并启动
docker-compose up -d

# 访问 http://localhost:3900
```

### Docker 手动构建

```bash
docker build -t audio-worker-web .
docker run -d \
  --name audio-worker \
  -p 3900:3900 \
  -v $(pwd)/data/config:/etc/audio-worker \
  -v $(pwd)/data/audio:/data/audio-artifacts \
  -v $(pwd)/data/temp:/data/temp \
  audio-worker-web
```

### 本地开发

```bash
# 后端
cargo run -- --config config/settings.example.json --port 3900

# 前端（另一个终端，带 API 代理）
cd frontend && pnpm install && pnpm dev
# 访问 http://localhost:1430，API 自动代理到 :3900
```

## 配置

配置文件路径由 `--config` 参数指定，默认 `/etc/audio-worker/settings.json`。

```jsonc
{
  "server": {
    "base_url": "https://m3u8.example.com",  // m3u8-preview-go 服务端地址
    "token": "mwt_xxx",                       // Worker 认证 Token
    "poll_interval_sec": 5,                   // 轮询间隔（秒）
    "heartbeat_interval_sec": 30,             // 心跳间隔（秒）
    "max_concurrent_tasks": 1                 // 最大并发任务数
  },
  "pipeline": {
    "m3u8dl_path": "",                        // N_m3u8DL-RE 路径（空 = PATH 查找）
    "ffmpeg_path": "",                        // ffmpeg 路径（空 = PATH 查找）
    "temp_dir": "/data/temp",                 // 临时文件目录
    "audio_storage_dir": "/data/audio-artifacts", // FLAC 存储目录
    "flac_compression_level": 8,              // FLAC 压缩等级 0-12
    "audio_local_max_pending": 5              // 本地暂存上限（0 = 不限流）
  },
  "network": {
    "download_proxy": ""                      // M3U8 下载代理（http/socks5）
  },
  "worker_name": "audio-worker-1"             // Worker 显示名称
}
```

所有配置项均可通过 Web 界面 Settings 页面修改，保存后即时生效。

## API 端点

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/settings` | 读取配置 |
| PUT | `/api/settings` | 保存配置 |
| GET | `/api/status` | 运行时状态 |
| POST | `/api/ping` | 测试服务器连接 |
| POST | `/api/register` | 注册 Worker |
| POST | `/api/pause` | 暂停任务轮询 |
| POST | `/api/resume` | 恢复任务轮询 |
| GET | `/api/logs?limit=200` | 获取最近日志 |
| WS | `/api/ws/logs` | WebSocket 实时日志流 |
| GET | `/api/doctor` | 工具可用性探测 |
| GET | `/api/history` | 任务历史列表 |
| GET | `/api/history/:jobId` | 任务详情 |
| DELETE | `/api/history` | 清空历史 |
| POST | `/api/retry/:mediaId` | 重试失败任务 |
| POST | `/api/validate-dir` | 校验目录可用性 |

所有响应统一为 `{success, data, message}` 信封格式。

## 架构

```
浏览器 (:3900)
  │
  ├── 静态前端 (React SPA)
  └── /api/* → Axum HTTP Server
        ├── poller     → claim 任务循环
        ├── runner     → download → extract → encode pipeline
        ├── audio_owner → broker fetch_loop（FLAC 上传）
        └── api_client → 与 m3u8-preview-go 服务端通信
```

## 依赖

**运行时**：
- ffmpeg（系统包管理器安装）
- N_m3u8DL-RE（Dockerfile 自动下载，或手动放到 PATH）

**Rust**：Axum 0.8 / tokio / reqwest / rusqlite / serde

**前端**：React 18 / React Router 7 / TanStack Query 5 / Tailwind CSS 3 / Vite 6

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `RUST_LOG` | `info` | 日志级别 |
| `STATIC_DIR` | `/srv/audio-worker/static` | 前端静态文件目录 |

## 目录挂载

| 容器路径 | 说明 |
|----------|------|
| `/etc/audio-worker/` | 配置文件 |
| `/data/audio-artifacts/` | FLAC 文件存储 |
| `/data/temp/` | 下载/编码临时文件 |

## License

MIT
