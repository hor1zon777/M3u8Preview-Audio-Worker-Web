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
        // 不再禁用分片数量校验：单个 ts 分片下载失败时让 N_m3u8DL-RE 直接 fail，
        // 而不是静默继续 mux 出一个缺片的 mp4（缺片会导致下游 ffmpeg 报
        // "moov atom not found"，错误归因混乱）。
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

    // 容器完整性预检：用 ffmpeg 跑一个 100ms 的 null muxer，验证 moov atom 存在 +
    // 至少一条音频轨道可解码。命中失败则把错误归因到 download 阶段（而不是
    // 让下游 extract 拿一个缺 moov 的 mp4，最终冒出 "moov atom not found"）。
    if let Err(e) = probe_container(tools, &downloaded).await {
        return Err(anyhow!(
            "downloaded file failed container probe ({}); m3u8DL-RE produced corrupt output \
             — usually caused by missing/failed segments on source side: {e:#}",
            downloaded.display()
        ));
    }

    Ok(downloaded)
}

/// 用 ffmpeg 探测下载产物的容器完整性。
///
/// 命令等价于：`ffmpeg -v error -i <file> -t 0.1 -f null -`
///   - `-v error`：仅输出错误，moov 缺失会留下明显信号
///   - `-t 0.1`：只解 100ms，绝大多数文件 < 1s 就退
///   - `-f null -`：丢弃输出，纯检测
///
/// 失败定义：exit != 0 或 stderr 命中 "moov atom not found" / "Invalid data found"。
/// 给 30s 上限即可（远超大多数 mp4 的索引解析时间）。
async fn probe_container(tools: &Tools, file: &Path) -> Result<()> {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-v").arg("error")
        .arg("-i").arg(file)
        .arg("-t").arg("0.1")
        .arg("-f").arg("null")
        .arg("-");

    let output = run_streamed("downloader-probe", cmd, Duration::from_secs(30)).await?;

    let stderr_lc = output.stderr.to_ascii_lowercase();
    let bad_signals = [
        "moov atom not found",
        "invalid data found when processing input",
    ];
    let hit = bad_signals.iter().any(|sig| stderr_lc.contains(sig));

    if !output.status.success() || hit {
        return Err(anyhow!(
            "ffmpeg probe exit {}: {}",
            output.status,
            tail(&output.stderr, 800)
        ));
    }
    Ok(())
}

/// 在 work_dir 下定位 N_m3u8DL-RE 实际写出的产物。
///
/// 命名规则的灰色地带（曾遇到过的偶发 case）：
///   - 单流标准 mux：`source.mp4` / `source.ts` / `source.m4a`（最常见）
///   - 视频+音频分离的 master m3u8：`source.AVC.mp4` + `source.AAC.m4a`
///     （EXT-X-MEDIA AUDIO group 存在时，N_m3u8DL-RE 默认按 codec 拆名）
///   - 多 codec fallback：`source.HEVC.mp4` 等
///   - mux 阶段被跳过 / 失败但 exit=0：可能只剩分离的中间产物
///   - 产物落在 tmp 子目录内（容器内路径解析差异等偶发场景）
///
/// 搜索策略（由近及远）：
///   1. work_dir 顶层：stem == save_name 或 starts_with "save_name."
///   2. tmp / .tmp 子目录：同上规则
///   3. 递归搜索（深度 ≤ 3）：跳过 tmp 目录，匹配 stem 规则
///   4. 最终兜底：递归搜索任意媒体文件（.mp4/.ts/.m4a/.mkv/.aac/.wav）
///
/// 按优先扩展名排序：含音频的容器优先（m4a / mp4 / ts / mka / mkv / aac / wav）。
///
/// 失败时 dump 完整目录树（深度 3），便于排查。
fn find_downloaded(work_dir: &Path, save_name: &str) -> Result<PathBuf> {
    let prefer_ext = ["m4a", "mp4", "ts", "mka", "mkv", "aac", "wav"];
    let prefix_with_dot = format!("{save_name}.");
    let media_exts: &[&str] = &["mp4", "ts", "m4a", "mkv", "mka", "aac", "wav", "mp3", "ogg", "flac", "webm"];

    // Step 1: 扫描 work_dir 顶层
    let mut candidates = scan_dir_for_stem(work_dir, save_name, &prefix_with_dot)?;

    // Step 2: 扫描 tmp / .tmp 子目录
    if candidates.is_empty() {
        for tmp_name in &["tmp", ".tmp"] {
            let tmp_dir = work_dir.join(tmp_name);
            if !tmp_dir.is_dir() {
                continue;
            }
            let tmp_hits = scan_dir_for_stem(&tmp_dir, save_name, &prefix_with_dot)?;
            for hit in tmp_hits {
                // move 到 work_dir 防止 TempDir drop 时提前清理
                let dest = work_dir.join(hit.file_name().unwrap_or_default());
                match std::fs::rename(&hit, &dest) {
                    Ok(()) => {
                        tracing::info!("[downloader] moved output from {} to work_dir", hit.display());
                        candidates.push(dest);
                    }
                    Err(_) => {
                        if let Ok(()) = std::fs::copy(&hit, &dest).and_then(|_| std::fs::remove_file(&hit)) {
                            tracing::info!("[downloader] moved output from {} (copy+delete fallback)", hit.display());
                            candidates.push(dest);
                        } else {
                            tracing::warn!("[downloader] found output {} but move failed", hit.display());
                            candidates.push(hit);
                        }
                    }
                }
            }
            if !candidates.is_empty() {
                break;
            }
        }
    }

    // Step 3: 递归搜索（深度 ≤ 3，跳过 tmp 目录）
    if candidates.is_empty() {
        candidates = recursive_scan_stem(work_dir, save_name, &prefix_with_dot, 3)?;
    }

    // Step 4: 最终兜底 — 递归搜索任意媒体文件（排除 tmp 目录）
    if candidates.is_empty() {
        candidates = recursive_scan_media(work_dir, media_exts, 3)?;
        if !candidates.is_empty() {
            tracing::warn!(
                "[downloader] stem '{}' not found, falling back to any media file: {:?}",
                save_name,
                candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
            );
        }
    }

    if candidates.is_empty() {
        // dump 完整目录树用于诊断
        let tree = dump_tree(work_dir, 3);
        return Err(anyhow!(
            "downloaded file not found under {} (expected stem={} or {}*);\n\
             directory tree:\n{}",
            work_dir.display(),
            save_name,
            prefix_with_dot,
            tree
        ));
    }

    candidates.sort_by_key(|p| {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let ext_rank = prefer_ext.iter().position(|x| *x == ext).unwrap_or(usize::MAX);
        let neg_size = std::fs::metadata(p)
            .map(|m| -(m.len() as i64))
            .unwrap_or(0);
        (ext_rank, neg_size)
    });
    let picked = candidates.remove(0);
    if !candidates.is_empty() {
        let others: Vec<String> = candidates
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(String::from))
            .collect();
        tracing::info!(
            "[downloader] multiple candidates found, picked {}; others ignored: [{}]",
            picked.display(),
            others.join(", ")
        );
    }
    Ok(picked)
}

/// 扫描单个目录，返回匹配 stem 规则的文件列表。
fn scan_dir_for_stem(dir: &Path, save_name: &str, prefix_with_dot: &str) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == save_name || stem.starts_with(prefix_with_dot) {
            out.push(path);
        }
    }
    Ok(out)
}

/// 递归搜索匹配 stem 的文件（跳过 tmp / .tmp 目录，深度限制）。
fn recursive_scan_stem(
    dir: &Path,
    save_name: &str,
    prefix_with_dot: &str,
    max_depth: u32,
) -> Result<Vec<PathBuf>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for ent in entries.flatten() {
        let path = ent.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() {
            if name == "tmp" || name == ".tmp" {
                continue; // 跳过分片缓存目录
            }
            if let Ok(sub) = recursive_scan_stem(&path, save_name, prefix_with_dot, max_depth - 1) {
                out.extend(sub);
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == save_name || stem.starts_with(prefix_with_dot) {
            out.push(path);
        }
    }
    Ok(out)
}

/// 递归搜索任意媒体文件（按扩展名匹配，跳过 tmp / .tmp 目录）。
fn recursive_scan_media(dir: &Path, exts: &[&str], max_depth: u32) -> Result<Vec<PathBuf>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for ent in entries.flatten() {
        let path = ent.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() {
            if name == "tmp" || name == ".tmp" {
                continue;
            }
            if let Ok(sub) = recursive_scan_media(&path, exts, max_depth - 1) {
                out.extend(sub);
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if exts.iter().any(|x| *x == ext) {
            out.push(path);
        }
    }
    Ok(out)
}

/// dump 目录树（用于诊断 "file not found" 场景）。
fn dump_tree(dir: &Path, max_depth: u32) -> String {
    fn walk(dir: &Path, depth: u32, max: u32, out: &mut String, prefix: &str) {
        if depth > max {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut items: Vec<_> = entries.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for (i, ent) in items.iter().enumerate() {
            let path = ent.path();
            let name = ent.file_name().to_string_lossy().to_string();
            let is_last = i == items.len() - 1;
            let connector = if is_last { "└── " } else { "├── " };
            let child_prefix = if is_last { "    " } else { "│   " };
            if path.is_dir() {
                out.push_str(&format!("{}{}{}/\n", prefix, connector, name));
                walk(&path, depth + 1, max, out, &format!("{}{}", prefix, child_prefix));
            } else {
                let size = std::fs::metadata(&path)
                    .map(|m| format!(" ({} bytes)", m.len()))
                    .unwrap_or_default();
                out.push_str(&format!("{}{}{}{}\n", prefix, connector, name, size));
            }
        }
    }
    let mut out = format!("{}/\n", dir.display());
    walk(dir, 1, max_depth, &mut out, "");
    out
}

fn truncate_url(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}
