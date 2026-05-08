// pipeline/m3u8_parser.rs：拉取并解析 m3u8 文本，累加 #EXTINF 得到音频/视频总时长。
//
// 用途：runner 在 download 之前先拿到"预期总时长"，编码完成后用它做合理性校验，
// 拒绝 lenient ffmpeg 抢救出的 0.1MB 残品（duration 远小于预期）。
//
// 协议覆盖：
//   - master playlist（含 #EXT-X-STREAM-INF）：自动解析第一个 variant 的 URL 并递归
//   - media playlist（含 #EXTINF:N.NNN, ...）：累加每个 #EXTINF 的秒数
//   - 多级 master 嵌套：最大递归深度 3，避免病态构造
//
// 注意：此模块只做**只读 GET 解析**，不下载任何分段，对源站无副作用。

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

const MAX_RECURSION_DEPTH: u8 = 3;
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 拉取 m3u8 并返回总时长（秒）。
///
/// 复用 downloader 同款 headers（如 Referer / User-Agent，避免 403）和下载代理。
/// 失败返回 Err；调用方可选择降级（仅记 warn 跳过校验）。
pub async fn fetch_total_duration_sec(
    m3u8_url: &str,
    headers: &HashMap<String, String>,
    proxy_url: &str,
) -> Result<f64> {
    fetch_inner(m3u8_url, headers, proxy_url, 0).await
}

async fn fetch_inner(
    m3u8_url: &str,
    headers: &HashMap<String, String>,
    proxy_url: &str,
    depth: u8,
) -> Result<f64> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(anyhow!(
            "m3u8 master/variant nested deeper than {} levels (suspected loop)",
            MAX_RECURSION_DEPTH
        ));
    }

    let text = fetch_text(m3u8_url, headers, proxy_url).await?;

    // 简单合法性校验
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Err(anyhow!("m3u8 body empty"));
    }
    if !trimmed.starts_with("#EXTM3U") {
        return Err(anyhow!("not a valid m3u8 (missing #EXTM3U marker)"));
    }

    // 主播放列表：递归解析第一个 variant
    if text.contains("#EXT-X-STREAM-INF") {
        let variant = pick_first_variant(&text, m3u8_url)
            .ok_or_else(|| anyhow!("master playlist has no parseable variant URL"))?;
        tracing::debug!(
            "[m3u8-parser] master detected, descending to variant: {}",
            truncate(&variant, 200)
        );
        // Box::pin 因为递归 async fn 类型大小未知
        return Box::pin(fetch_inner(&variant, headers, proxy_url, depth + 1)).await;
    }

    // 媒体播放列表：累加 #EXTINF
    let total = sum_extinf(&text);
    if total <= 0.0 {
        return Err(anyhow!(
            "no #EXTINF segments found in playlist (or all parse failed)"
        ));
    }
    Ok(total)
}

async fn fetch_text(url: &str, headers: &HashMap<String, String>, proxy_url: &str) -> Result<String> {
    let mut builder = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        // m3u8 站点 SNI / 证书参差不齐，与 ApiClient 保持一致放宽
        .danger_accept_invalid_certs(true)
        .user_agent(concat!("M3u8PreviewAudioWorker/", env!("CARGO_PKG_VERSION")));
    let trimmed_proxy = proxy_url.trim();
    if !trimmed_proxy.is_empty() {
        match reqwest::Proxy::all(trimmed_proxy) {
            Ok(p) => {
                builder = builder.proxy(p);
            }
            Err(e) => {
                tracing::warn!("[m3u8-parser] invalid proxy {}: {}", trimmed_proxy, e);
            }
        }
    }
    let client = builder.build().context("build reqwest client")?;
    let mut req = client.get(url);
    for (k, v) in headers {
        if v.is_empty() || k.eq_ignore_ascii_case("host") {
            continue;
        }
        req = req.header(k, v);
    }
    let resp = req.send().await.context("GET m3u8")?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("m3u8 GET returned {}", status));
    }
    let text = resp.text().await.context("read m3u8 body")?;
    Ok(text)
}

/// 提取 master playlist 中第一个 #EXT-X-STREAM-INF 后面的 URL。
///
/// 协议规定 STREAM-INF tag 的下一行（跳过空行）就是 variant URL，可能是绝对或相对路径。
fn pick_first_variant(text: &str, base_url: &str) -> Option<String> {
    let mut want_url = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("#EXT-X-STREAM-INF") {
            want_url = true;
            continue;
        }
        if want_url {
            if t.starts_with('#') {
                // 边缘情况：tag 之间还有别的 tag，继续等
                continue;
            }
            return resolve_url(base_url, t).ok();
        }
    }
    None
}

/// 解析 m3u8 文本中 #EXTINF 的累计秒数。
///
/// 格式：`#EXTINF:<duration>,<title>` 或 `#EXTINF:<duration>`。
/// 个别行解析失败时跳过，不打断累加。
fn sum_extinf(text: &str) -> f64 {
    text.lines()
        .filter_map(|line| {
            let t = line.trim();
            t.strip_prefix("#EXTINF:")
        })
        .filter_map(|s| {
            let head = s.split(',').next().unwrap_or("");
            head.trim().parse::<f64>().ok()
        })
        .filter(|x| x.is_finite() && *x >= 0.0)
        .sum()
}

/// 把相对 URL 解析成绝对 URL（针对 m3u8 协议常见的几种写法）。
///
/// 不引入 `url` crate 依赖：手动覆盖以下场景：
///   - `http://...` / `https://...`：原样返回
///   - `//host/path`：补 base 的 scheme
///   - `/abs/path`：替换为 base 的 scheme://host
///   - `relative/path`：替换为 base 同目录
///
/// 不处理：URL 中的 query string 在 base 路径段拼接时会被忽略（与浏览器/curl 行为一致）。
fn resolve_url(base: &str, href: &str) -> Result<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return Ok(href.to_string());
    }
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = base
            .split("://")
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("https");
        return Ok(format!("{scheme}://{rest}"));
    }
    let scheme_end = base
        .find("://")
        .ok_or_else(|| anyhow!("base url missing scheme: {base}"))?;
    let after_scheme = &base[scheme_end + 3..];
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let scheme_host = &base[..scheme_end + 3 + host_end];
    if href.starts_with('/') {
        return Ok(format!("{scheme_host}{href}"));
    }
    // 相对路径：base 砍到最后一个 '/' 当父目录
    // base 形如 https://h/x/y/play.m3u8?t=1 → 父目录 https://h/x/y/
    // query 不参与拼接
    let base_no_query = base.split('?').next().unwrap_or(base);
    let parent = base_no_query
        .rsplit_once('/')
        .map(|(p, _)| p)
        .unwrap_or(scheme_host);
    Ok(format!("{parent}/{href}"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_extinf_basic() {
        let text = "#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
#EXTINF:5.760,
seg0.ts
#EXTINF:6.000,
seg1.ts
#EXTINF:4.240,
seg2.ts
#EXT-X-ENDLIST
";
        assert!((sum_extinf(text) - 16.0).abs() < 1e-6);
    }

    #[test]
    fn sum_extinf_skips_bad_lines() {
        let text = "#EXTM3U
#EXTINF:abc,bad
#EXTINF:3.5,good
";
        assert!((sum_extinf(text) - 3.5).abs() < 1e-6);
    }

    #[test]
    fn pick_variant_relative() {
        let text = "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1280000
720p.m3u8
";
        let got = pick_first_variant(text, "https://h.example.com/path/index.m3u8").unwrap();
        assert_eq!(got, "https://h.example.com/path/720p.m3u8");
    }

    #[test]
    fn pick_variant_absolute() {
        let text = "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1280000
https://cdn.example.com/720p.m3u8
";
        let got = pick_first_variant(text, "https://h.example.com/index.m3u8").unwrap();
        assert_eq!(got, "https://cdn.example.com/720p.m3u8");
    }

    #[test]
    fn resolve_url_protocol_relative() {
        let got = resolve_url("https://a.com/x.m3u8", "//cdn.example/seg.ts").unwrap();
        assert_eq!(got, "https://cdn.example/seg.ts");
    }

    #[test]
    fn resolve_url_absolute_path() {
        let got = resolve_url("https://a.com/dir/x.m3u8", "/seg/0.ts").unwrap();
        assert_eq!(got, "https://a.com/seg/0.ts");
    }

    #[test]
    fn resolve_url_relative_path() {
        let got = resolve_url("https://a.com/dir/x.m3u8?token=1", "0.ts").unwrap();
        assert_eq!(got, "https://a.com/dir/0.ts");
    }
}
