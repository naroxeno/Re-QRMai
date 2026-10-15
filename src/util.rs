//! 通用小工具：安全截断、常量时间比较、日志脱敏。
//!
//! 独立成模块是因为 main.rs 与 wechat 子模块都要用：
//! - `truncate_bytes`：替代裸 `&s[..n]`。裸切片一旦落在 UTF-8 多字节字符中间
//!   就会 panic，而 release 配置带 `panic = "abort"`，一次请求即可让整个服务
//!   进程终止（安全审计 D1）。
//! - `ct_eq`：token 比较用常量时间，避免时序侧信道（安全审计 A3/S-15）。
//! - `mask_url` / `mask_payload`：日志里不写完整 URL 与二维码内容（S-12）。

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// 按 UTF-8 字符边界安全截断到最多 `max_bytes` 字节。
///
/// 与 `&s[..max_bytes]` 的区别：宁可少截几个字节，也不会在多字节字符
/// 中间切开而 panic。
pub fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 常量时间字符串比较。
///
/// 长度不同直接返回 false（长度本身不是秘密）；长度相同时逐字节异或累加，
/// 不提前返回，避免通过响应时间逐位猜出 token。
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 日志脱敏：把 URL 中 `MAID` 后面的登录令牌（十六进制串）换成掩码，
/// 并丢弃查询串（可能带一次性凭据）。
///
/// 例：`https://wq.wahlap.net/qrcode/req/MAID1A2B3C.html?x=1`
/// → `https://wq.wahlap.net/qrcode/req/<6 位令牌>.html?…`
pub fn mask_url(url: &str) -> String {
    let mut masked = match url.find("MAID") {
        Some(idx) => {
            let head_end = idx + 4;
            // MAID 后面的连续十六进制字符即为令牌；范围都是 ASCII，切片安全
            let tail_start = url[head_end..]
                .find(|c: char| !c.is_ascii_hexdigit())
                .map(|off| head_end + off)
                .unwrap_or(url.len());
            format!(
                "{}{}{}",
                &url[..head_end],
                format!("<{} 位令牌>", url[head_end..tail_start].len()),
                &url[tail_start..]
            )
        }
        None => url.to_string(),
    };

    if let Some(q) = masked.find('?') {
        masked.truncate(q);
        masked.push_str("?…");
    }
    masked
}

/// 日志脱敏：URL 走 [`mask_url`]，其它内容（二维码里可能是一次性凭据）
/// 只保留长度，不写进日志。
pub fn mask_payload(data: &str) -> String {
    if data.starts_with("http://") || data.starts_with("https://") {
        mask_url(data)
    } else {
        format!("<{} 字节>", data.len())
    }
}

/// 生成一段不可预测的十六进制后缀（用于临时目录名等）。
///
/// 熵来自 `RandomState` 的系统随机种子 + 当前时间 + PID，无需额外依赖。
pub fn random_suffix() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(d.as_nanos());
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_bytes_is_char_boundary_safe() {
        // 每个汉字 3 字节：第 5 字节落在「中」中间，必须回退到边界
        let s = "中文中文";
        assert_eq!(truncate_bytes(s, 5), "中");
        assert_eq!(truncate_bytes(s, 3), "中");
        assert_eq!(truncate_bytes(s, 6), "中文");
        assert_eq!(truncate_bytes(s, 0), "");
        assert_eq!(truncate_bytes(s, 100), s);
        // 原来的写法会 panic，这里保证不再 panic
        assert!(std::panic::catch_unwind(|| truncate_bytes(s, 4).to_string()).is_ok());
    }

    #[test]
    fn ct_eq_matches_eq() {
        assert!(ct_eq("qrmai", "qrmai"));
        assert!(!ct_eq("qrmai", "qrmaj"));
        assert!(!ct_eq("qrmai", "qrmaiX"));
        assert!(!ct_eq("", "qrmai"));
        assert!(ct_eq("", ""));
        assert!(ct_eq("中文令牌", "中文令牌"));
    }

    #[test]
    fn mask_url_hides_token_and_query() {
        let masked = mask_url("https://wq.wahlap.net/qrcode/req/MAID1A2B3C4D.html?k=v");
        assert!(!masked.contains("1A2B3C4D"), "令牌不能出现在日志里: {masked}");
        assert!(masked.contains("<8 位令牌>"), "应标注令牌长度: {masked}");
        assert!(masked.contains("wq.wahlap.net"), "主机名保留便于排查: {masked}");
        assert!(masked.ends_with("?…"), "查询串应被丢弃: {masked}");
    }

    #[test]
    fn mask_payload_only_keeps_length_for_opaque_data() {
        let masked = mask_payload("SECRET-QR-PAYLOAD");
        assert_eq!(masked, "<17 字节>");
        assert!(!masked.contains("SECRET"));
    }

    #[test]
    fn random_suffix_is_hex_and_unique() {
        let a = random_suffix();
        let b = random_suffix();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
