use anyhow::Result;
use log::{error, info};
use minijinja::{context, Environment};
use crate::config::{Config, load_or_create_config, render_config_toml};
use crate::mouse::MouseController;
use crate::wechat::{fetch_and_decode, WechatHijack};
use rocket::form::Form;
use rocket::fs::FileServer;
use rocket::http::{Cookie, CookieJar, ContentType, Status};
use rocket::response::content::RawHtml;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::State;
use rocket::tokio::sync::RwLock;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};
use std::net::IpAddr;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use rust_embed::Embed;

#[macro_use]
extern crate rocket;

mod config;
mod detect;
mod mouse;
mod wechat;

// 配置模型（Config/Decode/加载函数）已拆分到 src/config.rs，见 qrmai_rs::config

/// 登录表单
#[derive(FromForm)]
struct LoginForm {
    token: String,
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

/// 静态资源
#[derive(Embed)]
#[folder = "static/"]
struct Asset;
#[rocket::get("/static/<file..>")]
async fn static_files(
    file: PathBuf,
) -> Result<(ContentType, Vec<u8>), rocket::http::Status> {
    let path = file.to_string_lossy();

    // 从嵌入式资源（编译期 rust-embed 打包的 static/ 目录）获取文件
    match Asset::get(&path) {
        Some(content) => {
            // 推断 Content-Type（注意 .woff 与 .woff2 均为字体）
            let content_type = if path.ends_with(".css") {
                ContentType::CSS
            } else if path.ends_with(".js") {
                ContentType::JavaScript
            } else if path.ends_with(".png") {
                ContentType::PNG
            } else if path.ends_with(".svg") {
                ContentType::SVG
            } else if path.ends_with(".woff2") {
                ContentType::new("font", "woff2")
            } else if path.ends_with(".woff") {
                ContentType::new("font", "woff")
            } else {
                ContentType::Plain
            };

            Ok((content_type, content.data.to_vec()))
        }
        None => Err(rocket::http::Status::NotFound),
    }
}

/// 设置页 — 需要令牌鉴权，模板由 minijinja 渲染（Environment 启动时预编译并注入）
#[get("/settings")]
async fn settings_page(
    config: &State<SharedConfig>,
    cookies: &CookieJar<'_>,
    env: &State<Environment<'static>>,
) -> Result<RawHtml<String>, Redirect> {
    let c = config.read().await;
    let is_auth = cookies
        .get_private("auth_token")
        .map(|cookie| cookie.value() == c.token)
        .unwrap_or(false);

    if !is_auth {
        return Err(Redirect::to("/login"));
    }

    let tmpl = env.get_template("settings").expect("设置页模板未注册");
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
    let (threshold, ratio_x, ratio_y, manual_dx, manual_dy) = {
        let c = config.read().await;
        (
            c.template_threshold as f32,
            c.p2_region_ratio_x,
            c.p2_region_ratio_y,
            c.p2_max_dx,
            c.p2_max_dy,
        )
    };

    match detect::capture_screen() {
        Ok(screen) => {
            // 根据屏幕分辨率计算 P2 检测区大小：
            // 手动值 >0 优先（检测不到分辨率 / 用户手动指定），否则按比例 × 分辨率
            let (w, h) = (screen.width(), screen.height());
            let max_dx = if manual_dx > 0 {
                manual_dx
            } else {
                (w as f64 * ratio_x).round() as u32
            };
            let max_dy = if manual_dy > 0 {
                manual_dy
            } else {
                (h as f64 * ratio_y).round() as u32
            };
            info!(
                "[Detect] 屏幕 {w}x{h}，P2 检测区 {max_dx}x{max_dy} (比例 {ratio_x}/{ratio_y}, 手动 {manual_dx}/{manual_dy})"
            );

            match detect::detect_p1p2(&screen, Path::new("img"), threshold, max_dx, max_dy) {
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
            }
        }
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

/// 登出：清除认证 cookie 并跳转登录页
#[post("/logout")]
fn logout(cookies: &CookieJar<'_>) -> Redirect {
    cookies.remove_private(Cookie::from("auth_token"));
    Redirect::to("/login")
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

    // 应用表单（解析逻辑集中在 config::apply_settings）
    crate::config::apply_settings(&mut c, &form);

    // 校验配置合法性
    if let Err(e) = c.validate() {
        error!("[QRMai] 配置校验失败: {e}");
        return Err(Status::UnprocessableEntity);
    }

    // 写入配置文件（带说明的 TOML，注释常驻）
    let toml_str = render_config_toml(&c);
    fs::write("config.toml", toml_str).map_err(|_| Status::InternalServerError)?;

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
        fetch_and_decode(&url)
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

// ── 检查更新（GitHub API） ──────────────────────────────

/// GitHub 最新 release 信息（/check_update 用）
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: String,
    published_at: String,
    body: Option<String>,
}

/// 比较远程 tag 与当前版本：remote > current？
/// 支持 "v1.2.3" / "1.2.3" 等正式版本号；预发布（含 "-"，如 beta/rc）不视为更新
fn version_greater(remote: &str, current: &str) -> bool {
    // 预发布 tag（如 v0.1.0-beta.1）不算比当前正式版新
    if remote.contains('-') {
        return false;
    }
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    }
    let r = parts(remote);
    let c = parts(current);
    for (a, b) in r.iter().zip(c.iter()) {
        if a != b {
            return a > b;
        }
    }
    r.len() > c.len()
}

/// 检查更新：抓取 GitHub 最新 release，与当前版本（编译期 CARGO_PKG_VERSION）比较
#[post("/check_update")]
fn check_update() -> Json<serde_json::Value> {
    const REPO: &str = "SodaCodeSave/QRmai";
    const CURRENT: &str = env!("CARGO_PKG_VERSION");
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");

    match ureq::get(&url)
        .header("User-Agent", "Re-QRMai")
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(resp) => {
            let text = resp
                .into_body()
                .read_to_string()
                .map_err(|e| format!("读取 GitHub 响应失败: {e}"));
            match text.and_then(|t| {
                serde_json::from_str::<GithubRelease>(&t).map_err(|e| format!("解析 GitHub 响应失败: {e}"))
            }) {
                Ok(release) => {
                    let has_update = version_greater(&release.tag_name, CURRENT);
                    info!("[QRMai] 检查更新: 远程 {} / 当前 v{CURRENT}", release.tag_name);
                    Json(serde_json::json!({
                        "has_update": has_update,
                        "version": release.tag_name,
                        "name": release.name,
                        "published_at": release.published_at,
                        "body": release.body.unwrap_or_default(),
                        "message": if has_update {
                            format!("发现新版本: {}", release.tag_name)
                        } else {
                            format!("当前已是最新版本 v{CURRENT}")
                        },
                    }))
                }
                Err(e) => Json(serde_json::json!({
                    "error": true,
                    "message": e,
                })),
            }
        }
        Err(ureq::Error::StatusCode(404)) => {
            Json(serde_json::json!({
                "has_update": false,
                "message": "暂无已发布的版本",
            }))
        }
        Err(e) => Json(serde_json::json!({
            "error": true,
            "message": format!("检查更新失败: {e}"),
        })),
    }
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

    let config = load_or_create_config("config.toml").expect("Failed to load or create config");

    // ── 确保 img/ 目录及默认模板存在 ──
    ensure_img_dir();
    // 保存 qr_route 和 host/port 用于后续使用（config 将被 move 到 RwLock 中）
    let qr_route = config.qr_route.clone();
    let host = config.host.clone();
    let port = config.port;

    // ── 初始化微信劫持环境（仅 Linux） ──
    #[cfg(target_os = "linux")]
    let hijack = {
        match WechatHijack::init(&config.wechat_bin) {
            Ok(mut h) => {
                if !h.is_wechat_alive() && let Err(e) = h.launch_wechat() {
                    error!("[QRMai] 微信启动失败: {e}，QR 功能不可用");
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

    // ── 预编译设置页模板（只初始化一次，请求时复用，避免每次重新编译） ──
    let mut template_env = Environment::new();
    template_env
        .add_template("settings", include_str!("../templates/settings.html"))
        .expect("Failed to compile settings template");

    let rocket_config = rocket::Config {
        address: IpAddr::from_str(&host).expect("Invalid host in config"),
        port,
        ..rocket::Config::debug_default()
    };

    let _rocket = rocket::custom(rocket_config)
        .manage(shared_config)
        .manage(qr_cache)
        .manage(hijack_state)
        .manage(template_env)
        .mount(&qr_route, routes![qrmai_handler, qrmai_url_handler])
        .mount(
            "/",
            routes![
                index,
                login_page,
                settings_page,
                login,
                logout,
                save_settings,
                mouse_position,
                detect_positions,
                check_update,
                static_files
            ],
        );

    let _rocket = _rocket
        .mount("/img", FileServer::from("img"))
        .launch()
        .await?;

    Ok(())
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    /// 设置页 / 登录页模板应能编译并渲染（minijinja 语法回归保护）
    #[test]
    fn templates_compile_and_render() {
        let mut env = Environment::new();
        env.add_template("settings", include_str!("../templates/settings.html"))
            .expect("settings 模板编译失败");
        env.add_template("login", include_str!("../templates/login.html"))
            .expect("login 模板编译失败");

        let c = crate::config::Config::default();
        env.get_template("settings")
            .unwrap()
            .render(context! {
                config => &c,
                is_linux => cfg!(target_os = "linux"),
            })
            .expect("settings 渲染失败");

        env.get_template("login")
            .unwrap()
            .render(context! {})
            .expect("login 渲染失败");
    }

    /// 版本比较：远程 tag 与当前版本
    #[test]
    fn version_compare() {
        // 远程较新
        assert!(version_greater("v0.2.0", "0.1.0"));
        assert!(version_greater("v1.0.0", "v0.9.9"));
        assert!(version_greater("v0.1.1", "0.1.0"));
        // 相同 / 较旧 / 预发布
        assert!(!version_greater("v0.1.0", "0.1.0"));
        assert!(!version_greater("v0.0.9", "0.1.0"));
        assert!(!version_greater("v0.1.0-beta.1", "0.1.0"));
        // 更多段（0.1.0.1 > 0.1.0）
        assert!(version_greater("v0.1.0.1", "0.1.0"));
    }
}
