//! 基于 OpenCV 的 P1/P2 模板匹配位置自动识别

use anyhow::{Context, Result};
use opencv::core::{self, Mat, Point, Size, Vector};
use opencv::imgcodecs;
use opencv::prelude::*;
use opencv::imgproc;
use std::path::Path;

/// 模板匹配结果
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub x: i32,
    pub y: i32,
    pub confidence: f32,
}

/// 多尺度模板匹配
///
/// `pick_mode`: `"best"` 选最高置信度，`"bottom"` 选 Y 坐标最大（P2 用）
pub fn match_template_multiscale(
    screen: &Mat,
    template_path: &Path,
    threshold: f32,
    scales: &[f32],
    pick_mode: &str,
) -> Result<Option<MatchResult>> {
    let template = imgcodecs::imread(
        template_path.to_str().unwrap(),
        imgcodecs::IMREAD_GRAYSCALE,
    )
    .with_context(|| format!("无法读取模板图: {template_path:?}"))?;

    let mut best: Option<(i32, i32, f64)> = None; // (cx, cy, confidence)

    for &scale in scales {
        let tw = (template.cols() as f32 * scale) as i32;
        let th = (template.rows() as f32 * scale) as i32;
        if tw < 10 || th < 10 || tw > screen.cols() || th > screen.rows() {
            continue;
        }

        // 缩放模板
        let mut scaled = Mat::default();
        imgproc::resize(
            &template,
            &mut scaled,
            Size::new(tw, th),
            0.0,
            0.0,
            imgproc::INTER_LANCZOS4,
        )?;

        // 模板匹配
        let mut result = Mat::default();
        imgproc::match_template(
            screen,
            &scaled,
            &mut result,
            imgproc::TM_CCOEFF_NORMED,
            &Mat::default(),
        )?;

        // 找最大匹配位置
        let mut max_val = 0.0;
        let mut max_loc = Point::default();
        core::min_max_loc(
            &result,
            None,
            Some(&mut max_val),
            None,
            Some(&mut max_loc),
            &Mat::default(),
        )?;

        if (max_val as f32) >= threshold {
            let cx = max_loc.x + tw / 2;
            let cy = max_loc.y + th / 2;
            let better = match &best {
                Some((_, _, conf)) => {
                    if pick_mode == "bottom" {
                        cy > *conf as i32
                    } else {
                        max_val > *conf
                    }
                }
                None => true,
            };
            if better {
                best = Some((cx, cy, max_val));
            }
        }
    }

    Ok(best.map(|(x, y, c)| MatchResult {
        x,
        y,
        confidence: c as f32,
    }))
}

/// 加载模板路径，优先用户上传版本
pub fn get_template_path(img_dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let user_path = img_dir.join(format!("{name}_user.png"));
    let dev_path = img_dir.join(format!("{name}.png"));
    if user_path.is_file() {
        println!("[Detect] 使用用户模板: {user_path:?}");
        Some(user_path)
    } else if dev_path.is_file() {
        println!("[Detect] 使用开发者模板: {dev_path:?}");
        Some(dev_path)
    } else {
        eprintln!("[Detect] 未找到模板图 {name}");
        None
    }
}

/// 从屏幕截图中识别 P1 / P2 坐标
pub fn detect_p1p2(
    screen: &Mat,
    img_dir: &Path,
    threshold: f32,
) -> Result<(Option<[u32; 2]>, Option<[u32; 2]>)> {
    let scales = [0.6, 0.8, 1.0, 1.2, 1.5];

    let p1 = get_template_path(img_dir, "p1")
        .and_then(|path| {
            match_template_multiscale(screen, &path, threshold, &scales, "best")
                .unwrap_or(None)
        })
        .map(|m| [m.x as u32, m.y as u32]);

    let p2 = get_template_path(img_dir, "p2")
        .and_then(|path| {
            match_template_multiscale(screen, &path, threshold, &scales, "bottom")
                .unwrap_or(None)
        })
        .map(|m| [m.x as u32, m.y as u32]);

    Ok((p1, p2))
}

/// Linux 屏幕截图（grim → PNG → OpenCV Mat）
pub fn capture_screen() -> Result<Mat> {
    use std::process::Command;

    let output = Command::new("grim")
        .arg("-")
        .output()
        .context("grim 截图失败（请安装 grim）")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("grim 截图失败: {stderr}");
    }

    // 从内存 PNG 解码为 OpenCV Mat
    let buf = Vector::<u8>::from_slice(&output.stdout);
    let img = imgcodecs::imdecode(&buf, imgcodecs::IMREAD_GRAYSCALE)
        .context("无法解码截图")?;

    println!("[Detect] 截图成功: {}x{}", img.cols(), img.rows());
    Ok(img)
}
