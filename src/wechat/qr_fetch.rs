//! URL 抓取与二维码解码（纯函数 + 网络/解码流程）

use anyhow::{Context, Result};
use log::info;
use regex::Regex;

// ── QR 解码（zedbar 纯 Rust）─────────────────────────────

/// 二维码图片最大边长（像素）：真实二维码图片远小于此，
/// 用于挡住「几 KB 的 PNG 解出几百 MB 位图」的解码炸弹。
const MAX_QR_IMAGE_SIDE: u32 = 4096;

/// 使用 zedbar 解码 PNG 图片中的二维码
///
/// 安全审计 S-07：图片来自网络，解码前先限制尺寸与内存占用。
pub fn decode_qr_from_bytes(data: &[u8]) -> Result<String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .context("无法识别图片格式")?;

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_QR_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_QR_IMAGE_SIDE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);

    let gray = reader.decode().context("无法解析图片")?.into_luma8();
    let (width, height) = gray.dimensions();

    let mut img = zedbar::Image::from_gray(gray.as_raw(), width, height)
        .context("无法创建 zedbar 图像")?;
    let mut scanner = zedbar::Scanner::new();
    let symbols = scanner.scan(&mut img);

    for symbol in symbols {
        if let Some(data) = symbol.data_string() {
            let qr_data = data.trim().to_string();
            if !qr_data.is_empty() {
                // 安全审计 S-12：二维码内容是一次性登录凭据，不写进日志
                info!("[Wechat] 二维码解码成功: {}", crate::util::mask_payload(&qr_data));
                return Ok(qr_data);
            }
        }
    }

    anyhow::bail!("zedbar 未识别到二维码")
}

// ── URL 抓取 ─────────────────────────────────────────────

/// MAID 图片 src 正则（编译一次，缓存）
fn maid_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"<img\s+[^>]*src="([^"]*MAID[^"]*\\.png[^"]*)""#).unwrap()
    })
}

/// 任意 img src 正则（fallback）
fn any_img_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"<img\s+[^>]*src="([^"]+)""#).unwrap())
}

/// 从 HTML 中提取二维码图片 src：优先 MAID 规则，fallback 到任意 img
pub fn extract_qr_img_src(html: &str) -> Option<String> {
    if let Some(cap) = maid_re().captures(html) {
        return Some(cap[1].to_string());
    }
    any_img_re().captures(html).map(|c| c[1].to_string())
}

/// 将图片 src 与页面 URL 合并为完整 URL
///
/// - 绝对 http(s) src：原样返回；
/// - 根路径 src（/xxx）：取 base_url 的 scheme://host 拼接（如 https://a.b/qrcode/x.png）；
/// - 相对路径：按「页面路径目录 + src」拼接。
pub fn join_url(base_url: &str, img_src: &str) -> String {
    if img_src.starts_with("http") {
        return img_src.to_string();
    }
    if let Some(path) = img_src.strip_prefix('/') {
        // 根路径：提取 scheme://host 作为 origin
        if let Some(scheme_end) = base_url.find("://") {
            let rest = &base_url[scheme_end + 3..];
            let origin_end = scheme_end + 3 + rest.find('/').unwrap_or(rest.len());
            return format!("{}/{path}", &base_url[..origin_end]);
        }
        return format!("{base_url}{img_src}");
    }
    let base = base_url.rsplit_once('/').map(|(b, _)| b).unwrap_or(base_url);
    format!("{base}/{img_src}")
}

// ── 出网白名单与体积上限（安全审计 S-06 / S-07 / S-13）──────

/// 允许抓取的主机后缀：微信登录二维码只可能来自 wahlap 的域名。
///
/// 原来对任意 URL 直接 `ureq::get`，等于把服务变成 SSRF 跳板
/// （可扫内网端口、读云元数据）。若实际部署里二维码图片在别的 CDN 上，
/// 把该域名后缀加到这里即可。
const ALLOWED_HOST_SUFFIXES: &[&str] = &["wahlap.net", "wahlap.com"];

/// 从 URL 解析主机名（小写，去掉端口与 userinfo）；非 http(s) 返回 None
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // `https://wahlap.net@evil.com/` 这类 userinfo 骗不过解析：真实主机在最后一个 '@' 之后
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    if host_port.is_empty() || host_port.starts_with('[') {
        return None; // 空主机 / IPv6 字面量
    }
    let host = host_port.split(':').next().unwrap_or(host_port);
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// URL 是否指向允许抓取的主机（精确匹配或子域）
pub fn is_allowed_url(url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    // IP 字面量一律拒绝（127.0.0.1 / 192.168.x.x / 169.254.169.254 …）
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    ALLOWED_HOST_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// 页面 HTML 体积上限（真实页面只有几 KB）
const MAX_HTML_BYTES: u64 = 1024 * 1024;
/// 二维码图片体积上限
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// 抓取文本响应，带体积上限（安全审计 S-07）
fn fetch_text(url: &str, ua: &str, limit: u64) -> Result<String> {
    let mut body = ureq::get(url)
        .header("User-Agent", ua)
        .call()
        .context("无法访问链接")?
        .into_body();
    body.with_config()
        .limit(limit)
        .read_to_string()
        .context("读取响应失败（或响应体超过体积上限）")
}

/// 抓取二进制响应，带体积上限
fn fetch_bytes(url: &str, ua: &str, limit: u64) -> Result<Vec<u8>> {
    let mut body = ureq::get(url)
        .header("User-Agent", ua)
        .call()
        .context("下载二维码图片失败")?
        .into_body();
    body.with_config()
        .limit(limit)
        .read_to_vec()
        .context("读取图片数据失败（或图片超过体积上限）")
}

/// 请求 MAID 页面 → 提取二维码图片 → 下载 → 解码，返回二维码内容
pub fn fetch_and_decode(url: &str) -> Result<String> {
    let ua = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 \
              (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";

    // 白名单校验：拒绝内网与任意域名（S-06）
    if !is_allowed_url(url) {
        anyhow::bail!("拒绝抓取非白名单链接: {}", crate::util::mask_url(url));
    }

    // 日志脱敏 + 安全截断（S-12 / S-03）
    let shown = crate::util::mask_url(url);
    info!("[Wechat] 请求页面: {}", crate::util::truncate_bytes(&shown, 120));

    // 1. 请求微信打开的 HTML 页面
    let html = fetch_text(url, ua, MAX_HTML_BYTES)?;

    // 2. 提取二维码图片 src（纯函数，已单测）
    let img_src = extract_qr_img_src(&html).context("HTML 中未找到二维码图片链接")?;

    // 合并为完整 URL（纯函数，已单测）
    let img_url = join_url(url, &img_src);

    // 二级 SSRF 防护：页面里指向任意地址的 <img src> 同样要过白名单（S-08）
    if !is_allowed_url(&img_url) {
        anyhow::bail!("拒绝抓取非白名单图片地址: {}", crate::util::mask_url(&img_url));
    }
    let shown_img = crate::util::mask_url(&img_url);
    info!("[Wechat] 二维码图片: {}", crate::util::truncate_bytes(&shown_img, 120));

    // 3. 下载二维码图片
    let img_data = fetch_bytes(&img_url, ua, MAX_IMAGE_BYTES)?;

    // 4. 解码
    decode_qr_from_bytes(&img_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_maid_img_src() {
        let html = r#"<html><body><img src="/qrcode/MAID123.png" width="200"></body></html>"#;
        assert_eq!(
            extract_qr_img_src(html).as_deref(),
            Some("/qrcode/MAID123.png")
        );
    }

    #[test]
    fn extract_fallback_to_first_img() {
        let html = r#"<img src="https://cdn.example.com/x.png"><img src="/y.png">"#;
        assert_eq!(
            extract_qr_img_src(html).as_deref(),
            Some("https://cdn.example.com/x.png")
        );
    }

    #[test]
    fn extract_none_without_img() {
        assert_eq!(extract_qr_img_src("<html>no image</html>"), None);
    }

    #[test]
    fn join_absolute_src_passthrough() {
        assert_eq!(
            join_url("https://a.b/c/d.html", "https://cdn.x/y.png"),
            "https://cdn.x/y.png"
        );
    }

    #[test]
    fn join_relative_src() {
        // 相对路径按「页面路径目录 + src」拼接
        assert_eq!(
            join_url("https://a.b/c/d.html", "qrcode/x.png"),
            "https://a.b/c/qrcode/x.png"
        );
        // 根路径按「scheme://host + src」拼接
        assert_eq!(
            join_url("https://a.b/c/d.html", "/qrcode/x.png"),
            "https://a.b/qrcode/x.png"
        );
        // 无路径的 base
        assert_eq!(
            join_url("https://a.b", "/qrcode/x.png"),
            "https://a.b/qrcode/x.png"
        );
    }

    /// SSRF 白名单：只放行 wahlap 域名，内网/云元数据/伪装域名一律拒绝
    #[test]
    fn ssrf_allowlist_blocks_internal_and_foreign_hosts() {
        // 合法：微信二维码只来自 wahlap
        assert!(is_allowed_url("https://wq.wahlap.net/qrcode/req/MAID1A2B.html"));
        assert!(is_allowed_url("https://wahlap.com/x.png"));
        assert!(is_allowed_url("http://CDN.wahlap.net/a.png"));

        for bad in [
            "http://127.0.0.1:5000/login",
            "http://localhost/qrmai",
            "http://169.254.169.254/latest/meta-data/",
            "http://192.168.1.1/",
            "https://wahlap.net.evil.com/x",
            "https://wahlap.net@evil.com/x",
            "file:///etc/passwd",
            "https://[::1]/x",
            "not-a-url",
        ] {
            assert!(!is_allowed_url(bad), "不应允许抓取: {bad}");
        }
    }

    /// 页面里的 <img src> 经 join_url 后必须过同一套白名单（二级 SSRF）
    #[test]
    fn joined_img_url_is_checked_by_same_allowlist() {
        let page = "https://wq.wahlap.net/qrcode/req/MAID1A2B.html";
        assert!(is_allowed_url(&join_url(page, "/qrcode/req/x.png")));
        assert!(!is_allowed_url(&join_url(
            page,
            "http://127.0.0.1:5000/static/icon.png"
        )));
    }
}
