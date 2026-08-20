//! 配置模型与加载/保存（TOML 格式，自动生成带说明的配置文件）

use anyhow::{Context, Result};
use log::info;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

// 子结构体 `decode`
#[derive(Debug, Deserialize, Serialize)]
pub struct Decode {
    pub time: u64,
    #[serde(rename = "retry_count")]
    pub retry_count: u64,
}

impl Default for Decode {
    fn default() -> Self {
        Self {
            time: 10,
            retry_count: 10,
        }
    }
}

fn default_capture_mode() -> String {
    if cfg!(target_os = "linux") { "hijack".into() } else { "extension".into() }
}

fn default_p2_region_ratio_x() -> f64 {
    0.2
}

fn default_p2_region_ratio_y() -> f64 {
    0.2
}

// 主配置结构体（字段名即 TOML key）
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub p1: [u32; 2],
    pub p2: [u32; 2],
    pub token: String,
    pub host: String,
    pub port: u16,
    #[serde(rename = "qr_route")]
    pub qr_route: String,
    #[serde(rename = "cache_duration")]
    pub cache_duration: u64,
    #[serde(rename = "standalone_mode")]
    pub standalone_mode: bool,
    pub decode: Decode,
    #[serde(rename = "skin_format")]
    pub skin_format: String,
    #[serde(rename = "custom_skin_path")]
    pub custom_skin_path: String,
    #[serde(rename = "custom_skin_qrcode_size")]
    pub custom_skin_qrcode_size: u32,
    #[serde(rename = "custom_skin_qrcode_point")]
    pub custom_skin_qrcode_point: [u32; 2],
    #[serde(rename = "dev_mode")]
    pub dev_mode: bool,
    pub version: String,
    #[serde(rename = "wechat_bin")]
    pub wechat_bin: String,
    #[serde(rename = "wechat_url_timeout")]
    pub wechat_url_timeout: u64,
    #[serde(rename = "auto_detect_p1p2")]
    pub auto_detect_p1p2: bool,
    #[serde(rename = "template_threshold")]
    pub template_threshold: f64,
    /// P2 检测区宽度比例（自动识别时 = 屏幕宽 × 此值）；p2_max_dx > 0 时被覆盖
    #[serde(rename = "p2_region_ratio_x", default = "default_p2_region_ratio_x")]
    pub p2_region_ratio_x: f64,
    /// P2 检测区高度比例（自动识别时 = 屏幕高 × 此值）；p2_max_dy > 0 时被覆盖
    #[serde(rename = "p2_region_ratio_y", default = "default_p2_region_ratio_y")]
    pub p2_region_ratio_y: f64,
    /// P2 检测区手动宽度（像素）；>0 时优先于比例（检测不到分辨率或想手动指定时用）
    #[serde(rename = "p2_max_dx", default)]
    pub p2_max_dx: u32,
    /// P2 检测区手动高度（像素）；>0 时优先于比例
    #[serde(rename = "p2_max_dy", default)]
    pub p2_max_dy: u32,
    #[serde(rename = "skin_mode")]
    pub skin_mode: String,
    #[serde(rename = "skin_index")]
    pub skin_index: u32,
    #[serde(rename = "skin_images")]
    pub skin_images: Vec<String>,
    #[serde(rename = "p1_image")]
    pub p1_image: String,
    #[serde(rename = "p2_image")]
    pub p2_image: String,
    /// QR 码获取方式: "hijack" (Linux xdg-open 劫持) 或 "extension" (浏览器扩展)
    #[serde(rename = "capture_mode", default = "default_capture_mode")]
    pub capture_mode: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            p1: [1892, 1407],
            p2: [1453, 1300],
            token: "qrmai".into(),
            host: "0.0.0.0".into(),
            port: 5000,
            qr_route: "/qrmai".into(),
            cache_duration: 0,
            standalone_mode: false,
            decode: Decode::default(),
            skin_format: "new".into(),
            custom_skin_path: "./skin.png".into(),
            custom_skin_qrcode_size: 576,
            custom_skin_qrcode_point: [106, 638],
            dev_mode: false,
            version: "8d4e06be79dd88be4fbc8c40110a81bc".into(),
            wechat_bin: "/opt/wechat/wechat".into(),
            wechat_url_timeout: 5,
            auto_detect_p1p2: false,
            template_threshold: 0.6,
            p2_region_ratio_x: 0.2,
            p2_region_ratio_y: 0.2,
            p2_max_dx: 0,
            p2_max_dy: 0,
            skin_mode: "random".into(),
            skin_index: 0,
            skin_images: vec![],
            p1_image: "p1_user.png".into(),
            p2_image: "p2_user.png".into(),
            capture_mode: if cfg!(target_os = "linux") {
                "hijack".into()
            } else {
                "extension".into()
            },
        }
    }
}

/// 转义 TOML 字符串值（引号与反斜杠）
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 将配置渲染为带中文说明的 TOML。
///
/// 首次生成默认配置与设置页保存后重写均调用此函数，说明注释常驻，
/// 配置文件本身就是文档。
pub fn render_config_toml(c: &Config) -> String {
    format!(
        r#"# ============================================================
# Re-QRMai 配置文件
#
# 本文件为自动生成，注释即说明。可手动编辑后重启服务生效，
# 也可在 Web 设置页修改（保存时会按本模板重新生成）。
# ============================================================

# ── 微信窗口点击坐标（可被「自动识别位置」覆盖）───────────────
# P1: 「生成二维码」按钮的位置，格式 [X, Y]（屏幕像素坐标）
p1 = [{}, {}]
# P2: 二维码消息的位置，格式 [X, Y]
p2 = [{}, {}]

# ── 服务 ──────────────────────────────────────────────────
# 管理面板登录令牌（同时用于浏览器扩展提交鉴权）
token = "{}"
# 监听地址（0.0.0.0 = 允许局域网访问，127.0.0.1 = 仅本机）
host = "{}"
# 监听端口
port = {}
# 二维码页面访问路径（例如 /qrmai、/qrcode）
qr_route = "{}"

# ── 二维码捕获 ────────────────────────────────────────────
# 获取方式: "hijack"（Linux 劫持，默认）| "extension"（浏览器扩展，跨平台）
capture_mode = "{}"
# 微信可执行文件路径（hijack 模式使用）
wechat_bin = "{}"
# 等待扩展提交链接 / FIFO 收到 URL 的超时秒数
wechat_url_timeout = {}

# ── P2 自动识别检测区 ─────────────────────────────────────
# 检测区宽比例（0.2 = 屏幕宽的 20%），自动适配不同分辨率屏幕
p2_region_ratio_x = {}
# 检测区高比例
p2_region_ratio_y = {}
# 手动指定检测区宽（像素；填大于 0 时优先于比例，0 = 用比例）
p2_max_dx = {}
# 手动指定检测区高（像素；同上）
p2_max_dy = {}
# 模板匹配置信度阈值（0–1，越高越严格；识别不到时可调低）
template_threshold = {}
# 启动时自动识别 P1/P2 位置（需要 img/ 目录下有模板图）
auto_detect_p1p2 = {}

# ── 皮肤（预留功能） ──────────────────────────────────────
skin_format = "{}"
skin_mode = "{}"
skin_index = {}
skin_images = []
custom_skin_path = "{}"
custom_skin_qrcode_size = {}
custom_skin_qrcode_point = [{}, {}]

# ── 其他 ──────────────────────────────────────────────────
# 独立模式（预留）
standalone_mode = {}
# 二维码缓存秒数（预留，0 = 不缓存）
cache_duration = {}
# 开发模式（预留）
dev_mode = {}
# 版本标识（预留）
version = "{}"
# P1/P2 模板文件名（img/ 目录下，_user 前缀为用户上传版本）
p1_image = "{}"
p2_image = "{}"

# ── 二维码解码 ────────────────────────────────────────────
# （注意：本表必须位于文件末尾，TOML 中子表之后的裸 key 会归入子表）
[decode]
# 解码等待时间（秒）
time = {}
# 解码重试次数
retry_count = {}
"#,
        c.p1[0], c.p1[1], c.p2[0], c.p2[1],
        esc(&c.token), esc(&c.host), c.port, esc(&c.qr_route),
        esc(&c.capture_mode), esc(&c.wechat_bin), c.wechat_url_timeout,
        c.p2_region_ratio_x, c.p2_region_ratio_y, c.p2_max_dx, c.p2_max_dy,
        c.template_threshold, c.auto_detect_p1p2,
        esc(&c.skin_format), esc(&c.skin_mode), c.skin_index,
        esc(&c.custom_skin_path), c.custom_skin_qrcode_size,
        c.custom_skin_qrcode_point[0], c.custom_skin_qrcode_point[1],
        c.standalone_mode, c.cache_duration, c.dev_mode, esc(&c.version),
        esc(&c.p1_image), esc(&c.p2_image),
        c.decode.time, c.decode.retry_count,
    )
}

impl Config {
    /// 校验配置值是否合法（保存/启动前调用），返回首个错误描述
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=65535).contains(&self.port) {
            return Err(format!("端口必须在 1–65535 之间，当前 {}", self.port));
        }
        if self.host.parse::<std::net::IpAddr>().is_err() {
            return Err(format!("host 必须是有效的 IP 地址，当前 {}", self.host));
        }
        if self.token.trim().is_empty() {
            return Err("访问令牌不能为空".into());
        }
        if !(0.0..=1.0).contains(&self.template_threshold) {
            return Err(format!(
                "模板匹配置信度阈值必须在 0–1 之间，当前 {}",
                self.template_threshold
            ));
        }
        for (name, v) in [
            ("p2_region_ratio_x", self.p2_region_ratio_x),
            ("p2_region_ratio_y", self.p2_region_ratio_y),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{name} 必须在 0–1 之间，当前 {v}"));
            }
        }
        if self.wechat_url_timeout == 0 {
            return Err("wechat_url_timeout 必须大于 0".into());
        }
        Ok(())
    }
}

/// 解析 "X,Y" 格式坐标（设置表单使用）
fn parse_pair(s: &str) -> Option<[u32; 2]> {
    let mut parts = s.splitn(2, ',');
    let x: u32 = parts.next()?.trim().parse().ok()?;
    let y: u32 = parts.next()?.trim().parse().ok()?;
    Some([x, y])
}

/// 将设置页表单（key → 字符串值）应用到配置。
///
/// 解析逻辑集中在此处（原散落在 main.rs 的手写 match），
/// 未知 key 忽略；无法解析的值保持原值；
/// 表单缺失 standalone_mode 视为开关关闭。
pub fn apply_settings(c: &mut Config, form: &std::collections::BTreeMap<String, String>) {
    for (key, value) in form {
        match key.as_str() {
            "token" => c.token = value.clone(),
            "qr_route" => c.qr_route = value.clone(),
            "host" => c.host = value.clone(),
            "port" => {
                if let Ok(p) = value.parse() {
                    c.port = p;
                }
            }
            "cache_duration" => {
                if let Ok(d) = value.parse() {
                    c.cache_duration = d;
                }
            }
            "standalone_mode" => c.standalone_mode = value == "true" || value == "on",
            "skin_format" => c.skin_format = value.clone(),
            "custom_skin_path" => c.custom_skin_path = value.clone(),
            "custom_skin_qrcode_size" => {
                if let Ok(s) = value.parse() {
                    c.custom_skin_qrcode_size = s;
                }
            }
            "custom_skin_qrcode_point" => {
                if let Some(pt) = parse_pair(value) {
                    c.custom_skin_qrcode_point = pt;
                }
            }
            "decode.time" => {
                if let Ok(t) = value.parse() {
                    c.decode.time = t;
                }
            }
            "decode.retry_count" => {
                if let Ok(rc) = value.parse() {
                    c.decode.retry_count = rc;
                }
            }
            "wechat_bin" => c.wechat_bin = value.clone(),
            "wechat_url_timeout" => {
                if let Ok(t) = value.parse() {
                    c.wechat_url_timeout = t;
                }
            }
            "skin_mode" => c.skin_mode = value.clone(),
            "skin_index" => {
                if let Ok(i) = value.parse() {
                    c.skin_index = i;
                }
            }
            "p1" => {
                if let Some(pt) = parse_pair(value) {
                    c.p1 = pt;
                }
            }
            "p2" => {
                if let Some(pt) = parse_pair(value) {
                    c.p2 = pt;
                }
            }
            "p2_region_ratio_x" => {
                if let Ok(r) = value.parse() {
                    c.p2_region_ratio_x = r;
                }
            }
            "p2_region_ratio_y" => {
                if let Ok(r) = value.parse() {
                    c.p2_region_ratio_y = r;
                }
            }
            "p2_max_dx" => {
                if let Ok(v) = value.parse() {
                    c.p2_max_dx = v;
                }
            }
            "p2_max_dy" => {
                if let Ok(v) = value.parse() {
                    c.p2_max_dy = v;
                }
            }
            "capture_mode" => c.capture_mode = value.clone(),
            _ => {}
        }
    }

    // 表单中缺失 standalone_mode 字段，说明开关被关闭
    if !form.contains_key("standalone_mode") {
        c.standalone_mode = false;
    }
}

/// 生成随机访问令牌：`qrmai` + 6 位随机字符（A-Za-z0-9）。
///
/// 仅用于首次创建配置文件时生成默认 token，避免沿用公开的弱默认值。
/// 随机源来自 `RandomState` 的系统熵种子（跨平台，无需额外依赖）；
/// 注意：非密码学安全随机，若需高强度令牌请手动修改 config.toml。
pub fn generate_random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    // RandomState::new() 每次使用不同的系统熵种子
    let mut hasher = RandomState::new().build_hasher();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    hasher.write_u64(now);
    let mut x = hasher.finish();

    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut token = String::from("qrmai");
    for _ in 0..6 {
        token.push(CHARS[(x % 62) as usize] as char);
        // 线性同余扩展，避免直接取同一哈希的相邻位
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    token
}

/// 加载配置：文件存在则读取（TOML）；不存在则生成带说明的默认 TOML 并写入
/// （首次创建时 token 自动随机生成，格式 qrmaiXXXXXX）。
pub fn load_or_create_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let path = path.as_ref();
    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件: {path:?}"))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("解析 TOML 失败: {path:?}"))?;
        Ok(config)
    } else {
        let mut config = Config::default();
        config.token = generate_random_token();
        fs::write(path, render_config_toml(&config))
            .with_context(|| format!("写入默认配置文件失败: {path:?}"))?;
        info!("已创建默认配置文件: {path:?}（token 已随机生成）");
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 渲染出的带说明 TOML 应能被解析回等价配置（注释不影响解析）
    #[test]
    fn rendered_toml_roundtrips() {
        let c = Config::default();
        let toml_str = render_config_toml(&c);
        assert!(toml_str.contains("# P1"));
        let parsed: Config = toml::from_str(&toml_str).expect("渲染的 TOML 应可解析");
        assert_eq!(parsed.token, c.token);
        assert_eq!(parsed.p1, c.p1);
        assert_eq!(parsed.p2_region_ratio_x, 0.2);
        assert_eq!(parsed.decode.time, 10);
        assert_eq!(parsed.capture_mode, c.capture_mode);
    }

    /// 旧配置（不含新字段）应能正常解析，新字段走 serde default
    #[test]
    fn toml_parses_old_config_with_new_defaults() {
        let toml_str = r#"
p1 = [1892, 1407]
p2 = [1453, 1152]
token = "qrmai"
host = "0.0.0.0"
port = 5000
qr_route = "/qrmai"
cache_duration = 0
standalone_mode = false
skin_format = "new"
custom_skin_path = "./skin.png"
custom_skin_qrcode_size = 576
custom_skin_qrcode_point = [106, 638]
dev_mode = false
version = "x"
wechat_bin = "/opt/wechat/wechat"
wechat_url_timeout = 5
auto_detect_p1p2 = false
template_threshold = 0.6
skin_mode = "random"
skin_index = 0
skin_images = []
p1_image = "p1_user.png"
p2_image = "p2_user.png"
capture_mode = "hijack"

[decode]
time = 10
retry_count = 10
"#;
        let c: Config = toml::from_str(toml_str).expect("旧版 TOML 应能解析");
        assert_eq!(c.p2_region_ratio_x, 0.2);
        assert_eq!(c.p2_region_ratio_y, 0.2);
        assert_eq!(c.p2_max_dx, 0);
        assert_eq!(c.p2_max_dy, 0);
    }

    /// 渲染模板应包含所有新字段（设置页保存后注释与字段常驻）
    #[test]
    fn rendered_toml_contains_all_fields() {
        let toml_str = render_config_toml(&Config::default());
        for key in ["p2_region_ratio_x", "p2_region_ratio_y", "p2_max_dx", "p2_max_dy"] {
            assert!(toml_str.contains(key), "渲染 TOML 缺少 {key}");
        }
    }

    /// apply_settings：字段更新、无效值忽略、未知 key 忽略
    #[test]
    fn apply_settings_updates_fields() {
        use std::collections::BTreeMap;

        let mut c = Config::default();
        let mut form = BTreeMap::new();
        form.insert("token".into(), "new_token".into());
        form.insert("port".into(), "8080".into());
        form.insert("p1".into(), "100, 200".into());
        form.insert("p2_region_ratio_x".into(), "0.15".into());
        form.insert("decode.time".into(), "30".into());
        form.insert("standalone_mode".into(), "on".into());
        form.insert("unknown_key".into(), "ignored".into());
        // 无效值应被忽略，保持原值
        form.insert("p2_max_dx".into(), "not-a-number".into());

        apply_settings(&mut c, &form);
        assert_eq!(c.token, "new_token");
        assert_eq!(c.port, 8080);
        assert_eq!(c.p1, [100, 200]);
        assert_eq!(c.p2_region_ratio_x, 0.15);
        assert_eq!(c.decode.time, 30);
        assert!(c.standalone_mode);
        assert_eq!(c.p2_max_dx, 0, "无效值应保持原值");
    }

    /// 表单缺失 standalone_mode 字段 = 开关关闭
    #[test]
    fn apply_settings_turns_off_missing_switch() {
        use std::collections::BTreeMap;

        let mut c = Config::default();
        c.standalone_mode = true;
        let form = BTreeMap::new(); // 空表单
        apply_settings(&mut c, &form);
        assert!(!c.standalone_mode);
    }

    /// validate：默认配置合法；越界值报错
    #[test]
    fn validate_checks_ranges() {
        assert!(Config::default().validate().is_ok(), "默认配置应合法");

        let mut c = Config::default();
        c.port = 0;
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.template_threshold = 1.5;
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.p2_region_ratio_x = 2.0;
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.wechat_url_timeout = 0;
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.token = "  ".into();
        assert!(c.validate().is_err());
    }

    /// 随机 token：格式 qrmai + 6 位字母数字，两次生成不同
    #[test]
    fn random_token_format_and_uniqueness() {
        let a = generate_random_token();
        let b = generate_random_token();
        assert!(a.starts_with("qrmai"), "应以 qrmai 开头: {a}");
        assert_eq!(a.len(), 11, "qrmai(5) + 6 位随机: {a}");
        assert!(
            a[5..].chars().all(|c| c.is_ascii_alphanumeric()),
            "随机部分应为字母数字: {a}"
        );
        assert_ne!(a, b, "两次生成不应相同");
    }
}
