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

const STATE_FILE: &str = "/tmp/qrmai_state.json";

#[derive(Serialize, Deserialize)]
struct HijackState {
    wechat_pid: u32,
    temp_dir: String,
    fifo_path: String,
    fake_bin_dir: String,
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
    fn try_recover() -> Option<Self> {
        let state_path = Path::new(STATE_FILE);
        if !state_path.exists() {
            return None;
        }

        let json = fs::read_to_string(state_path).ok()?;
        let state: HijackState = serde_json::from_str(&json).ok()?;

        // 检查微信进程是否仍在运行
        if !process::pid_is_alive(state.wechat_pid) {
            info!("[Wechat] 上次的微信进程 (PID {}) 已退出，将创建新环境", state.wechat_pid);
            let _ = fs::remove_file(state_path);
            return None;
        }

        // 检查关键文件是否完整
        let temp_dir = PathBuf::from(&state.temp_dir);
        let fifo_path = PathBuf::from(&state.fifo_path);
        let fake_bin_dir = PathBuf::from(&state.fake_bin_dir);
        let xdg_open = fake_bin_dir.join("xdg-open");

        if !temp_dir.exists() || !fifo_path.exists() || !xdg_open.exists() {
            info!("[Wechat] 上次的劫持环境文件不完整，将重建");
            let _ = fs::remove_file(state_path);
            let _ = fs::remove_dir_all(&temp_dir);
            return None;
        }

        // 恢复成功：复用已有环境
        let stop_flag = Arc::new(AtomicBool::new(false));
        let url_rx = Mutex::new(fifo::spawn_fifo_listener(fifo_path.clone(), stop_flag.clone()));

        info!("[Wechat] ♻ 已恢复劫持环境:");
        info!("         微信 PID: {}", state.wechat_pid);
        info!("         临时目录: {temp_dir:?}");
        info!("         FIFO:     {fifo_path:?}");

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

        if let Err(e) = fs::write(STATE_FILE, serde_json::to_string(&state).unwrap()) {
            error!("[Wechat] 保存状态文件失败: {e}");
        } else {
            info!("[Wechat] 已保存劫持环境状态 → {STATE_FILE}");
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

        info!("[Wechat] 点击 P1 ({p1:?}) 生成二维码");
        mouse.move_click(p1[0] as i32, p1[1] as i32, 100)?;
        thread::sleep(Duration::from_secs(2));

        info!("[Wechat] 点击 P2 ({p2:?})");
        mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;
        mouse.move_click(p2[0] as i32, p2[1] as i32, 0)?;

        Ok(())
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
            info!("[Wechat] 劫持环境已保留，下次启动将自动恢复");
            return;
        }

        // ── 完整清理 ──
        let _ = fs::remove_file(STATE_FILE);

        self.stop_flag.store(true, Ordering::Relaxed);

        if let Some(mut proc) = self.wechat_proc.take() {
            info!("[Wechat] 正在终止微信进程...");
            let _ = proc.kill();
            let _ = proc.wait();
        } else if let Some(pid) = self.wechat_pid {
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

        if self.temp_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&self.temp_dir) {
                error!("[Wechat] 清理临时目录失败: {e}");
            } else {
                info!("[Wechat] 已清理临时目录");
            }
        }
    }
}
