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
