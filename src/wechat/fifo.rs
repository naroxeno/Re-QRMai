//! FIFO 管道与伪装的 xdg-open（拦截微信打开的 MAID 链接）

use anyhow::{Context, Result};
use log::info;
use std::fs;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

/// 生成伪装的 `xdg-open`：MAID 链接写入 FIFO，其余转发系统 xdg-open
pub fn create_fake_xdg_open(fake_bin_dir: &Path, fifo_path: &Path) -> Result<()> {
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

    #[allow(unused_mut)]
    let mut perms = fs::metadata(&xdg_open)
        .with_context(|| format!("读取权限失败: {xdg_open:?}"))?
        .permissions();
    #[cfg(unix)]
    {
        perms.set_mode(0o755);
    }
    fs::set_permissions(&xdg_open, perms)
        .with_context(|| format!("设置可执行权限失败: {xdg_open:?}"))?;

    info!("[Wechat] 已创建伪装的 xdg-open: {xdg_open:?}");
    Ok(())
}

/// 后台线程持续读取 FIFO，把截获的 URL 通过 mpsc 通道送出
pub fn spawn_fifo_listener(
    fifo_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        info!("[Wechat] FIFO 监听线程已启动");
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
                        info!("[Wechat] 截获链接: {url}");
                        let _ = tx.send(url);
                    }
                }
            }
            thread::sleep(Duration::from_millis(200));
        }
        info!("[Wechat] FIFO 监听线程已退出");
    });

    rx
}
