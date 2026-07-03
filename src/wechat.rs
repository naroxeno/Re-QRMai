//! Linux WeChat 劫持模块
//!
//! 通过伪装 `xdg-open` + FIFO 管道拦截微信打开的 MAID 链接，实现二维码自动获取。
//!
//! 支持崩溃恢复：程序退出时可选择保留劫持环境，下次启动自动恢复，
//! 无需重新创建 FIFO / 伪装脚本 / 重启微信。

use crate::mouse::MouseController;
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

// ── 状态持久化 ──────────────────────────────────────────

const STATE_FILE: &str = "/tmp/qrmai_state.json";

#[derive(Serialize, Deserialize)]
struct HijackState {
    wechat_pid: u32,
    temp_dir: String,
    fifo_path: String,
    fake_bin_dir: String,
}

/// 通过发送信号 0 检查指定 PID 的进程是否存活
fn pid_is_alive(pid: u32) -> bool {
    // kill(pid, 0) 不发送信号，仅检查进程是否存在
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

// ── 伪装的 xdg-open ──────────────────────────────────────

fn create_fake_xdg_open(fake_bin_dir: &Path, fifo_path: &Path) -> Result<()> {
    fs::create_dir_all(fake_bin_dir)
        .with_context(|| format!("创建伪装目录失败: {fake_bin_dir:?}"))?;

    let xdg_open = fake_bin_dir.join("xdg-open");
    let script = format!(
        r#"#!/bin/bash
URL="$1"
if [[ "$URL" =~ ^https?://wq\.wahlap\.net/qrcode/req/MAID[0-9A-Fa-f]+\.html ]]; then
    echo "$URL" > "{}"
    exit 0
else
    unset BROWSER
    exec /usr/bin/xdg-open "$@"
fi
"#,
        fifo_path.display()
    );

    fs::write(&xdg_open, script)
        .with_context(|| format!("写入伪装 xdg-open 失败: {xdg_open:?}"))?;

    let mut perms = fs::metadata(&xdg_open)
        .with_context(|| format!("读取权限失败: {xdg_open:?}"))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&xdg_open, perms)
        .with_context(|| format!("设置可执行权限失败: {xdg_open:?}"))?;

    println!("[Wechat] 已创建伪装的 xdg-open: {xdg_open:?}");
    Ok(())
}

// ── FIFO 监听 ───────────────────────────────────────────

fn spawn_fifo_listener(
    fifo_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        println!("[Wechat] FIFO 监听线程已启动");
        while !stop_flag.load(Ordering::Relaxed) && fifo_path.exists() {
            let file = match fs::File::open(&fifo_path) {
                Ok(f) => f,
                Err(_) => {
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
            };
            for line in BufReader::new(file).lines() {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(url) = line {
                    let url = url.trim().to_string();
                    if !url.is_empty() {
                        println!("[Wechat] 截获链接: {url}");
                        let _ = tx.send(url);
                    }
                }
            }
            thread::sleep(Duration::from_millis(200));
        }
        println!("[Wechat] FIFO 监听线程已退出");
    });

    rx
}

// ── 微信进程启动 ────────────────────────────────────────

fn launch_wechat(wechat_bin: &str, fake_bin_dir: &Path) -> Result<Child> {
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    println!("[Wechat] 启动微信: dbus-run-session {wechat_bin}");

    Command::new("dbus-run-session")
        .arg(wechat_bin)
        .env("PATH", &path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("启动微信失败: {wechat_bin}"))
}

// ── QR 解码（zedbar 纯 Rust）─────────────────────────────

/// 使用 zedbar 解码 PNG 图片中的二维码
fn decode_qr_from_bytes(data: &[u8]) -> Result<String> {
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
                println!("[Wechat] 二维码解码成功: {}...", &qr_data[..qr_data.len().min(50)]);
                return Ok(qr_data);
            }
        }
    }

    anyhow::bail!("zedbar 未识别到二维码")
}

// ── URL 获取与二维码解码 ─────────────────────────────────

fn fetch_and_decode(url: &str) -> Result<String> {
    let ua = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 \
              (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";

    println!("[Wechat] 请求页面: {}...", &url[..url.len().min(80)]);

    // 1. 请求微信打开的 HTML 页面
    let html = ureq::get(url)
        .header("User-Agent", ua)
        .call()
        .context("无法访问微信链接")?
        .into_body()
        .read_to_string()
        .context("读取 HTML 失败")?;

    // 2. 正则提取 MAID 图片 src
    let re = Regex::new(r#"<img\s+[^>]*src="([^"]*MAID[^"]*\.png[^"]*)""#).unwrap();
    let img_src = match re.captures(&html) {
        Some(cap) => cap[1].to_string(),
        None => {
            let fallback = Regex::new(r#"<img\s+[^>]*src="([^"]+)""#).unwrap();
            fallback
                .captures(&html)
                .map(|c| c[1].to_string())
                .context("HTML 中未找到二维码图片链接")?
        }
    };

    // URL join
    let img_url = if img_src.starts_with("http") {
        img_src
    } else {
        let base = url.rsplit_once('/').map(|(b, _)| b).unwrap_or(url);
        format!("{base}/{img_src}")
    };
    println!("[Wechat] 二维码图片: {}...", &img_url[..img_url.len().min(80)]);

    // 3. 下载二维码图片
    let img_data = ureq::get(&img_url)
        .header("User-Agent", ua)
        .call()
        .context("下载二维码图片失败")?
        .into_body()
        .read_to_vec()
        .context("读取图片数据失败")?;

    // 4. zbarimg 解码
    decode_qr_from_bytes(&img_data)
}

// ── WechatHijack ────────────────────────────────────────

/// Linux 微信劫持环境管理器
pub struct WechatHijack {
    temp_dir: PathBuf,
    fake_bin_dir: PathBuf,
    fifo_path: PathBuf,
    wechat_proc: Option<Child>,
    wechat_pid: Option<u32>,
    stop_flag: Arc<AtomicBool>,
    url_rx: Mutex<mpsc::Receiver<String>>,
    wechat_bin: String,
    /// 是否从崩溃恢复（跳过微信启动询问）
    recovered: bool,
}

impl WechatHijack {
    // ── 初始化入口：先尝试恢复，再全新创建 ──

    /// 初始化劫持环境（优先从上次崩溃恢复）
    pub fn init(wechat_bin: &str) -> Result<Self> {
        if let Some(hijack) = Self::try_recover() {
            println!("[Wechat] ♻ 已从上次会话恢复劫持环境");
            return Ok(hijack);
        }
        Self::create_fresh(wechat_bin)
    }

    /// 全新创建劫持环境
    pub fn create_fresh(wechat_bin: &str) -> Result<Self> {
        let temp_dir = std::env::temp_dir().join(format!("qrmai_{}", std::process::id()));
        fs::create_dir_all(&temp_dir)
            .with_context(|| format!("创建临时目录失败: {temp_dir:?}"))?;

        let fake_bin_dir = temp_dir.join(".local_bin");
        let fifo_path = temp_dir.join(".link_pipe");

        // 创建 FIFO
        let status = Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .context("mkfifo 命令失败，请确认系统支持命名管道")?;
        if !status.success() {
            anyhow::bail!("mkfifo 返回非零退出码");
        }
        println!("[Wechat] 已创建 FIFO: {fifo_path:?}");

        create_fake_xdg_open(&fake_bin_dir, &fifo_path)?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let url_rx = Mutex::new(spawn_fifo_listener(fifo_path.clone(), stop_flag.clone()));

        Ok(Self {
            temp_dir,
            fake_bin_dir,
            fifo_path,
            wechat_proc: None,
            wechat_pid: None,
            stop_flag,
            url_rx,
            wechat_bin: wechat_bin.to_string(),
            recovered: false,
        })
    }

    /// 尝试从状态文件恢复劫持环境
    fn try_recover() -> Option<Self> {
        let state_path = Path::new(STATE_FILE);
        if !state_path.exists() {
            return None;
        }

        let json = fs::read_to_string(state_path).ok()?;
        let state: HijackState = serde_json::from_str(&json).ok()?;

        // 检查微信进程是否仍在运行
        if !pid_is_alive(state.wechat_pid) {
            println!("[Wechat] 上次的微信进程 (PID {}) 已退出，将创建新环境", state.wechat_pid);
            let _ = fs::remove_file(state_path);
            return None;
        }

        // 检查关键文件是否完整
        let temp_dir = PathBuf::from(&state.temp_dir);
        let fifo_path = PathBuf::from(&state.fifo_path);
        let fake_bin_dir = PathBuf::from(&state.fake_bin_dir);
        let xdg_open = fake_bin_dir.join("xdg-open");

        if !temp_dir.exists() || !fifo_path.exists() || !xdg_open.exists() {
            println!("[Wechat] 上次的劫持环境文件不完整，将重建");
            let _ = fs::remove_file(state_path);
            let _ = fs::remove_dir_all(&temp_dir);
            return None;
        }

        // 恢复成功：复用已有环境
        let stop_flag = Arc::new(AtomicBool::new(false));
        let url_rx = Mutex::new(spawn_fifo_listener(fifo_path.clone(), stop_flag.clone()));

        println!("[Wechat] ♻ 已恢复劫持环境:");
        println!("         微信 PID: {}", state.wechat_pid);
        println!("         临时目录: {temp_dir:?}");
        println!("         FIFO:     {fifo_path:?}");

        Some(Self {
            temp_dir,
            fake_bin_dir,
            fifo_path,
            wechat_proc: None,
            wechat_pid: Some(state.wechat_pid),
            stop_flag,
            url_rx,
            wechat_bin: String::new(), // 恢复时不需要 wechat_bin
            recovered: true,
        })
    }

    /// 保存当前劫持环境状态到文件（供崩溃后恢复）
    fn save_state(&self) {
        let pid = match self.wechat_pid {
            Some(p) => p,
            None => match &self.wechat_proc {
                Some(proc) => proc.id(),
                None => {
                    eprintln!("[Wechat] 没有微信 PID 可保存");
                    return;
                }
            },
        };

        let state = HijackState {
            wechat_pid: pid,
            temp_dir: self.temp_dir.display().to_string(),
            fifo_path: self.fifo_path.display().to_string(),
            fake_bin_dir: self.fake_bin_dir.display().to_string(),
        };

        if let Err(e) = fs::write(STATE_FILE, serde_json::to_string(&state).unwrap()) {
            eprintln!("[Wechat] 保存状态文件失败: {e}");
        } else {
            println!("[Wechat] 已保存劫持环境状态 → {STATE_FILE}");
        }
    }

    // ── 微信管理 ──

    /// 以劫持环境启动微信（仅在非恢复模式下启动）
    pub fn launch_wechat(&mut self) -> Result<()> {
        if self.recovered {
            println!("[Wechat] 使用从崩溃中恢复的微信进程 (PID {:?})，无需重启", self.wechat_pid);
            return Ok(());
        }

        let child = launch_wechat(&self.wechat_bin, &self.fake_bin_dir)?;
        self.wechat_pid = Some(child.id());
        self.wechat_proc = Some(child);
        thread::sleep(Duration::from_secs(3));
        self.save_state();
        Ok(())
    }

    /// 检查微信进程是否仍在运行
    pub fn is_wechat_alive(&mut self) -> bool {
        // 优先通过 PID 检查
        if let Some(pid) = self.wechat_pid {
            if !pid_is_alive(pid) {
                self.wechat_proc = None;
                return false;
            }
            return true;
        }
        // 回退到 Child 对象检查
        self.wechat_proc
            .as_mut()
            .map(|c| c.try_wait().ok().flatten().is_none())
            .unwrap_or(false)
    }

    // ── QR 扫码 ──

    /// 执行 QR 扫码核心流程：
    ///   1. 点击 P1（生成二维码按钮）
    ///   2. 等待 → 点击 P2（二维码消息 → 触发 xdg-open）
    ///   3. 等待 FIFO 收到 URL → 下载 → zbarimg 解码 → 返回 QR 数据
    pub fn qr_action(
        &mut self,
        mouse: &mut MouseController,
        p1: [u32; 2],
        p2: [u32; 2],
        timeout_secs: u64,
    ) -> Result<String> {
        if !self.is_wechat_alive() {
            println!("[Wechat] 微信进程已退出，正在重新启动...");
            self.recovered = false;
            self.launch_wechat()?;
        }

        self.drain_queue();

        println!("[Wechat] 点击 P1 ({p1:?}) 生成二维码");
        mouse.move_click(p1[0] as i32, p1[1] as i32, 100)?;
        thread::sleep(Duration::from_secs(2));

        let url = self.click_p2_and_wait(mouse, p2, timeout_secs)?;
        fetch_and_decode(&url)
    }

    fn click_p2_and_wait(
        &self,
        mouse: &mut MouseController,
        p2: [u32; 2],
        timeout_secs: u64,
    ) -> Result<String> {
        let rx = self.url_rx.lock().unwrap();

        for attempt in 0..2 {
            let label = if attempt > 0 {
                format!(" (第{}次)", attempt + 1)
            } else {
                String::new()
            };
            println!("[Wechat] 点击 P2 ({p2:?}){label}");

            mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;
            mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;

            let wait = if attempt == 0 {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(timeout_secs)
            };

            match rx.recv_timeout(wait) {
                Ok(url) => return Ok(url),
                Err(mpsc::RecvTimeoutError::Timeout) if attempt == 0 => {
                    println!("[Wechat] 未获取到链接，重试点击 P2");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("FIFO 监听线程意外断开");
                }
            }
        }

        anyhow::bail!("等待微信链接超时 ({}s)", timeout_secs)
    }

    fn drain_queue(&self) {
        let rx = self.url_rx.lock().unwrap();
        while rx.try_recv().is_ok() {}
    }

    // ── 清理 ──

    /// 清理劫持环境
    ///
    /// - `keep = true`：保留微信进程、临时目录和状态文件，下次启动自动恢复
    /// - `keep = false`：终止微信、删除临时目录和状态文件
    pub fn cleanup(&mut self, keep: bool) {
        if keep {
            self.save_state();
            println!("[Wechat] 劫持环境已保留，下次启动将自动恢复");
            return;
        }

        // ── 完整清理 ──
        let _ = fs::remove_file(STATE_FILE);

        self.stop_flag.store(true, Ordering::Relaxed);

        if let Some(mut proc) = self.wechat_proc.take() {
            println!("[Wechat] 正在终止微信进程...");
            let _ = proc.kill();
            let _ = proc.wait();
        } else if let Some(pid) = self.wechat_pid {
            println!("[Wechat] 正在终止微信进程 (PID {pid})...");
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            // 等待进程退出
            for _ in 0..30 {
                if !pid_is_alive(pid) {
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
            if pid_is_alive(pid) {
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        }

        if self.temp_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&self.temp_dir) {
                eprintln!("[Wechat] 清理临时目录失败: {e}");
            } else {
                println!("[Wechat] 已清理临时目录");
            }
        }
    }
}
