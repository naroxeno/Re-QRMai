//! URL 抓取与二维码解码（纯函数 + 网络/解码流程）

use anyhow::{Context, Result};
use log::info;
use regex::Regex;

// ── QR 解码（zedbar 纯 Rust）─────────────────────────────

/// 使用 zedbar 解码 PNG 图片中的二维码
pub fn decode_qr_from_bytes(data: &[u8]) -> Result<String> {
    let gray = image::load_from_memory(data)
        .context("无法解析图片")?
        .into_luma8();
    let (width, height) = gray.dimensions();

    let mut img = zedbar::Image::from_gray(gray.as_raw(), width, height)
        .context("无法创建 zedbar 图像")?;
    let mut scanner = zedbar::Scanner::new();
    let symbols = scanner.scan(&mut img);

    for symbol in symbols {
        if let Some(data) = symbol.data_string() {
            let qr_data = data.trim().to_string();
            if !qr_data.is_empty() {
                info!("[Wechat] 二维码解码成功: {}...", &qr_data[..qr_data.len().min(50)]);
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

/// 请求 MAID 页面 → 提取二维码图片 → 下载 → 解码，返回二维码内容
pub fn fetch_and_decode(url: &str) -> Result<String> {
    let ua = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 \
              (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";

    info!("[Wechat] 请求页面: {}...", &url[..url.len().min(80)]);

    // 1. 请求微信打开的 HTML 页面
    let html = ureq::get(url)
        .header("User-Agent", ua)
        .call()
        .context("无法访问微信链接")?
        .into_body()
        .read_to_string()
        .context("读取 HTML 失败")?;

    // 2. 提取二维码图片 src（纯函数，已单测）
    let img_src = extract_qr_img_src(&html).context("HTML 中未找到二维码图片链接")?;

    // 合并为完整 URL（纯函数，已单测）
    let img_url = join_url(url, &img_src);
    info!("[Wechat] 二维码图片: {}...", &img_url[..img_url.len().min(80)]);

    // 3. 下载二维码图片
    let img_data = ureq::get(&img_url)
        .header("User-Agent", ua)
        .call()
        .context("下载二维码图片失败")?
        .into_body()
        .read_to_vec()
        .context("读取图片数据失败")?;

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
}
