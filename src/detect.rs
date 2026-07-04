//! 基于 template-matching crate 的 P1/P2 模板匹配位置自动识别
//!
//! 使用 GPU 加速的 SSD 模板匹配替代 OpenCV，更轻量且跨平台。

use anyhow::{Context, Result};
use image::ImageBuffer;
use log::{error, info};
use std::path::Path;
use template_matching::{find_extremes, match_template, Image as TmImage, MatchTemplateMethod};

/// 模板匹配结果
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

/// 多尺度模板匹配
///
/// - `pick_mode`: `"best"` 选最高置信度，`"bottom"` 选 Y 坐标最大（P2 用）
/// - `threshold`: 归一化置信度阈值（0–1）
pub fn match_template_multiscale(
    screen: &ImageBuffer<image::Luma<f32>, Vec<f32>>,
    template_path: &Path,
    threshold: f32,
    scales: &[f32],
    pick_mode: &str,
) -> Result<Option<MatchResult>> {
    let template = load_template(template_path)?;
    let (tw_orig, th_orig) = (template.width(), template.height());
    let template_area = (tw_orig * th_orig) as f32;

    let mut best: Option<(u32, u32, f32)> = None; // (cx, cy, confidence)

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

        // GPU 模板匹配 (SSD) — 手动构造 Image 以绕过 image 0.24/0.25 版本差异
        let result = match_template(
            TmImage::new(screen.as_raw(), screen.width(), screen.height()),
            TmImage::new(scaled.as_raw(), scaled.width(), scaled.height()),
            MatchTemplateMethod::SumOfSquaredDifferences,
        );
        let extremes = find_extremes(&result);

        // SSD: min_value 越小越匹配，归一化为 [0, 1] 置信度
        let confidence =
            1.0 - (extremes.min_value / template_area).clamp(0.0, 1.0);

        if confidence >= threshold {
            let cx = extremes.min_value_location.0 + tw / 2;
            let cy = extremes.min_value_location.1 + th / 2;
            let better = match &best {
                Some((_, _, conf)) => {
                    if pick_mode == "bottom" {
                        cy > *conf as u32
                    } else {
                        confidence > *conf
                    }
                }
                None => true,
            };
            if better {
                best = Some((cx, cy, confidence));
            }
        }
    }

    Ok(best.map(|(x, y, c)| MatchResult {
        x,
        y,
        confidence: c,
    }))
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

/// 从屏幕截图中识别 P1 / P2 坐标
pub fn detect_p1p2(
    screen: &ImageBuffer<image::Luma<f32>, Vec<f32>>,
    img_dir: &Path,
    threshold: f32,
) -> Result<(Option<[u32; 2]>, Option<[u32; 2]>)> {
    let scales = [0.6, 0.8, 1.0, 1.2, 1.5];

    let p1 = get_template_path(img_dir, "p1")
        .and_then(|path| {
            match_template_multiscale(screen, &path, threshold, &scales, "best")
                .unwrap_or(None)
        })
        .map(|m| [m.x, m.y]);

    let p2 = get_template_path(img_dir, "p2")
        .and_then(|path| {
            match_template_multiscale(screen, &path, threshold, &scales, "bottom")
                .unwrap_or(None)
        })
        .map(|m| [m.x, m.y]);

    Ok((p1, p2))
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
