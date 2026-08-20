//! 基于 template-matching crate 的 P1/P2 模板匹配位置自动识别
//!
//! 使用 GPU 加速的 SSD 模板匹配替代 OpenCV，更轻量且跨平台。

use anyhow::{Context, Result};
use image::ImageBuffer;
use log::{error, info, warn};
use std::path::Path;
use template_matching::{match_template, Image as TmImage, MatchTemplateMethod};

/// 模板匹配结果（x/y 为匹配框中心点）
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub x: u32,
    pub y: u32,
    /// 归一化置信度，范围 [0, 1]，越高越好
    pub confidence: f32,
}

/// 加载模板为 f32 灰度图（像素值 0.0–1.0）
fn load_template(path: &Path) -> Result<ImageBuffer<image::Luma<f32>, Vec<f32>>> {
    let img = image::open(path)
        .with_context(|| format!("无法读取模板图: {path:?}"))?;
    Ok(img.to_luma32f())
}

/// 3×3 Sobel 梯度幅值图：纯色区域 → 0，边缘/纹理 → 大值。
///
/// 在梯度图上做 SSD 匹配可消除「大面积纯色背景」的干扰：
/// 模板与屏幕上任意纯色区域对齐时差的平方和几乎为 0，导致置信度虚高、
/// 位置错乱（本项目 P2 模板约 90% 是深色背景，正是此前误匹配的根源）。
fn to_gradient_f32(
    img: &ImageBuffer<image::Luma<f32>, Vec<f32>>,
) -> ImageBuffer<image::Luma<f32>, Vec<f32>> {
    let (w, h) = img.dimensions();
    let mut out = ImageBuffer::new(w, h);
    const GX: [f32; 9] = [-1.0, 0.0, 1.0, -2.0, 0.0, 2.0, -1.0, 0.0, 1.0];
    const GY: [f32; 9] = [-1.0, -2.0, -1.0, 0.0, 0.0, 0.0, 1.0, 2.0, 1.0];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let mut sx = 0.0f32;
            let mut sy = 0.0f32;
            let mut k = 0usize;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let px = (x as i32 + dx) as u32;
                    let py = (y as i32 + dy) as u32;
                    let p = img.get_pixel(px, py)[0];
                    sx += GX[k] * p;
                    sy += GY[k] * p;
                    k += 1;
                }
            }
            out.put_pixel(x, y, image::Luma([(sx * sx + sy * sy).sqrt()]));
        }
    }
    out
}

/// 在 SSD 结果图中找多个局部极小值（NMS），返回按 SSD 升序的 top-N 个 (x, y, value)。
///
/// 原 find_extremes 只取全局唯一最优，屏幕上存在多个相似区域时会漏掉次优候选，
/// 导致误匹配；这里收集多个候选供上层做语义/位置校验。
fn local_minima_nms(
    data: &[f32],
    width: u32,
    height: u32,
    radius: usize,
    max_count: usize,
) -> Vec<(u32, u32, f32)> {
    let w = width as usize;
    let h = height as usize;
    if w < 3 || h < 3 {
        return Vec::new();
    }
    let mut minima: Vec<(u32, u32, f32)> = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let v = data[y * w + x];
            let mut is_min = true;
            'nbr: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = (x as i32 + dx) as usize;
                    let ny = (y as i32 + dy) as usize;
                    if data[ny * w + nx] < v {
                        is_min = false;
                        break 'nbr;
                    }
                }
            }
            if is_min {
                minima.push((x as u32, y as u32, v));
            }
        }
    }
    // SSD 升序（越小越匹配）
    minima.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    // 邻域去重：跳过与已选候选距离 < radius 的点
    let mut picked: Vec<(u32, u32, f32)> = Vec::new();
    for m in minima {
        let too_close = picked.iter().any(|p| {
            (p.0 as i64 - m.0 as i64).abs() < radius as i64
                && (p.1 as i64 - m.1 as i64).abs() < radius as i64
        });
        if too_close {
            continue;
        }
        picked.push(m);
        if picked.len() >= max_count {
            break;
        }
    }
    picked
}

/// 多尺度模板匹配（在 Sobel 梯度图上进行）
///
/// 收集所有「置信度 ≥ 阈值」的候选（每个尺度 1 个全局最优），
/// 由调用方结合语义/位置约束做最终选择。
///
/// - `label`: 仅用于日志标注（如 "p1" / "p2"）
/// - `threshold`: 归一化置信度阈值（0–1）
pub fn match_template_multiscale(
    screen: &ImageBuffer<image::Luma<f32>, Vec<f32>>,
    template_path: &Path,
    threshold: f32,
    scales: &[f32],
    label: &str,
) -> Result<Vec<MatchResult>> {
    // 屏幕与模板统一转梯度图：只保留边缘/纹理，纯色背景不再干扰 SSD
    let screen = to_gradient_f32(screen);
    let template_img = load_template(template_path)?;
    let template = to_gradient_f32(&template_img);
    let (tw_orig, th_orig) = (template.width(), template.height());

    let mut cands: Vec<MatchResult> = Vec::new();

    for &scale in scales {
        let tw = (tw_orig as f32 * scale) as u32;
        let th = (th_orig as f32 * scale) as u32;
        if tw < 10 || th < 10 || tw > screen.width() || th > screen.height() {
            continue;
        }

        // 缩放模板
        let scaled = image::imageops::resize(
            &template,
            tw,
            th,
            image::imageops::FilterType::Lanczos3,
        );

        // 有效（有内容）像素数：只统计梯度幅值较大的像素，
        // 使置信度只反映「内容对齐程度」，纯色背景不再贡献分母
        let n_active = scaled.pixels().filter(|p| p[0] > 0.03).count().max(1) as f32;

        // GPU 模板匹配 (SSD) — 手动构造 Image 以绕过 image 0.24/0.25 版本差异
        let result = match_template(
            TmImage::new(screen.as_raw(), screen.width(), screen.height()),
            TmImage::new(scaled.as_raw(), scaled.width(), scaled.height()),
            MatchTemplateMethod::SumOfSquaredDifferences,
        );

        // NMS 收集多个局部极小值（每尺度 top-5），替代 find_extremes 的全局唯一最优
        let minima = local_minima_nms(result.data.as_ref(), result.width, result.height, 8, 5);
        let mut scale_hits = 0;
        for (x, y, min_value) in minima {
            // 置信度 = 1 - 平均内容差 / 有效像素数；内容对齐时接近 1
            let confidence = 1.0 - (min_value / n_active).clamp(0.0, 1.0);
            if confidence >= threshold {
                let cx = x + tw / 2;
                let cy = y + th / 2;
                info!(
                    "[Detect] {label} scale={scale:.2} 候选 ({cx}, {cy}) 置信度={confidence:.3}",
                );
                cands.push(MatchResult {
                    x: cx,
                    y: cy,
                    confidence,
                });
                scale_hits += 1;
            }
        }
        if scale_hits > 0 {
            info!("[Detect] {label} scale={scale:.2} 达标 {scale_hits} 个 (有效像素 {n_active:.0})");
        }
    }

    Ok(cands)
}

/// 加载模板路径，优先用户上传版本
pub fn get_template_path(img_dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let user_path = img_dir.join(format!("{name}_user.png"));
    let dev_path = img_dir.join(format!("{name}.png"));
    if user_path.is_file() {
        info!("[Detect] 使用用户模板: {user_path:?}");
        Some(user_path)
    } else if dev_path.is_file() {
        info!("[Detect] 使用开发者模板: {dev_path:?}");
        Some(dev_path)
    } else {
        error!("[Detect] 未找到模板图 {name}");
        None
    }
}

/// 置信度容差：与最佳置信度差距在此范围内的候选视为「并列」，
/// 并列时结合位置约束（离 P1 更近者优先，呼应「P2 不会太远」）
const CONF_TOLERANCE: f32 = 0.05;

/// 从候选中选置信度最高者（best 语义）
fn pick_best(cands: &[MatchResult]) -> Option<MatchResult> {
    cands
        .iter()
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
        .cloned()
}

/// 候选与 P1 的平方距离（仅用于排序）
fn dist2_to_p1(c: &MatchResult, p1: [u32; 2]) -> u64 {
    let dx = (c.x as i64 - p1[0] as i64).unsigned_abs();
    let dy = (c.y as i64 - p1[1] as i64).unsigned_abs();
    dx * dx + dy * dy
}

/// 在 P1 左上方区域内选 P2（仅区域，不 fallback）：
/// 区域内 best（最高置信度），与最佳差距 < 容差的并列者选离 P1 最近。
fn pick_p2_in_region(
    cands: &[MatchResult],
    p1: [u32; 2],
    max_dx: u32,
    max_dy: u32,
) -> Option<MatchResult> {
    let in_region: Vec<&MatchResult> = cands
        .iter()
        .filter(|c| {
            c.x < p1[0] && c.y < p1[1] && p1[0] - c.x <= max_dx && p1[1] - c.y <= max_dy
        })
        .collect();

    if in_region.is_empty() {
        return None;
    }
    let best_conf = in_region
        .iter()
        .map(|c| c.confidence)
        .fold(0.0f32, f32::max);
    let tied = in_region
        .iter()
        .filter(|c| best_conf - c.confidence < CONF_TOLERANCE)
        .count();
    let chosen = in_region
        .iter()
        .filter(|c| best_conf - c.confidence < CONF_TOLERANCE)
        .min_by_key(|c| dist2_to_p1(c, p1))
        .map(|c| (*c).clone());
    if let Some(c) = &chosen {
        info!(
            "[Detect] P2 区域候选 {} 个(并列 {tied}), 选中中心 ({}, {}) 置信度={:.3}",
            in_region.len(),
            c.x,
            c.y,
            c.confidence
        );
    }
    chosen
}

/// 语义化选择 P2（带兜底）：区域内选 P2；区域内无候选 → 退回全屏 best 并打警告。
fn pick_p2(
    cands: &[MatchResult],
    p1: [u32; 2],
    max_dx: u32,
    max_dy: u32,
) -> Option<MatchResult> {
    if let Some(c) = pick_p2_in_region(cands, p1, max_dx, max_dy) {
        return Some(c);
    }
    // 兜底：全屏 best + 警告
    if let Some(c) = pick_best(cands) {
        warn!(
            "[Detect] P2 未在 P1 左上方区域 ({max_dx}x{max_dy}) 内找到候选 (P1=({},{}))，退回全屏最佳 ({}, {}) 置信度={:.3}，请检查模板",
            p1[0], p1[1], c.x, c.y, c.confidence
        );
        return Some(c);
    }
    None
}

/// P1-P2 组合选择：
/// 利用「P2 在 P1 左上方且不太远」的语义——对 P1 候选按置信度降序，
/// 选第一个「其左上方区域存在 P2 候选」的 P1（及其 P2）。
/// 这能纠正误匹配：若 P1 上方存在置信度更高的相似按钮，其左上方没有二维码消息 P2，
/// 会被跳过，选到正确的 P1。
fn select_pair(
    p1_cands: &[MatchResult],
    p2_cands: &[MatchResult],
    max_dx: u32,
    max_dy: u32,
) -> (Option<MatchResult>, Option<MatchResult>) {
    let mut p1_sorted = p1_cands.to_vec();
    p1_sorted.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));

    for p1 in &p1_sorted {
        if let Some(p2) = pick_p2_in_region(p2_cands, [p1.x, p1.y], max_dx, max_dy) {
            info!(
                "[Detect] P1-P2 组合校验通过: P1=({}, {}) + P2=({}, {})",
                p1.x, p1.y, p2.x, p2.y
            );
            return (Some(p1.clone()), Some(p2));
        }
    }

    // 无组合：退回各自 best（P2 走带兜底的语义选择）
    let p1 = pick_best(p1_cands);
    let p2 = p1
        .as_ref()
        .and_then(|p1| pick_p2(p2_cands, [p1.x, p1.y], max_dx, max_dy))
        .or_else(|| pick_best(p2_cands));
    (p1, p2)
}

/// P1/P2 识别结果（None 表示未识别到）
pub type DetectResult = Result<(Option<[u32; 2]>, Option<[u32; 2]>)>;

/// 从屏幕截图中识别 P1 / P2 坐标
///
/// - P1：全屏 best（最高置信度）
/// - P2：语义化选择——在 P1 左上方且距离不太远的区域内取 best，
///   置信度并列时选离 P1 更近者；区域内无候选则退回全屏 best 并警告
pub fn detect_p1p2(
    screen: &ImageBuffer<image::Luma<f32>, Vec<f32>>,
    img_dir: &Path,
    threshold: f32,
    // P2 检测区宽（px），由调用方按屏幕分辨率计算或手动指定
    p2_max_dx: u32,
    // P2 检测区高（px）
    p2_max_dy: u32,
) -> DetectResult {
    let scales = [0.6, 0.8, 1.0, 1.2, 1.5];

    let p1_cands = get_template_path(img_dir, "p1")
        .map(|path| match_template_multiscale(screen, &path, threshold, &scales, "p1"))
        .transpose()?
        .unwrap_or_default();

    let p2_cands = get_template_path(img_dir, "p2")
        .map(|path| match_template_multiscale(screen, &path, threshold, &scales, "p2"))
        .transpose()?
        .unwrap_or_default();

    // P1-P2 组合校验：P2 在 P1 左上方 → 优先选「左上方有 P2 候选」的 P1，
    // 纠正 P1 误匹配（上方相似按钮置信度更高但左上方无二维码消息）
    let (p1m, p2m) = select_pair(&p1_cands, &p2_cands, p2_max_dx, p2_max_dy);

    Ok((p1m.map(|m| [m.x, m.y]), p2m.map(|m| [m.x, m.y])))
}

// ── 跨平台屏幕截图 ────────────────────────────────────────
//
// 各平台使用原生 crate 截图，统一转为 f32 灰度 ImageBuffer 返回

/// RGBA 像素 → f32 灰度图（像素值 0.0–1.0，各平台共用）
fn rgba_to_luma32f(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<ImageBuffer<image::Luma<f32>, Vec<f32>>> {
    let pixels: Vec<f32> = rgba
        .chunks_exact(4)
        .map(|p| {
            // 标准加权灰度: 0.299R + 0.587G + 0.114B，归一化到 0.0–1.0
            (0.299_f32 * p[0] as f32 + 0.587_f32 * p[1] as f32 + 0.114_f32 * p[2] as f32)
                / 255.0
        })
        .collect();

    let img = ImageBuffer::from_raw(width, height, pixels)
        .context("截图像素数据尺寸不匹配")?;

    info!("[Detect] 截图成功: {}x{}", img.width(), img.height());
    Ok(img)
}

// ── Linux: grim-rs ──────────────────────────────────────

#[cfg(target_os = "linux")]
pub fn capture_screen() -> Result<ImageBuffer<image::Luma<f32>, Vec<f32>>> {
    use grim_rs::Grim;

    let mut grim = Grim::new().context("初始化 grim-rs 失败")?;
    let result = grim
        .capture_all()
        .context("截图失败：请检查显示服务是否运行")?;

    let (w, h) = (result.width(), result.height());
    rgba_to_luma32f(result.data(), w, h)
}

// ── Windows: windows-capture ────────────────────────────

#[cfg(target_os = "windows")]
pub fn capture_screen() -> Result<ImageBuffer<image::Luma<f32>, Vec<f32>>> {
    use std::sync::{Arc, Mutex};

    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::graphics_capture_picker::GraphicsCapturePicker;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    struct CaptureFlags {
        size: (u32, u32),
        buffer: Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>,
    }

    struct OneShot {
        buffer: Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>,
        w: u32,
        h: u32,
    }

    impl GraphicsCaptureApiHandler for OneShot {
        type Flags = CaptureFlags;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self {
                buffer: ctx.flags.buffer,
                w: ctx.flags.size.0,
                h: ctx.flags.size.1,
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let mut raw = frame.buffer()?;
            let rgba: Vec<u8> = raw.as_raw_buffer().to_vec();
            *self.buffer.lock().unwrap() = Some((rgba, self.w, self.h));
            control.stop();
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    let png_data: Arc<Mutex<Option<(Vec<u8>, u32, u32)>>> = Arc::new(Mutex::new(None));

    let item = GraphicsCapturePicker::pick_item().context("无法打开捕获选择器")?;
    let Some(item) = item else {
        anyhow::bail!("未选择捕获目标");
    };
    let (width, height) = item.size().context("无法获取捕获目标尺寸")?;

    let flags = CaptureFlags {
        size: (width as u32, height as u32),
        buffer: png_data.clone(),
    };

    let settings = Settings::new(
        item,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        flags,
    );

    OneShot::start(settings).map_err(|e| anyhow::anyhow!("截图失败: {e}"))?;

    let (rgba, w, h) = png_data
        .lock()
        .unwrap()
        .take()
        .context("未获取到截图数据")?;

    rgba_to_luma32f(&rgba, w, h)
}

// ── macOS: screencapturekit ─────────────────────────────

#[cfg(target_os = "macos")]
pub fn capture_screen() -> Result<ImageBuffer<image::Luma<f32>, Vec<f32>>> {
    use screencapturekit::screenshot_manager::SCScreenshotManager;
    use screencapturekit::shareable_content::SCShareableContent;
    use screencapturekit::stream::configuration::SCStreamConfiguration;
    use screencapturekit::stream::content_filter::SCContentFilter;

    let content = SCShareableContent::get().context("无法获取显示器列表")?;
    let displays = content.displays();
    if displays.is_empty() {
        anyhow::bail!("未找到可捕获的显示器");
    }

    let display = &displays[0];
    let width = display.width();
    let height = display.height();

    let filter = SCContentFilter::new(display).context("无法创建内容过滤器")?;

    let config = SCStreamConfiguration::new();
    config.set_width(width);
    config.set_height(height);
    config.set_pixel_format(
        screencapturekit::stream::configuration::PixelFormat::BGRA8888,
    );

    let img =
        SCScreenshotManager::capture_image(&filter, &config).context("截屏失败")?;
    let bgra = img.bgra_data().context("无法读取截图像素")?;

    // BGRA → 灰度（blue/red 通道交换不影响灰度转换结果）
    rgba_to_luma32f(bgra, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    /// 构造 u8 灰度图案：边缘留边框（bg），内部填充 fg
    fn make_pattern(w: u32, h: u32, bg: u8, fg: u8) -> ImageBuffer<Luma<u8>, Vec<u8>> {
        ImageBuffer::from_fn(w, h, |x, y| {
            let v = if x >= 5 && x < w - 5 && y >= 5 && y < h - 5 {
                fg
            } else {
                bg
            };
            Luma([v])
        })
    }

    /// 合成图像：模板以已知位置粘贴进屏幕，多尺度匹配应找到正确中心
    #[test]
    fn match_finds_template_at_known_position() {
        let (tw, th) = (30u32, 20u32);
        let (sw, sh) = (200u32, 150u32);
        let tpl_u8 = make_pattern(tw, th, 128, 255);
        let tpl_path = std::env::temp_dir().join("qrmai_test_tpl.png");
        tpl_u8.save(&tpl_path).unwrap();

        // 屏幕：背景与模板灰底一致（避免外圈额外边缘干扰）+ 在 (100, 80) 粘贴模板
        let bg = 128.0f32 / 255.0; // 模板灰底 128 → 0.502
        let mut screen = ImageBuffer::from_pixel(sw, sh, Luma([bg]));
        for y in 0..th {
            for x in 0..tw {
                let v = tpl_u8.get_pixel(x, y)[0] as f32 / 255.0;
                screen.put_pixel(100 + x, 80 + y, Luma([v]));
            }
        }

        let cands = match_template_multiscale(&screen, &tpl_path, 0.5, &[1.0], "test")
            .expect("匹配失败");
        std::fs::remove_file(&tpl_path).ok();
        assert_eq!(cands.len(), 1, "应找到 1 个候选");
        let m = &cands[0];
        let (ex, ey) = (115i64, 90i64); // 期望中心 (100+15, 80+10)
        assert!(
            (m.x as i64 - ex).abs() <= 2 && (m.y as i64 - ey).abs() <= 2,
            "位置偏差过大: 实际 ({}, {}), 期望 ({ex}, {ey})",
            m.x,
            m.y
        );
        assert!(m.confidence > 0.9, "置信度应高: {}", m.confidence);
    }

    fn mr(x: u32, y: u32, confidence: f32) -> MatchResult {
        MatchResult { x, y, confidence }
    }

    /// P2 语义：区域内候选优先于区域外更高置信度
    #[test]
    fn pick_p2_prefers_region_over_global_best() {
        let p1 = [300, 300];
        let cands = vec![
            mr(280, 260, 0.9),  // P1 左上方区域内
            mr(500, 500, 0.95), // 区域外（右下）置信度更高
        ];
        let chosen = pick_p2(&cands, p1, 200, 200).unwrap();
        assert_eq!(
            (chosen.x, chosen.y),
            (280, 260),
            "应选区域内候选而非全局最高置信度"
        );
    }

    /// P2 语义：置信度并列（差距 < 容差）时选离 P1 更近者
    #[test]
    fn pick_p2_ties_prefer_closer_to_p1() {
        let p1 = [300, 300];
        let cands = vec![
            mr(280, 260, 0.91), // 较远（dist2=2000）
            mr(290, 290, 0.90), // 较近（dist2=200），置信度差距 0.01 < 0.05 → 并列
        ];
        let chosen = pick_p2(&cands, p1, 200, 200).unwrap();
        assert_eq!((chosen.x, chosen.y), (290, 290), "并列时应选离 P1 更近者");
    }

    /// P2 语义：区域内无候选 → 退回全屏最高置信度
    #[test]
    fn pick_p2_falls_back_to_global_best() {
        let p1 = [300, 300];
        let cands = vec![
            mr(500, 500, 0.9),
            mr(510, 510, 0.95),
        ];
        let chosen = pick_p2(&cands, p1, 100, 100).unwrap(); // 区域 100x100，无候选
        assert_eq!((chosen.x, chosen.y), (510, 510), "应退回全屏最高置信度");
    }

    /// best 语义：返回最高置信度
    #[test]
    fn pick_best_returns_highest_confidence() {
        let cands = vec![mr(10, 10, 0.6), mr(20, 20, 0.9), mr(30, 30, 0.8)];
        let best = pick_best(&cands).unwrap();
        assert_eq!((best.x, best.y), (20, 20));
    }

    /// P1-P2 组合校验：上方相似按钮置信度更高但左上方无 P2 →
    /// 应跳过，选「左上方有 P2 候选」的正确 P1
    #[test]
    fn select_pair_corrects_false_p1() {
        // 误匹配 P1（上方相似按钮，置信度更高），其左上方没有 P2
        let p1_false = mr(1890, 1200, 0.95);
        // 正确 P1，其左上方 (x<1892, y<1407) 有 P2
        let p1_true = mr(1892, 1407, 0.90);
        let p1_cands = vec![p1_false, p1_true];

        // P2 候选：正确 P1 左上方区域内的二维码消息
        let p2_true = mr(1380, 1254, 0.85);
        let p2_far = mr(500, 500, 0.88); // 区域外，置信度更高但不满足位置
        let p2_cands = vec![p2_true, p2_far];

        let (p1, p2) = select_pair(&p1_cands, &p2_cands, 800, 600);
        let p1 = p1.expect("应选中 P1");
        let p2 = p2.expect("应选中 P2");
        assert_eq!((p1.x, p1.y), (1892, 1407), "应纠正误匹配，选正确 P1");
        assert_eq!((p2.x, p2.y), (1380, 1254), "P2 应为正确 P1 左上方区域的候选");
    }

    /// P1-P2 组合校验：无任何组合 → 退回各自 best（P2 全屏兜底）
    #[test]
    fn select_pair_falls_back_without_combination() {
        let p1_cands = vec![mr(100, 100, 0.95)];
        let p2_cands = vec![mr(800, 800, 0.90), mr(820, 820, 0.88)];
        let (p1, p2) = select_pair(&p1_cands, &p2_cands, 200, 200);
        assert_eq!(p1.as_ref().map(|m| m.x), Some(100));
        assert_eq!(p2.as_ref().map(|m| m.x), Some(800), "无组合时退回全屏最佳");
    }
}
