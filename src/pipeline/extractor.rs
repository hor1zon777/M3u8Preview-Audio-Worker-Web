// pipeline/extractor.rs：用 ffmpeg 把下载产物转成 16kHz mono PCM WAV。
//
// 关键改动：用流式 subprocess 调用，避免 stderr 输出过多导致 pipe 死锁。
// ffmpeg 默认 `-loglevel warning` 已经很安静，但仍以流式模式跑保险。
//
// 为什么是 16kHz mono：
//   whisper.cpp / whisper-cli 的输入要求是 16kHz 单声道 PCM。
//   不是 16kHz 它会内部 resample 浪费时间；不是 mono 它会取第一路；
//   直接给标准格式最快最稳。

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

use super::proc_util::{run_streamed, tail};
use super::tools::Tools;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// 默认抽音超时 10 分钟（即使是 4 小时长视频，单纯抽音也很快）。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub async fn extract_wav(
    tools: &Tools,
    input: &Path,
    work_dir: &Path,
) -> Result<PathBuf> {
    let out = work_dir.join("audio.wav");
    tracing::info!("[extractor] {} -> {}", input.display(), out.display());

    // 第一次尝试：标准参数（覆盖 99%+ 正常 case）
    match try_extract(tools, input, &out, false).await {
        Ok(()) => {}
        Err(first_err) => {
            // 兜底重试：仅在错误信号命中可恢复关键字时触发，避免对 io/timeout 类
            // 错误做无意义重试。上游 downloader.probe_container 已先 fail 掉大多数
            // 缺 moov 的产物；走到这里说明 mp4 头能开但解码路径有损，仍值得
            // -err_detect ignore_err -fflags +genpts 抢救一次（部分音频帧可解出）。
            let msg = format!("{first_err}").to_ascii_lowercase();
            let recoverable = msg.contains("moov atom not found")
                || msg.contains("invalid data found when processing input");
            if !recoverable {
                return Err(first_err);
            }
            tracing::warn!(
                "[extractor] first pass failed with recoverable signal, retrying lenient: {first_err}"
            );
            let _ = std::fs::remove_file(&out);
            try_extract(tools, input, &out, true)
                .await
                .map_err(|e| anyhow!("first pass: {first_err}; lenient retry also failed: {e}"))?;
        }
    }

    let meta = std::fs::metadata(&out).with_context(|| format!("stat {}", out.display()))?;
    if meta.len() < 1024 {
        return Err(anyhow!(
            "ffmpeg produced suspiciously tiny audio ({} bytes), input may have no audio stream",
            meta.len()
        ));
    }
    tracing::info!("[extractor] done: {} bytes", meta.len());
    Ok(out)
}

/// 单次 ffmpeg 抽音；`lenient=true` 时打开宽容解码标志，用于损坏 mp4 的兜底。
async fn try_extract(tools: &Tools, input: &Path, out: &Path, lenient: bool) -> Result<()> {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.arg("-hide_banner").arg("-loglevel").arg("warning").arg("-y");
    if lenient {
        // 解码层：忽略 CRC / marker 错误；时间戳层：丢失则重新生成
        cmd.arg("-err_detect").arg("ignore_err").arg("-fflags").arg("+genpts");
    }
    cmd.arg("-i").arg(input)
        .arg("-vn") // 丢弃视频流
        .arg("-ac").arg("1") // 单声道
        .arg("-ar").arg(TARGET_SAMPLE_RATE.to_string()) // 16kHz
        .arg("-c:a").arg("pcm_s16le") // 16-bit 整数 PCM（whisper-cli 接受）
        .arg("-f").arg("wav")
        .arg(out);

    let tag = if lenient { "ffmpeg-lenient" } else { "ffmpeg" };
    let output = run_streamed(tag, cmd, DEFAULT_TIMEOUT).await?;
    if !output.status.success() {
        return Err(anyhow!(
            "ffmpeg exit {}: {}",
            output.status,
            tail(&output.stderr, 1500)
        ));
    }
    Ok(())
}
