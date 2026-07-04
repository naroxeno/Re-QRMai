//! 基于 OpenCV 的 P1/P2 模板匹配位置自动识别

use anyhow::{Context, Result};
use log::{error, info};
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

// ── 跨平台屏幕截图 ────────────────────────────────────────
//
// 各平台使用原生 crate 截图，统一转为灰度 OpenCV Mat 返回

/// RGBA 像素 → PNG 字节 → OpenCV 灰度 Mat（各平台共用）
fn rgba_to_gray_mat(rgba: &[u8], width: u32, height: u32) -> Result<Mat> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .context("截图像素数据尺寸不匹配")?;
    let gray = image::DynamicImage::ImageRgba8(img).into_luma8();

    let mut png_buf = Vec::new();
    gray.write_to(
        &mut std::io::Cursor::new(&mut png_buf),
        image::ImageFormat::Png,
    )?;

    let buf = Vector::<u8>::from_slice(&png_buf);
    let mat = imgcodecs::imdecode(&buf, imgcodecs::IMREAD_GRAYSCALE)
        .context("无法解码截图为 OpenCV Mat")?;

    info!("[Detect] 截图成功: {}x{}", mat.cols(), mat.rows());
    Ok(mat)
}

// ── Linux: grim-rs ──────────────────────────────────────

#[cfg(target_os = "linux")]
pub fn capture_screen() -> Result<Mat> {
    use grim_rs::Grim;

    let mut grim = Grim::new().context("初始化 grim-rs 失败")?;
    let result = grim.capture_all().context("截图失败：请检查显示服务是否运行")?;

    let (w, h) = (result.width(), result.height());
    rgba_to_gray_mat(result.data(), w, h)
}

// ── Windows: windows-capture ────────────────────────────

#[cfg(target_os = "windows")]
pub fn capture_screen() -> Result<Mat> {
    use std::sync::{Arc, Mutex};

    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::graphics_capture_picker::GraphicsCapturePicker;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    // 共享状态与捕获标志（通过 Settings Flags 传入 handler）
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

    // 弹出选择器让用户选择捕获目标（屏幕或窗口）
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

    // start() 阻塞直到 on_frame_arrived 调用 control.stop()
    OneShot::start(settings).map_err(|e| anyhow::anyhow!("截图失败: {e}"))?;

    let (rgba, w, h) = png_data
        .lock()
        .unwrap()
        .take()
        .context("未获取到截图数据")?;

    rgba_to_gray_mat(&rgba, w, h)
}

// ── macOS: screencapturekit ─────────────────────────────

#[cfg(target_os = "macos")]
pub fn capture_screen() -> Result<Mat> {
    use screencapturekit::screenshot_manager::SCScreenshotManager;
    use screencapturekit::shareable_content::SCShareableContent;
    use screencapturekit::stream::configuration::SCStreamConfiguration;
    use screencapturekit::stream::content_filter::SCContentFilter;

    // 获取可共享内容（显示器列表）
    let content = SCShareableContent::get().context("无法获取显示器列表")?;
    let displays = content.displays();
    if displays.is_empty() {
        anyhow::bail!("未找到可捕获的显示器");
    }

    let display = &displays[0];
    let width = display.width();
    let height = display.height();

    // 创建内容过滤器（捕获整个显示器，不排除任何窗口）
    let filter = SCContentFilter::new(display).context("无法创建内容过滤器")?;

    // 配置流参数
    let config = SCStreamConfiguration::new();
    config.set_width(width);
    config.set_height(height);
    config.set_pixel_format(screencapturekit::stream::configuration::PixelFormat::BGRA8888);

    // 截图
    let img = SCScreenshotManager::capture_image(&filter, &config)
        .context("截屏失败")?;
    let bgra = img.bgra_data().context("无法读取截图像素")?;

    // BGRA → 灰度 Mat（blue/red 通道交换不影响灰度转换结果）
    rgba_to_gray_mat(bgra, width, height)
}
