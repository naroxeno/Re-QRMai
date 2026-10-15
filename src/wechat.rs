//! Linux WeChat 劫持模块
//!
//! 通过伪装 `xdg-open` + FIFO 管道拦截微信打开的 MAID 链接，实现二维码自动获取。
//!
//! 支持崩溃恢复：程序退出时可选择保留劫持环境，下次启动自动恢复，
//! 无需重新创建 FIFO / 伪装脚本 / 重启微信。
//!
//! 子模块划分（Rust 2018 模块布局）：
//! - `fifo`：伪装 xdg-open 与 FIFO 监听线程
//! - `process`：微信进程启动与探活
//! - `qr_fetch`：URL 抓取与二维码解码（纯函数已单测）

mod fifo;
mod process;
mod qr_fetch;

use crate::mouse::MouseController;
use anyhow::{Context, Result};
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

// 对外 re-export（main.rs 使用，保持原 wechat:: 路径不变）
pub use qr_fetch::fetch_and_decode;

// ── 状态持久化 ──────────────────────────────────────────

/// 历史遗留的固定状态文件路径。
///
/// 安全审计 S-09/E1：`/tmp` 下的固定文件名可被其他本机用户预先做成符号链接，
/// 诱导程序以其身份覆盖任意可写文件。现在状态文件只写在私有目录里，
/// 这个常量仅用于**一次性迁移**旧状态（读取时同样做普通文件 + 属主校验）。
const LEGACY_STATE_FILE: &str = "/tmp/qrmai_state.json";

#[derive(Serialize, Deserialize)]
struct HijackState {
    wechat_pid: u32,
    temp_dir: String,
    fifo_path: String,
    fake_bin_dir: String,
}

/// 创建（或复用）只属于当前用户的目录：权限 0700 + 属主校验 + 拒绝符号链接
fn ensure_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(dir)
            .with_context(|| format!("创建私有目录失败: {dir:?}"))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir).with_context(|| format!("创建私有目录失败: {dir:?}"))?;

    // symlink_metadata 不跟随符号链接：指向别处的目录会被这里挡下
    let md = fs::symlink_metadata(dir).with_context(|| format!("读取目录信息失败: {dir:?}"))?;
    if !md.is_dir() {
        anyhow::bail!("{dir:?} 不是目录，拒绝使用");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let me = unsafe { libc::geteuid() };
        if md.uid() != me {
            anyhow::bail!("目录 {dir:?} 属主不是当前用户，拒绝使用");
        }
        if md.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("收紧目录权限失败: {dir:?}"))?;
        }
    }
    Ok(())
}

/// 私有运行根目录：优先 `$XDG_RUNTIME_DIR/qrmai`，其次 `$HOME/.cache/qrmai`，
/// 最后回退 `/tmp/qrmai-<uid>`（依然是 0700 + 属主校验）。
fn private_root() -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var("XDG_RUNTIME_DIR").ok().filter(|s| s.starts_with('/')) {
        candidates.push(Path::new(&dir).join("qrmai"));
    }
    if let Some(home) = std::env::var("HOME").ok().filter(|s| s.starts_with('/')) {
        candidates.push(Path::new(&home).join(".cache/qrmai"));
    }
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0;
    candidates.push(std::env::temp_dir().join(format!("qrmai-{uid}")));

    let mut last_err: Option<anyhow::Error> = None;
    for dir in candidates {
        match ensure_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => {
                info!("[Wechat] 私有目录 {dir:?} 不可用: {e}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("找不到可用的私有目录")))
}

/// 状态文件路径（私有目录下的 state.json）
fn state_file_path() -> Result<PathBuf> {
    Ok(private_root()?.join("state.json"))
}

/// 安全读取状态文件：必须是当前用户的普通文件（拒绝符号链接）
fn read_state_file(path: &Path) -> Option<HijackState> {
    let md = fs::symlink_metadata(path).ok()?;
    if !md.is_file() {
        info!("[Wechat] 状态文件 {path:?} 不是普通文件，忽略");
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if md.uid() != unsafe { libc::geteuid() } {
            info!("[Wechat] 状态文件 {path:?} 属主不是当前用户，忽略");
            return None;
        }
    }
    let json = fs::read_to_string(path).ok()?;
    match serde_json::from_str(&json) {
        Ok(state) => Some(state),
        Err(e) => {
            info!("[Wechat] 状态文件 {path:?} 解析失败: {e}");
            None
        }
    }
}

/// 写状态文件：O_NOFOLLOW（不跟随符号链接）+ 0600（安全审计 E1）
fn write_state_file(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("打开状态文件失败: {path:?}"))?
    };
    #[cfg(not(unix))]
    let mut file = fs::File::create(path).with_context(|| format!("创建状态文件失败: {path:?}"))?;

    file.write_all(contents.as_bytes())
        .with_context(|| format!("写入状态文件失败: {path:?}"))
}

/// 为本进程创建一次性工作目录（私有根下 + 随机名 + 0700）
///
/// 安全审计 S-10/E3：原来用可预测的 `/tmp/qrmai_<pid>`（默认 0755），
/// 其他本机用户可以抢先建好目录并替换里面的伪装 `xdg-open`，而该目录会被
/// 前置进 PATH 用来拉起微信 → 以本用户身份执行任意程序。
fn create_run_dir() -> Result<PathBuf> {
    let root = private_root()?;
    let dir = root.join(format!(
        "run-{}-{}",
        std::process::id(),
        crate::util::random_suffix()
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("创建临时目录失败: {dir:?}"))?;
    }
    #[cfg(not(unix))]
    fs::create_dir(&dir).with_context(|| format!("创建临时目录失败: {dir:?}"))?;

    Ok(dir)
}

/// 只在「确实位于私有根目录之下」时才递归删除
/// （安全审计 E2：被篡改的状态文件可指定任意 temp_dir，进而递归删掉整个目录）
fn remove_run_dir(dir: &Path) {
    if !dir.exists() {
        return;
    }
    let root = private_root().ok().and_then(|r| r.canonicalize().ok());
    let target = dir.canonicalize().ok();
    let allowed = match (target, root) {
        (Some(t), Some(r)) => t != r && t.starts_with(&r),
        _ => false,
    };
    if !allowed {
        error!("[Wechat] 拒绝清理不在私有目录下的路径 {dir:?}");
        return;
    }
    match fs::remove_dir_all(dir) {
        Ok(()) => info!("[Wechat] 已清理临时目录 {dir:?}"),
        Err(e) => error!("[Wechat] 清理临时目录失败: {e}"),
    }
}

// ── WechatHijack ────────────────────────────────────────

/// Linux 微信劫持环境管理器
pub struct WechatHijack {
    temp_dir: PathBuf,
    fake_bin_dir: PathBuf,
    fifo_path: PathBuf,
    wechat_proc: Option<std::process::Child>,
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
            info!("[Wechat] ♻ 已从上次会话恢复劫持环境");
            return Ok(hijack);
        }
        Self::create_fresh(wechat_bin)
    }

    /// 全新创建劫持环境
    pub fn create_fresh(wechat_bin: &str) -> Result<Self> {
        // 私有目录 + 随机名 + 0700（安全审计 S-10/E3）
        let temp_dir = create_run_dir()?;

        let fake_bin_dir = temp_dir.join(".local_bin");
        let fifo_path = temp_dir.join(".link_pipe");

        // 创建 FIFO，权限 0600：其他本机用户读不到截获的登录链接（S-14）
        let status = Command::new("mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&fifo_path)
            .status()
            .context("mkfifo 命令失败，请确认系统支持命名管道")?;
        if !status.success() {
            anyhow::bail!("mkfifo 返回非零退出码");
        }
        info!("[Wechat] 已创建 FIFO: {fifo_path:?}");

        fifo::create_fake_xdg_open(&fake_bin_dir, &fifo_path)?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let url_rx = Mutex::new(fifo::spawn_fifo_listener(fifo_path.clone(), stop_flag.clone()));

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
    ///
    /// 安全审计 E2：状态文件内容不再被无条件信任——PID 必须真的是微信进程，
    /// 目录必须是私有目录下的普通目录，清理也只允许动私有目录内的路径。
    fn try_recover() -> Option<Self> {
        // 新路径没有状态文件时，允许一次性读取历史遗留的 /tmp 固定路径，
        // 这样升级后第一次重启不会把正在运行的微信劫持环境丢掉。
        let (state_path, legacy) = match state_file_path() {
            Ok(p) if p.exists() => (p, false),
            _ => {
                let legacy = PathBuf::from(LEGACY_STATE_FILE);
                if !legacy.exists() {
                    return None;
                }
                (legacy, true)
            }
        };

        let state = read_state_file(&state_path)?;

        // 检查微信进程是否仍在运行，且确实是微信
        if !process::pid_is_alive(state.wechat_pid) {
            info!("[Wechat] 上次的微信进程 (PID {}) 已退出，将创建新环境", state.wechat_pid);
            let _ = fs::remove_file(&state_path);
            return None;
        }
        if !process::pid_looks_like_wechat(state.wechat_pid) {
            error!(
                "[Wechat] 状态文件里的 PID {} 不像微信进程，拒绝恢复",
                state.wechat_pid
            );
            let _ = fs::remove_file(&state_path);
            return None;
        }

        // 检查关键文件是否完整
        let temp_dir = PathBuf::from(&state.temp_dir);
        let fifo_path = PathBuf::from(&state.fifo_path);
        let fake_bin_dir = PathBuf::from(&state.fake_bin_dir);
        let xdg_open = fake_bin_dir.join("xdg-open");

        if !temp_dir.is_dir() || !fifo_path.exists() || !xdg_open.is_file() {
            info!("[Wechat] 上次的劫持环境文件不完整，将重建");
            let _ = fs::remove_file(&state_path);
            remove_run_dir(&temp_dir);
            return None;
        }

        // 恢复成功：复用已有环境
        let stop_flag = Arc::new(AtomicBool::new(false));
        let url_rx = Mutex::new(fifo::spawn_fifo_listener(fifo_path.clone(), stop_flag.clone()));

        info!("[Wechat] ♻ 已恢复劫持环境:");
        info!("         微信 PID: {}", state.wechat_pid);
        info!("         临时目录: {temp_dir:?}");
        info!("         FIFO:     {fifo_path:?}");

        let hijack = Self {
            temp_dir,
            fake_bin_dir,
            fifo_path,
            wechat_proc: None,
            wechat_pid: Some(state.wechat_pid),
            stop_flag,
            url_rx,
            wechat_bin: String::new(), // 恢复时不需要 wechat_bin
            recovered: true,
        };

        // 从旧路径恢复的：顺手把状态写进私有目录，下次就走新路径
        if legacy {
            info!("[Wechat] 已把历史遗留状态文件迁移到私有目录");
            hijack.save_state();
        }

        Some(hijack)
    }

    /// 保存当前劫持环境状态到文件（供崩溃后恢复）
    fn save_state(&self) {
        let pid = match self.wechat_pid {
            Some(p) => p,
            None => match &self.wechat_proc {
                Some(proc) => proc.id(),
                None => {
                    error!("[Wechat] 没有微信 PID 可保存");
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

        let json = match serde_json::to_string(&state) {
            Ok(j) => j,
            Err(e) => {
                error!("[Wechat] 序列化状态失败: {e}");
                return;
            }
        };
        let path = match state_file_path() {
            Ok(p) => p,
            Err(e) => {
                error!("[Wechat] 无法确定状态文件位置: {e}");
                return;
            }
        };
        if let Err(e) = write_state_file(&path, &json) {
            error!("[Wechat] 保存状态文件失败: {e}");
        } else {
            info!("[Wechat] 已保存劫持环境状态 → {path:?}");
        }
    }

    // ── 微信管理 ──

    /// 以劫持环境启动微信（仅在非恢复模式下启动）
    pub fn launch_wechat(&mut self) -> Result<()> {
        if self.recovered {
            info!("[Wechat] 使用从崩溃中恢复的微信进程 (PID {:?})，无需重启", self.wechat_pid);
            return Ok(());
        }

        let child = process::launch_wechat(&self.wechat_bin, &self.fake_bin_dir)?;
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
            if !process::pid_is_alive(pid) {
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

    /// 纯点击流程（扩展模式 / 非 Linux 平台共用）：
    /// 点击 P1（生成二维码按钮）→ 等待 → 点击 P2（触发打开链接，双击防漏）→ 返回
    pub fn click_p1_p2(
        mouse: &mut MouseController,
        p1: [u32; 2],
        p2: [u32; 2],
    ) -> Result<()> {
        info!("[Wechat] 点击 P1 ({p1:?}) 生成二维码");
        mouse.move_click(p1[0] as i32, p1[1] as i32, 100)?;
        thread::sleep(Duration::from_secs(2));

        info!("[Wechat] 点击 P2 ({p2:?})");
        mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;
        mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;

        Ok(())
    }

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
            info!("[Wechat] 微信进程已退出，正在重新启动...");
            self.recovered = false;
            self.launch_wechat()?;
        }

        self.drain_queue();

        info!("[Wechat] 点击 P1 ({p1:?}) 生成二维码");
        mouse.move_click(p1[0] as i32, p1[1] as i32, 100)?;
        thread::sleep(Duration::from_secs(2));

        let url = self.click_p2_and_wait(mouse, p2, timeout_secs)?;
        fetch_and_decode(&url)
    }

    /// 仅执行 P1 → P2 点击（扩展模式），不等待 FIFO
    ///
    /// 返回后由外部轮询 QR 缓存获取解码结果
    pub fn click_p1p2(
        &mut self,
        mouse: &mut MouseController,
        p1: [u32; 2],
        p2: [u32; 2],
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            if !self.is_wechat_alive() {
                info!("[Wechat] 微信进程已退出，正在重新启动...");
                self.recovered = false;
                self.launch_wechat()?;
            }
            self.drain_queue();
        }

        Self::click_p1_p2(mouse, p1, p2)
    }

    fn click_p2_and_wait(
        &self,
        mouse: &mut MouseController,
        p2: [u32; 2],
        timeout_secs: u64,
    ) -> Result<String> {
        // 互斥锁中毒也不 panic（release 下 panic = "abort" 会拖死整个服务）
        let rx = self.url_rx.lock().unwrap_or_else(|e| e.into_inner());

        for attempt in 0..2 {
            let label = if attempt > 0 {
                format!(" (第{}次)", attempt + 1)
            } else {
                String::new()
            };
            info!("[Wechat] 点击 P2 ({p2:?}){label}");

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
                    info!("[Wechat] 未获取到链接，重试点击 P2");
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
        // 互斥锁中毒也不 panic（release 下 panic = "abort" 会拖死整个服务）
        let rx = self.url_rx.lock().unwrap_or_else(|e| e.into_inner());
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
            info!("[Wechat] 劫持环境已保留，下次启动将自动恢复");
            return;
        }

        // ── 完整清理 ──
        if let Ok(path) = state_file_path() {
            let _ = fs::remove_file(&path);
        }
        let _ = fs::remove_file(LEGACY_STATE_FILE);

        self.stop_flag.store(true, Ordering::Relaxed);

        if let Some(mut proc) = self.wechat_proc.take() {
            info!("[Wechat] 正在终止微信进程...");
            let _ = proc.kill();
            let _ = proc.wait();
        } else if let Some(pid) = self.wechat_pid {
            // 安全审计 S-09/E2：恢复来的 PID 必须先确认目标确实是微信，才敢发信号
            if !process::pid_looks_like_wechat(pid) {
                error!("[Wechat] PID {pid} 不像微信进程，拒绝发送信号");
            } else {
                info!("[Wechat] 正在终止微信进程 (PID {pid})...");
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                // 等待进程退出
                for _ in 0..30 {
                    if !process::pid_is_alive(pid) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(200));
                }
                if process::pid_is_alive(pid) {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
            }
        }

        remove_run_dir(&self.temp_dir);
    }
}
