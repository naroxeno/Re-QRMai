//! 微信进程管理：启动 / 探活 / PID 工具

use anyhow::{Context, Result};
use log::info;
use std::path::Path;
use std::process::{Child, Command, Stdio};

#[cfg(unix)]
/// 检查 PID 对应的进程是否存活（信号 0 探活）
pub fn pid_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
#[cfg(not(unix))]
pub fn pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
/// 该 PID 的 cmdline 是否像微信进程。
///
/// 安全审计 S-09/E2：恢复状态文件时会按里面的 PID 发信号、删目录，
/// 必须确认目标进程真的是微信，否则被篡改的状态文件可以借程序之手
/// 杀掉本用户的任意进程。
pub fn pid_looks_like_wechat(pid: u32) -> bool {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(bytes) if !bytes.is_empty() => {
            let cmdline = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            cmdline.contains("wechat")
                || cmdline.contains("weixin")
                // 本程序是自己用 dbus-run-session 拉起微信的，命令行里会有它
                || cmdline.contains("dbus-run-session")
        }
        _ => false,
    }
}
#[cfg(not(target_os = "linux"))]
pub fn pid_looks_like_wechat(_pid: u32) -> bool {
    false
}

/// 以劫持环境启动微信（PATH 前置伪装目录，使微信调用到伪装的 xdg-open）
pub fn launch_wechat(wechat_bin: &str, fake_bin_dir: &Path) -> Result<Child> {
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    info!("[Wechat] 启动微信: dbus-run-session {wechat_bin}");

    Command::new("dbus-run-session")
        .arg(wechat_bin)
        .env("PATH", &path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("启动微信失败: {wechat_bin}"))
}
