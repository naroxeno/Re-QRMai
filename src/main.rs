use anyhow::{Context, Result};
use log::{error, info};
use minijinja::{context, Environment};
use rocket::form::Form;
use rocket::fs::FileServer;
use rocket::http::{Cookie, CookieJar, ContentType, Status};
use rocket::response::content::RawHtml;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::State;
use rocket::tokio::sync::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{BufReader, Cursor, Write};
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

#[macro_use]
extern crate rocket;

mod mouse;
mod wechat;
mod detect;

use mouse::MouseController;
use wechat::WechatHijack;

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

// 主配置结构体
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
            template_threshold: 0.8,
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

/// 登录表单
#[derive(FromForm)]
struct LoginForm {
    token: String,
}

/// 加载配置：文件存在则读取，不存在则创建默认配置并写入
pub fn load_or_create_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let path = path.as_ref();
    if path.exists() {
        let file =
            File::open(path).with_context(|| format!("无法打开配置文件: {path:?}"))?;
        let reader = BufReader::new(file);
        let config: Config =
            serde_json::from_reader(reader).with_context(|| format!("解析 JSON 失败: {path:?}"))?;
        Ok(config)
    } else {
        let config = Config::default();
        let json = serde_json::to_string_pretty(&config)
            .context("序列化默认配置失败")?;
        fs::write(path, json)
            .with_context(|| format!("写入默认配置文件失败: {path:?}"))?;
        info!("已创建默认配置文件: {path:?}");
        Ok(config)
    }
}

/// 从文件路径读取并解析配置（要求文件必须存在）
pub fn read_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let file =
        File::open(&path).with_context(|| format!("无法打开配置文件: {:?}", path.as_ref()))?;
    let reader = BufReader::new(file);
    let config: Config = serde_json::from_reader(reader)
        .with_context(|| format!("解析 JSON 失败: {:?}", path.as_ref()))?;
    Ok(config)
}

/// 确保 img/ 目录存在，并写入默认模板图片（嵌入在二进制中）
fn ensure_img_dir() {
    let img_dir = Path::new("img");
    if !img_dir.exists() {
        fs::create_dir_all(img_dir).expect("无法创建 img/ 目录");
        info!("[Init] 已创建 img/ 目录");
    }

    // 写入默认 P1 模板（如果不存在）
    let p1_path = img_dir.join("p1.png");
    if !p1_path.exists() {
        fs::write(&p1_path, include_bytes!("../img/p1.png"))
            .expect("无法写入默认 p1.png 模板");
        info!("[Init] 已创建默认模板: {p1_path:?}");
    }

    // 写入默认 P2 模板（如果不存在）
    let p2_path = img_dir.join("p2.png");
    if !p2_path.exists() {
        fs::write(&p2_path, include_bytes!("../img/p2.png"))
            .expect("无法写入默认 p2.png 模板");
        info!("[Init] 已创建默认模板: {p2_path:?}");
    }

    // 写入 README（如果不存在）
    let readme_path = img_dir.join("README.txt");
    if !readme_path.exists() {
        fs::write(&readme_path, include_str!("../img/README.txt"))
            .expect("无法写入 img/README.txt");
        info!("[Init] 已创建 img/README.txt");
    }
}

// ── 路由 ──────────────────────────────────────────────

/// 共享的可变 Config 类型（异步读写锁，不阻塞 tokio 工作线程）
pub type SharedConfig = Arc<RwLock<Config>>;

/// QR 码缓存：扩展模式下暂存最新解码结果
pub type QrCache = Arc<RwLock<Option<(String, std::time::Instant)>>>;
pub struct HijackState(pub Option<Arc<std::sync::Mutex<WechatHijack>>>);

/// 首页 / 登录页（静态编译）
#[get("/")]
fn index() -> RawHtml<&'static str> {
    RawHtml(include_str!("../templates/login.html"))
}

/// 登录页
#[get("/login")]
fn login_page() -> RawHtml<&'static str> {
    RawHtml(include_str!("../templates/login.html"))
}

/// 设置页 — 需要令牌鉴权，模板由 minijinja 渲染
#[get("/settings")]
async fn settings_page(
    config: &State<SharedConfig>,
    cookies: &CookieJar<'_>,
) -> Result<RawHtml<String>, Redirect> {
    let c = config.read().await;
    let is_auth = cookies
        .get_private("auth_token")
        .map(|cookie| cookie.value() == c.token)
        .unwrap_or(false);

    if !is_auth {
        return Err(Redirect::to("/login"));
    }

    let mut env = Environment::new();
    env.add_template("settings", include_str!("../templates/settings.html"))
        .expect("Failed to compile settings template");
    let tmpl = env.get_template("settings").unwrap();
    let html = tmpl
        .render(context! {
            config => &*c,
            is_linux => cfg!(target_os = "linux"),
        })
        .expect("Failed to render settings template");
    Ok(RawHtml(html))
}

/// 获取当前光标坐标（settings 页面「自动识别位置」功能用）
#[get("/mouse_position")]
fn mouse_position() -> Json<serde_json::Value> {
    let mc = MouseController::new();
    match mc {
        Ok(mc) => match mc.position() {
            Some((x, y)) => Json(serde_json::json!({"x": x, "y": y})),
            None => Json(serde_json::json!({"error": "无法读取光标位置，请安装 hyprctl 或 xdotool"})),
        },
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

/// QR 二维码获取路由 — 绑定到 config.qr_route（默认 /qrmai）
///
/// 劫持模式：点击 P1 → P2 → FIFO 拦截 URL → 解码 → 返回 PNG
/// 扩展模式：点击 P1 → P2 → 轮询浏览器扩展提交的缓存 → 返回 PNG
#[get("/")]
async fn qrmai_handler(
    config: &State<SharedConfig>,
    hijack_state: &State<HijackState>,
    qr_cache: &State<QrCache>,
) -> Result<(ContentType, Vec<u8>), Status> {
    let (capture_mode, p1, p2, timeout) = {
        let c = config.read().await;
        (c.capture_mode.clone(), c.p1, c.p2, c.wechat_url_timeout)
    };

    if capture_mode == "extension" {
        // ── 扩展模式：点击 P1 → P2 → 轮询缓存 ──

        // 清空旧缓存
        {
            let mut cache = qr_cache.write().await;
            *cache = None;
        }

        // 在阻塞线程中执行鼠标点击
        let hijack_opt = hijack_state.0.clone();
        rocket::tokio::task::spawn_blocking(move || {
            let mut mouse = MouseController::new()?;
            if let Some(hijack_arc) = hijack_opt {
                let mut hijack = hijack_arc.lock()
                    .map_err(|e| anyhow::anyhow!("Lock error: {e}"))?;
                hijack.click_p1p2(&mut mouse, p1, p2)
            } else {
                // 非 Linux 平台：直接模拟点击，无需微信劫持
                info!("[QRMai] 点击 P1 ({p1:?}) 生成二维码");
                mouse.move_click(p1[0] as i32, p1[1] as i32, 100)?;
                std::thread::sleep(std::time::Duration::from_secs(2));
                info!("[QRMai] 点击 P2 ({p2:?})");
                mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;
                mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;
                Ok(())
            }
        })
        .await
        .map_err(|_| Status::InternalServerError)?
        .map_err(|e| {
            error!("[QRMai] 鼠标点击失败: {e}");
            Status::InternalServerError
        })?;

        // 轮询缓存，等待浏览器扩展提交
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
        loop {
            {
                let cache = qr_cache.read().await;
                if let Some((ref data, _)) = *cache {
                    info!("[QRMai] 从扩展缓存获取二维码: {}...", &data[..data.len().min(50)]);
                    return qr_png_response(data);
                }
            }
            if std::time::Instant::now() > deadline {
                break;
            }
            rocket::tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        error!("[QRMai] 等待扩展提交链接超时 ({}s)", timeout);
        return Err(Status::InternalServerError);
    }

    // ── 劫持模式：点击 P1 → P2 → FIFO 拦截 → 解码 ──
    let hijack = hijack_state
        .0
        .as_ref()
        .ok_or_else(|| {
            error!("[QRMai] 劫持模式仅在 Linux 上可用");
            Status::InternalServerError
        })?
        .clone();

    let result = rocket::tokio::task::spawn_blocking(move || {
        let mut mouse = MouseController::new()?;
        let mut hijack = hijack.lock().map_err(|e| anyhow::anyhow!("Lock error: {e}"))?;
        hijack.qr_action(&mut mouse, p1, p2, timeout)
    })
    .await
    .map_err(|_| Status::InternalServerError)?;

    match result {
        Ok(qr_data) => {
            info!("[QRMai] 二维码获取成功: {}...", &qr_data[..qr_data.len().min(50)]);
            qr_png_response(&qr_data)
        }
        Err(e) => {
            error!("[QRMai] 二维码获取失败: {e}");
            Err(Status::InternalServerError)
        }
    }
}

/// 将 QR 字符串编码为 PNG 返回
fn qr_png_response(data: &str) -> Result<(ContentType, Vec<u8>), Status> {
    let code = qrcode::QrCode::new(data).map_err(|_| Status::InternalServerError)?;
    let img = code.render::<image::Luma<u8>>().build();
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|_| Status::InternalServerError)?;
    Ok((ContentType::PNG, buf))
}

/// 自动识别 P1/P2 位置（GPU 加速模板匹配）
#[post("/detect_positions")]
async fn detect_positions(config: &State<SharedConfig>) -> Json<serde_json::Value> {
    let threshold = config.read().await.template_threshold as f32;

    match detect::capture_screen() {
        Ok(screen) => match detect::detect_p1p2(&screen, Path::new("img"), threshold) {
            Ok((p1, p2)) => {
                let mut resp = serde_json::json!({});
                if let Some(p) = p1 {
                    resp["p1"] = serde_json::json!(p);
                }
                if let Some(p) = p2 {
                    resp["p2"] = serde_json::json!(p);
                }
                if p1.is_none() && p2.is_none() {
                    resp["error"] = serde_json::json!("未找到 P1 或 P2 模板，请上传模板图片到 img/ 目录");
                }
                Json(resp)
            }
            Err(e) => Json(serde_json::json!({"error": e.to_string()})),
        },
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

#[post("/login", data = "<form>")]
async fn login(
    config: &State<SharedConfig>,
    cookies: &CookieJar<'_>,
    form: Form<LoginForm>,
) -> Json<serde_json::Value> {
    let token = config.read().await.token.clone();
    let success = form.token == token;
    if success {
        cookies.add_private(Cookie::new("auth_token", form.into_inner().token));
    }
    Json(serde_json::json!({"success": success}))
}

/// 保存配置 — 接收表单数据，更新到内存并写入 config.json
#[post("/settings", data = "<form>")]
async fn save_settings(
    config: &State<SharedConfig>,
    cookies: &CookieJar<'_>,
    form: Form<BTreeMap<String, String>>,
) -> Result<Json<serde_json::Value>, Status> {
    // 鉴权
    {
        let c = config.read().await;
        let is_auth = cookies
            .get_private("auth_token")
            .map(|cookie| cookie.value() == c.token)
            .unwrap_or(false);
        if !is_auth {
            return Err(Status::Forbidden);
        }
    }

    let form = form.into_inner();
    let mut c = config.write().await;

    // 辅助函数：解析 "X,Y" 格式坐标
    fn parse_pair(s: &str) -> Option<[u32; 2]> {
        let mut parts = s.splitn(2, ',');
        let x: u32 = parts.next()?.trim().parse().ok()?;
        let y: u32 = parts.next()?.trim().parse().ok()?;
        Some([x, y])
    }

    for (key, value) in &form {
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
            "capture_mode" => c.capture_mode = value.clone(),
            _ => {}
        }
    }

    // 如果表单中没有 standalone_mode 字段，说明开关被关闭了
    if !form.contains_key("standalone_mode") {
        c.standalone_mode = false;
    }

    // 写入配置文件
    let json = serde_json::to_string_pretty(&*c).map_err(|_| Status::InternalServerError)?;
    fs::write("config.json", json).map_err(|_| Status::InternalServerError)?;

    Ok(Json(serde_json::json!({"success": true})))
}

/// 浏览器扩展提交的二维码 URL 处理（跨平台方案）
///
/// 浏览器扩展拦截到微信打开的 MAID 链接后，通过此端点提交
#[post("/url", format = "json", data = "<body>")]
async fn qrmai_url_handler(
    config: &State<SharedConfig>,
    qr_cache: &State<QrCache>,
    body: Json<QrUrlPayload>,
) -> Result<(ContentType, Vec<u8>), Status> {
    // Token 验证
    {
        let c = config.read().await;
        if body.token != c.token {
            return Err(Status::Forbidden);
        }
    }

    let url = body.url.clone();

    // 在阻塞线程中执行网络请求 + 解码
    let qr_data = rocket::tokio::task::spawn_blocking(move || {
        wechat::fetch_and_decode(&url)
    })
    .await
    .map_err(|_| Status::InternalServerError)?
    .map_err(|e| {
        error!("[QRMai] 扩展提交的链接解码失败: {e}");
        Status::InternalServerError
    })?;

    info!("[QRMai] 扩展提交的链接解码成功: {}...", &qr_data[..qr_data.len().min(50)]);

    // 写入 QR 缓存（供 GET /qrmai 扩展模式读取）
    {
        let mut cache = qr_cache.write().await;
        *cache = Some((qr_data.clone(), std::time::Instant::now()));
    }

    // 生成 QR 图片
    let code = qrcode::QrCode::new(&qr_data).map_err(|_| Status::InternalServerError)?;
    let img = code.render::<image::Luma<u8>>().build();
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|_| Status::InternalServerError)?;

    Ok((ContentType::PNG, buf))
}

// ── 数据结构 ──────────────────────────────────────────────

/// 浏览器扩展提交的 JSON 载荷
#[derive(Deserialize)]
struct QrUrlPayload {
    url: String,
    token: String,
}

// ── 日志初始化 ──────────────────────────────────────────

/// 计算当天的日志文件基础名（格式：YYYY-MM-DD-序号）
fn log_basename() -> String {
    use time::OffsetDateTime;

    let now = OffsetDateTime::now_utc();
    let date_str = format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );

    // 扫描 log/ 目录，计算当天第 N 次运行
    let mut count = 0u32;
    if let Ok(entries) = std::fs::read_dir("log") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&date_str) && name.ends_with(".log") {
                count += 1;
            }
        }
    }

    format!("{date_str}-{}", count + 1)
}

/// 终端日志格式（带颜色）：[HH:MM:SS] LEVEL message
fn stderr_format(
    w: &mut dyn Write,
    now: &mut flexi_logger::DeferredNow,
    record: &log::Record,
) -> std::io::Result<()> {
    let level_color = match record.level() {
        log::Level::Error => "[1;31m",
        log::Level::Warn  => "[1;33m",
        log::Level::Info  => "[1;32m",
        log::Level::Debug => "[1;34m",
        log::Level::Trace => "[1;35m",
    };
    write!(
        w,
        "[{}] {}{:<5}[0m {}",
        now.format("%H:%M:%S"),
        level_color,
        record.level(),
        record.args()
    )
}

/// 文件日志格式（纯文本）：[YYYY-MM-DD HH:MM:SS] LEVEL message
fn file_format(
    w: &mut dyn Write,
    now: &mut flexi_logger::DeferredNow,
    record: &log::Record,
) -> std::io::Result<()> {
    write!(
        w,
        "[{}] {:<5} {}",
        now.format("%Y-%m-%d %H:%M:%S"),
        record.level(),
        record.args()
    )
}

/// 初始化 flexi_logger：彩色终端输出 + 写入 log/ 目录
fn init_logger() {
    let basename = log_basename();
    let file_spec = flexi_logger::FileSpec::default()
        .directory("log")
        .basename(&basename)
        .suppress_timestamp();

    flexi_logger::Logger::try_with_env_or_str("info")
        .unwrap()
        .format_for_files(file_format)
        .format_for_stderr(stderr_format)
        .log_to_file(file_spec)
        .duplicate_to_stderr(flexi_logger::Duplicate::All)
        .start()
        .unwrap();
}

// ── 启动入口 ──────────────────────────────────────────

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    // ── 初始化日志系统 ──
    init_logger();

    let config = load_or_create_config("config.json").expect("Failed to load or create config");

    // ── 确保 img/ 目录及默认模板存在 ──
    ensure_img_dir();

    info!("读取到的配置: {:#?}", config);
    info!("Token: {}", config.token);
    info!("Port: {}", config.port);

    // 保存 qr_route 和 host/port 用于后续使用（config 将被 move 到 RwLock 中）
    let qr_route = config.qr_route.clone();
    let host = config.host.clone();
    let port = config.port;

    // ── 初始化微信劫持环境（仅 Linux） ──
    #[cfg(target_os = "linux")]
    let hijack = {
        match WechatHijack::init(&config.wechat_bin) {
            Ok(mut h) => {
                if !h.is_wechat_alive() {
                    if let Err(e) = h.launch_wechat() {
                        error!("[QRMai] 微信启动失败: {e}，QR 功能不可用");
                    }
                }
                Some(Arc::new(std::sync::Mutex::new(h)))
            }
            Err(e) => {
                error!("[QRMai] 微信劫持环境创建失败: {e}，QR 功能不可用");
                None
            }
        }
    };
    #[cfg(not(target_os = "linux"))]
    let hijack: Option<Arc<std::sync::Mutex<WechatHijack>>> = None;

    let hijack_state = HijackState(hijack);

    let shared_config: SharedConfig = Arc::new(RwLock::new(config));
    let qr_cache: QrCache = Arc::new(RwLock::new(None));

    let rocket_config = rocket::Config {
        address: IpAddr::from_str(&host).expect("Invalid host in config"),
        port,
        ..rocket::Config::debug_default()
    };

    let _rocket = rocket::custom(rocket_config)
        .manage(shared_config)
        .manage(qr_cache)
        .manage(hijack_state)
        .mount(&qr_route, routes![qrmai_handler, qrmai_url_handler])
        .mount(
            "/",
            routes![
                index,
                login_page,
                settings_page,
                login,
                save_settings,
                mouse_position,
                detect_positions
            ],
        );

    let _rocket = _rocket
        .mount("/img", FileServer::from("img"))
        .mount("/extension", FileServer::from("extension"))
        .launch()
        .await?;

    Ok(())
}
