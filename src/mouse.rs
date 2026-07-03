use anyhow::{Context, Result};
use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
use std::process::Command;
use std::time::Duration;

// ── 光标位置后端检测 ──────────────────────────────────────

/// 可用的光标位置读取后端
#[derive(Debug, Clone, Copy, PartialEq)]
enum PosBackend {
    /// Hyprland 合成器: `hyprctl cursorpos`
    Hyprctl,
    /// X11 / XWayland: `xdotool getmouselocation`
    Xdotool,
    /// 无可用后端
    None,
}

impl PosBackend {
    /// 自动检测当前环境可用的后端
    fn detect() -> Self {
        if which("hyprctl").is_some() {
            return Self::Hyprctl;
        }
        if std::env::var("DISPLAY").is_ok() && which("xdotool").is_some() {
            return Self::Xdotool;
        }
        Self::None
    }

    /// 读取当前光标位置，失败返回 None
    fn read(&self) -> Option<(i32, i32)> {
        match self {
            Self::Hyprctl => hyprctl_cursorpos(),
            Self::Xdotool => xdotool_getmouselocation(),
            Self::None => None,
        }
    }
}

/// 检查命令是否在 PATH 中
fn which(cmd: &str) -> Option<String> {
    Command::new("which")
        .arg(cmd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// hyprctl cursorpos → 解析 "x, y" 输出
fn hyprctl_cursorpos() -> Option<(i32, i32)> {
    let out = Command::new("hyprctl").arg("cursorpos").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let nums: Vec<i32> = text
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter_map(|s| s.parse().ok())
        .collect();
    (nums.len() >= 2).then(|| (nums[0], nums[1]))
}

/// xdotool getmouselocation --shell → 解析 "X=...\nY=..."
fn xdotool_getmouselocation() -> Option<(i32, i32)> {
    let out = Command::new("xdotool")
        .args(["getmouselocation", "--shell"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut x: Option<i32> = None;
    let mut y: Option<i32> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("X=") {
            x = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("Y=") {
            y = v.trim().parse().ok();
        }
    }
    match (x, y) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    }
}

// ── 鼠标控制器 ──────────────────────────────────────────

/// Linux 鼠标操控器，封装 enigo 实现移动 + 点击，
/// 同时自动检测光标位置读取后端
pub struct MouseController {
    enigo: Enigo,
    pos_backend: PosBackend,
}

impl MouseController {
    /// 创建新的鼠标控制器，自动检测可用后端
    pub fn new() -> Result<Self> {
        let enigo =
            Enigo::new(&Settings::default()).context("Failed to initialize enigo")?;
        let pos_backend = PosBackend::detect();
        Ok(Self {
            enigo,
            pos_backend,
        })
    }

    /// 获取当前光标位置（通过 hyprctl / xdotool 等外部工具）
    pub fn position(&self) -> Option<(i32, i32)> {
        self.pos_backend.read()
    }

    /// 将鼠标移动到屏幕绝对坐标 (x, y)
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        self.enigo
            .move_mouse(x, y, Coordinate::Abs)
            .context("move_to failed")?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    }

    /// 在当前鼠标位置执行左键点击（按下+释放）
    pub fn click(&mut self) -> Result<()> {
        self.enigo
            .button(Button::Left, Direction::Click)
            .context("click failed")?;
        Ok(())
    }

    /// 移动鼠标到 (x, y) 并点击
    ///
    /// `delay_ms` — 移动完成后等待的毫秒数，默认 100ms
    pub fn move_click(&mut self, x: i32, y: i32, delay_ms: u64) -> Result<()> {
        self.move_to(x, y)?;
        std::thread::sleep(Duration::from_millis(delay_ms));
        self.click()
    }
}
