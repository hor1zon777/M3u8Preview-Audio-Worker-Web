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
///
/// 之前 `stem == save_name` 严格匹配会在这些场景全部漏掉，触发
/// "downloaded file not found"。这里改为：
///   - 匹配 `stem == save_name` 或 `stem` 以 `"<save_name>."` 开头
///     （第二条覆盖 source.AVC / source.AAC / source.HEVC / source.audio 等）
///   - 跳过 work_dir/tmp/（那是分片缓存目录，--del-after-done 通常会清，但偶发残留）
///   - 按优先扩展名排序：含音频的容器优先（m4a / mp4 / ts / mka / mkv / aac / wav）
///
/// 失败时把 work_dir 顶层实际文件清单写进错误信息，便于排查到底是
/// 文件名规则又变了还是真的没下载到。
fn find_downloaded(work_dir: &Path, save_name: &str) -> Result<PathBuf> {
    let entries = std::fs::read_dir(work_dir)
        .with_context(|| format!("read work_dir {}", work_dir.display()))?;

    // 优先扩展名：m4a / aac 对 audio worker 最直接（已是音频容器）；
    // mp4 / ts / mkv 紧随其后；wav 兜底。
    let prefer_ext = ["m4a", "mp4", "ts", "mka", "mkv", "aac", "wav"];
    let prefix_with_dot = format!("{save_name}.");

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut top_level: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if !name.is_empty() {
            top_level.push(name.clone());
        }
        if !path.is_file() {
            // 跳过 tmp 分片缓存子目录，其它子目录也不递归（保持简单 + 可控）
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == save_name || stem.starts_with(&prefix_with_dot) {
            candidates.push(path);
        }
    }
    if candidates.is_empty() {
        // 兜底：N_m3u8DL-RE 偶发将产物写到 tmp 子目录内（如容器内路径解析差异）。
        // 仅扫描常见的分片子目录 tmp / .tmp，不递归整个 work_dir。
        for tmp_name in &["tmp", ".tmp"] {
            let tmp_dir = work_dir.join(tmp_name);
            if !tmp_dir.is_dir() {
                continue;
            }
            if let Ok(tmp_entries) = std::fs::read_dir(&tmp_dir) {
                for ent in tmp_entries.flatten() {
                    let p = ent.path();
                    if !p.is_file() {
                        continue;
                    }
                    let s = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if s == save_name || s.starts_with(&prefix_with_dot) {
                        // 在分片子目录找到产物：move 到 work_dir 防止 TempDir 提前清理
                        let dest = work_dir.join(
                            p.file_name().unwrap_or_default(),
                        );
                        if let Err(e) = std::fs::rename(&p, &dest) {
                            // 跨设备 rename 失败时 fallback copy+delete
                            if let Ok(()) = std::fs::copy(&p, &dest).and_then(|_| {
                                std::fs::remove_file(&p)
                            }) {
                                tracing::info!(
                                    "[downloader] moved output from {} (copy+delete fallback)",
                                    p.display()
                                );
                                candidates.push(dest);
                                break;
                            }
                            tracing::warn!(
                                "[downloader] found output {} but move failed: {}",
                                p.display(),
                                e
                            );
                            continue;
                        }
                        tracing::info!(
                            "[downloader] moved output from {} to work_dir",
                            p.display()
                        );
                        candidates.push(dest);
                        break;
                    }
                }
            }
            if !candidates.is_empty() {
                break;
            }
        }
    }

    if candidates.is_empty() {
        top_level.sort();
        let listing = if top_level.is_empty() {
            "<empty>".to_string()
        } else {
            top_level.join(", ")
        };
        return Err(anyhow!(
            "downloaded file not found under {} (expected stem={} or {}*); \
             actual entries: [{}]",
            work_dir.display(),
            save_name,
            prefix_with_dot,
            listing
        ));
    }

    candidates.sort_by_key(|p| {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let ext_rank = prefer_ext.iter().position(|x| *x == ext).unwrap_or(usize::MAX);
        // 同扩展名情况下，文件大者优先（mux 完成的整片通常比中间残片大）
        let neg_size = std::fs::metadata(p)
            .map(|m| -(m.len() as i64))
            .unwrap_or(0);
        (ext_rank, neg_size)
    });
    let picked = candidates.remove(0);
    if !candidates.is_empty() {
        // 多候选时记一行 info，方便对照实际选了哪个；不上升为 warn 避免噪音。
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

fn truncate_url(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}
