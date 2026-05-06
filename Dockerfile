# ---- 阶段 1：构建前端 ----
FROM node:20-bookworm-slim AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY frontend/ .
RUN pnpm build

# ---- 阶段 2：构建 Rust 后端 ----
FROM rust:1.82-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

# ---- 阶段 3：运行时 ----
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ffmpeg \
        ca-certificates \
        wget \
        unzip && \
    rm -rf /var/lib/apt/lists/*

# N_m3u8DL-RE（Linux x64）— 从 GitHub Release 下载
ARG N3U8DL_VERSION=0.3.0-alpha
RUN wget -q "https://github.com/nilaoda/N_m3u8DL-RE/releases/download/v${N3U8DL_VERSION}/N_m3u8DL-RE_Linux_x64" \
        -O /usr/local/bin/N_m3u8DL-RE && \
    chmod +x /usr/local/bin/N_m3u8DL-RE || \
    echo "WARN: N_m3u8DL-RE download failed; provide binary via volume mount"

# Rust 二进制
COPY --from=builder /app/target/release/audio-worker /usr/local/bin/audio-worker

# 前端静态文件
COPY --from=frontend /app/frontend/dist /srv/audio-worker/static

# 默认配置
RUN mkdir -p /etc/audio-worker /data/audio-artifacts /data/temp
COPY config/settings.example.json /etc/audio-worker/settings.json

# 环境变量
ENV STATIC_DIR=/srv/audio-worker/static
ENV RUST_LOG=info

EXPOSE 3900

ENTRYPOINT ["/usr/local/bin/audio-worker"]
CMD ["--config", "/etc/audio-worker/settings.json", "--port", "3900"]
