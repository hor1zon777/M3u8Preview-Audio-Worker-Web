// pipeline/downloader.rs：用 N_m3u8DL-RE 下载 m3u8 到临时目录。
//
// 关键改动（修复 stdout pipe 死锁）：
//   - 用 proc_util::run_streamed 而非 cmd.output()，流式读 stdout/stderr
//   - 默认 30 分钟超时（大文件 + 慢链路兜底）
//   - 每行 stdout/stderr 实时落 tracing，UI Logs 页能看到实时进度
//
// 鉴权头注入：
//   - 服务端 claim job 响应的 headers 字段会带 Referer / User-Agent 等
//   - 这里转成 N_m3u8DL-RE 的 -H "Key: Value" 参数（每个 header 一个 -H）
//   - 与服务端代理 (ProxyHandler.m3u8) 保持一致，避免 worker 直连源站 403
//
// N_m3u8DL-RE 关键参数：
//   --tmp-dir <dir>        临时分片目录
//   --save-dir <dir>       最终输出目录
//   --save-name <name>     输出文件名（不含扩展）
//   --auto-select          自动选最高码率
//   --thread-count 16      并发分片下载
//   --no-log               不写日志文件
//   --del-after-done       下载后清理 tmp
//   -H "Key: Value"        请求头（可重复）
//
// 输出：work_dir/source.{mp4,ts,...}（具体扩展名由 N_m3u8DL-RE 决定）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

use super::proc_util::{run_streamed, tail};
use super::tools::Tools;

/// 默认下载超时（30 分钟），覆盖大部分长视频。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub async fn download(
    tools: &Tools,
    m3u8_url: &str,
    work_dir: &Path,
    headers: &HashMap<String, String>,
    proxy_url: &str,
) -> Result<PathBuf> {
    let save_name = "source";
    tracing::info!("[downloader] start: {}", truncate_url(m3u8_url, 200));

    let mut cmd = Command::new(&tools.m3u8dl);
    cmd.arg(m3u8_url)
        .arg("--save-dir").arg(work_dir)
        .arg("--tmp-dir").arg(work_dir.join("tmp"))
        .arg("--save-name").arg(save_name)
        .arg("--auto-select")
        .arg("--thread-count").arg("16")
        .arg("--check-segments-count").arg("false")
        .arg("--no-log")
        .arg("--del-after-done")
        .arg("--no-date-info")
        .arg("--ui-language").arg("en-US");

    // 下载代理（HTTP / HTTPS / SOCKS5）
    let trimmed_proxy = proxy_url.trim();
    if !trimmed_proxy.is_empty() {
        cmd.arg("--proxy").arg(trimmed_proxy);
        tracing::info!("[downloader] using proxy: {}", trimmed_proxy);
    }

    // 注入鉴权头（服务端按域名生成）。N_m3u8DL-RE 接受 -H "Key: Value" 形式，
    // 多个 header 通过多次 -H 传递。值不做引号包裹，由 OS arg parsing 兜底。
    for (k, v) in headers {
        // 跳过空值与 host（host 由 m3u8 URL 决定，强写会被某些下载器拒绝）
        if v.is_empty() || k.eq_ignore_ascii_case("host") {
            continue;
        }
        let header_arg = format!("{k}: {v}");
        cmd.arg("-H").arg(&header_arg);
        tracing::debug!("[downloader] header injected: {}", k);
    }

    let output = run_streamed("downloader", cmd, DEFAULT_TIMEOUT).await?;

    if !output.status.success() {
        let combined = if output.stderr.trim().is_empty() {
            output.stdout
        } else {
            format!("{}\n{}", output.stdout, output.stderr)
        };
        return Err(anyhow!(
            "N_m3u8DL-RE exit {}: {}",
            output.status,
            tail(&combined, 1500)
        ));
    }

    let downloaded = find_downloaded(work_dir, save_name)?;
    let size = std::fs::metadata(&downloaded).map(|m| m.len()).unwrap_or(0);
    tracing::info!("[downloader] done: {} ({} bytes)", downloaded.display(), size);
    if size < 1024 {
        return Err(anyhow!(
            "downloaded file suspiciously small: {} bytes",
            size
        ));
    }
    Ok(downloaded)
}

fn find_downloaded(work_dir: &Path, save_name: &str) -> Result<PathBuf> {
    let entries = std::fs::read_dir(work_dir)
        .with_context(|| format!("read work_dir {}", work_dir.display()))?;

    let prefer_ext = ["mp4", "m4a", "ts", "mka", "mkv", "aac", "wav"];

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == save_name {
            candidates.push(path);
        }
    }
    if candidates.is_empty() {
        return Err(anyhow!(
            "downloaded file not found under {} (expected stem={})",
            work_dir.display(),
            save_name
        ));
    }

    candidates.sort_by_key(|p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        prefer_ext.iter().position(|x| *x == ext).unwrap_or(usize::MAX)
    });
    Ok(candidates.remove(0))
}

fn truncate_url(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}
