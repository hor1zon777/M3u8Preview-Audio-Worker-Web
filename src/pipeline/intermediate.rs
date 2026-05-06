// pipeline/intermediate.rs：把 16 kHz mono PCM WAV 编成 FLAC，并算 SHA-256 + 探测时长。
//
// 三个职责：
//   1. encode_flac：调 ffmpeg 把 WAV → FLAC（无损 + 50% 压缩 vs WAV）
//   2. sha256_and_size：流式算 SHA256 + 文件字节数（spawn_blocking 避免阻塞 tokio runtime）
//   3. probe_duration_ms：用 ffmpeg -i 拿 Duration 行解析时长
//
// 与服务端 audio-complete 协议的对应：
//   - encode_flac 产物 = 上传文件
//   - sha256_and_size = meta.sha256 + meta.size
//   - probe_duration_ms = meta.durationMs

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use tokio::process::Command;

use super::proc_util::{run_streamed, tail};
use super::tools::Tools;

/// FLAC 编码：调 ffmpeg。
///
/// `compression_level` 取值 0..=12（更高更小，但 CPU 上升缓慢）；服务端不感知此参数。
pub async fn encode_flac(
    tools: &Tools,
    wav: &Path,
    work_dir: &Path,
    compression_level: u8,
    timeout: Duration,
) -> Result<PathBuf> {
    let level = compression_level.min(12);
    let out = work_dir.join("audio.flac");
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .arg("-y")
        .arg("-i")
        .arg(wav)
        .arg("-c:a")
        .arg("flac")
        .arg("-compression_level")
        .arg(level.to_string())
        .arg("-sample_fmt")
        .arg("s16")
        .arg(&out);
    let output = run_streamed("ffmpeg-flac", cmd, timeout).await?;
    if !output.status.success() {
        return Err(anyhow!(
            "ffmpeg flac exit {}: {}",
            output.status,
            tail(&output.stderr, 1500)
        ));
    }
    let size = std::fs::metadata(&out).context("stat flac")?.len();
    if size < 1024 {
        return Err(anyhow!("flac too small: {} bytes", size));
    }
    tracing::info!(
        "[intermediate] flac done: {} bytes (level={})",
        size,
        level
    );
    Ok(out)
}

/// 流式计算 SHA-256 + 文件大小。spawn_blocking 避免大文件 I/O 阻塞 tokio runtime。
pub async fn sha256_and_size(path: &Path) -> Result<(i64, String)> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(i64, String)> {
        use std::io::Read;
        let mut f = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut total: i64 = 0;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as i64;
        }
        let hex = hex::encode(hasher.finalize());
        Ok((total, hex))
    })
    .await?
}

/// 用 ffmpeg 探测时长（毫秒）。
///
/// 简单做法：`ffmpeg -i <audio>` 没有输出文件会非零退出，但 stderr 含 `Duration: HH:MM:SS.mmm`。
pub async fn probe_duration_ms(tools: &Tools, audio: &Path) -> Result<i64> {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.arg("-hide_banner").arg("-i").arg(audio);
    let output = run_streamed("ffprobe-via-ffmpeg", cmd, Duration::from_secs(30)).await?;
    parse_ffmpeg_duration(&output.stderr)
}

/// 从 ffmpeg stderr 中提取 `"Duration: HH:MM:SS.mmm"` 行。
fn parse_ffmpeg_duration(stderr: &str) -> Result<i64> {
    for line in stderr.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Duration: ") {
            // "Duration: 00:01:23.45, start: ..."
            let head: &str = rest.split(',').next().unwrap_or("");
            return parse_hms_to_ms(head);
        }
    }
    Err(anyhow!("ffmpeg stderr has no Duration line"))
}

fn parse_hms_to_ms(s: &str) -> Result<i64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(anyhow!("bad duration format: {s}"));
    }
    let h: f64 = parts[0].parse()?;
    let m: f64 = parts[1].parse()?;
    let sec: f64 = parts[2].parse()?;
    Ok(((h * 3600.0 + m * 60.0 + sec) * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_basic() {
        let s = "  Duration: 01:23:45.67, start: 0.000000, bitrate: 128 kb/s";
        assert_eq!(parse_ffmpeg_duration(s).unwrap(), 5_025_670);
    }

    #[test]
    fn parse_duration_zero() {
        let s = "Duration: 00:00:00.00, start: 0";
        assert_eq!(parse_ffmpeg_duration(s).unwrap(), 0);
    }

    #[test]
    fn parse_duration_missing_returns_err() {
        let s = "no duration here";
        assert!(parse_ffmpeg_duration(s).is_err());
    }

    #[tokio::test]
    async fn sha256_consistency() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.bin");
        std::fs::write(&p, b"hello world").unwrap();
        let (size, sha) = sha256_and_size(&p).await.unwrap();
        assert_eq!(size, 11);
        assert_eq!(
            sha,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
