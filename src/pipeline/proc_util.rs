// pipeline/proc_util.rs：subprocess 通用辅助。
//
// 关键修复：tokio::process::Command::output() 等命令退出时若 stdout/stderr
// pipe 缓冲（默认 ~64KB）被子进程写满会死锁。我们用流式读 stdout/stderr，
// 每行 log 一次（前缀 [tool/stream]），同时收集到 String 用于错误诊断。
//
// 还提供超时控制：超时则发送 kill 信号并 wait。

use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub struct ProcOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// 跑一个命令，流式读 stdout/stderr 到 tracing，整体超时 timeout。
///
/// `tag` 用于日志前缀，例如 "downloader" / "ffmpeg" / "whisper-cli"。
/// 超时则发 kill，返回 Err。
pub async fn run_streamed(
    tag: &'static str,
    mut cmd: Command,
    timeout_dur: Duration,
) -> Result<ProcOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("[{tag}] spawn failed"))?;

    let pid = child.id();
    tracing::info!("[{tag}] subprocess started (pid={:?})", pid);

    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

    let stdout_handle = tokio::spawn(stream_lines(tag, "out", stdout));
    let stderr_handle = tokio::spawn(stream_lines(tag, "err", stderr));

    // 等待子进程退出，或超时则 kill
    let status = tokio::select! {
        r = child.wait() => r.context("wait subprocess")?,
        _ = tokio::time::sleep(timeout_dur) => {
            tracing::warn!("[{tag}] timeout after {:?}, killing pid={:?}", timeout_dur, pid);
            let _ = child.start_kill();
            // 给 1s 让 OS 处理 SIGKILL
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            return Err(anyhow!("[{tag}] timeout after {:?}", timeout_dur));
        }
    };

    let stdout = stdout_handle.await.unwrap_or_default();
    let stderr = stderr_handle.await.unwrap_or_default();
    Ok(ProcOutput { status, stdout, stderr })
}

async fn stream_lines<R: tokio::io::AsyncRead + Unpin>(
    tag: &'static str,
    stream: &'static str,
    reader: R,
) -> String {
    let mut buf = BufReader::new(reader);
    let mut collected = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        match buf.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim_end_matches(&['\n', '\r'][..]);
                if !trimmed.is_empty() {
                    tracing::info!("[{tag}/{stream}] {}", trimmed);
                }
                collected.push_str(&line);
                // 避免 collected 无限增长（错误信息时只需要末尾 1500 字符）
                if collected.len() > 8 * 1024 {
                    let cut = collected.len() - 4 * 1024;
                    collected = collected.split_off(cut);
                }
            }
            Err(e) => {
                tracing::warn!("[{tag}/{stream}] read error: {e}");
                break;
            }
        }
    }
    collected
}

/// 截取字符串末尾 max 字符（用于错误信息）。
pub fn tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let skip = s.chars().count() - max;
        let result: String = s.chars().skip(skip).collect();
        format!("…{result}")
    }
}
