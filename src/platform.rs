use crate::model::RgbColor;
use enigo::{Enigo, Mouse, Settings};
use screenshots::{Screen, image::RgbaImage};

const LENS_RADIUS: i32 = 8;
const LENS_SCALE: usize = 8;

#[derive(Clone, Debug)]
pub struct PixelPreview {
    pub x: i32,
    pub y: i32,
    pub color: RgbColor,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn mouse_position(enigo: &mut Enigo) -> Result<(i32, i32), String> {
    enigo
        .location()
        .map_err(|error| format!("读取鼠标位置失败：{error}"))
}

pub fn move_mouse(enigo: &mut Enigo, x: i32, y: i32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = enigo;
        // Enigo 0.3 normalizes absolute coordinates against the primary display.
        // SetCursorPos accepts virtual-desktop coordinates, including negative ones.
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y) } == 0 {
            Err(format!("移动鼠标失败：{}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        enigo
            .move_mouse(x, y, enigo::Coordinate::Abs)
            .map_err(|error| format!("移动鼠标失败：{error}"))
    }
}

pub fn sample_pixel(x: i32, y: i32) -> Result<RgbColor, String> {
    let (source, center_x, center_y) = capture_region(x, y, 0)?;
    Ok(rgb(source.get_pixel(center_x, center_y).0))
}

pub fn capture_preview(x: i32, y: i32) -> Result<PixelPreview, String> {
    let (source, center_x, center_y) = capture_region(x, y, LENS_RADIUS)?;
    let color = rgb(source.get_pixel(center_x, center_y).0);
    let preview_width = (LENS_RADIUS * 2 + 1) as usize * LENS_SCALE;
    let mut rgba = vec![0; preview_width * preview_width * 4];
    for dy in -LENS_RADIUS..=LENS_RADIUS {
        for dx in -LENS_RADIUS..=LENS_RADIUS {
            let image_x = center_x as i32 + dx;
            let image_y = center_y as i32 + dy;
            let pixel = if image_x >= 0
                && image_y >= 0
                && image_x < source.width() as i32
                && image_y < source.height() as i32
            {
                source.get_pixel(image_x as u32, image_y as u32).0
            } else {
                [15, 24, 28, 255]
            };
            let output_x = (dx + LENS_RADIUS) as usize * LENS_SCALE;
            let output_y = (dy + LENS_RADIUS) as usize * LENS_SCALE;
            for py in 0..LENS_SCALE {
                for px in 0..LENS_SCALE {
                    let index = ((output_y + py) * preview_width + output_x + px) * 4;
                    rgba[index..index + 4].copy_from_slice(&pixel);
                }
            }
        }
    }
    // A one-pixel outline marks the exact sampled pixel without obscuring its fill.
    let center = LENS_RADIUS as usize * LENS_SCALE;
    for offset in 0..LENS_SCALE {
        for (px, py) in [
            (center + offset, center),
            (center + offset, center + LENS_SCALE - 1),
            (center, center + offset),
            (center + LENS_SCALE - 1, center + offset),
        ] {
            let index = (py * preview_width + px) * 4;
            rgba[index..index + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    Ok(PixelPreview {
        x,
        y,
        color,
        width: preview_width as u32,
        height: preview_width as u32,
        rgba,
    })
}

fn rgb(pixel: [u8; 4]) -> RgbColor {
    RgbColor {
        red: pixel[0],
        green: pixel[1],
        blue: pixel[2],
    }
}

#[derive(Clone, Copy)]
struct ScreenGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f32,
}

impl ScreenGeometry {
    fn for_screen(screen: &Screen) -> Self {
        let info = screen.display_info;
        Self {
            x: info.x,
            y: info.y,
            width: info.width,
            height: info.height,
            // Windows cursor positions and monitor bounds share physical pixels.
            // Bypass screenshots' extra scaling on that platform.
            scale: if cfg!(windows) {
                1.0
            } else {
                info.scale_factor
            },
        }
    }

    fn contains(self, x: i32, y: i32) -> bool {
        let scale = f64::from(self.scale);
        let left = (f64::from(self.x) * scale) as i64;
        let top = (f64::from(self.y) * scale) as i64;
        let right = ((f64::from(self.x) + f64::from(self.width)) * scale) as i64;
        let bottom = ((f64::from(self.y) + f64::from(self.height)) * scale) as i64;
        i64::from(x) >= left && i64::from(x) < right && i64::from(y) >= top && i64::from(y) < bottom
    }
}

#[derive(Debug)]
struct CapturePlan {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    center_x: u32,
    center_y: u32,
}

fn capture_plan(
    geometry: ScreenGeometry,
    x: i32,
    y: i32,
    radius: i32,
) -> Result<CapturePlan, String> {
    if geometry.width == 0
        || geometry.height == 0
        || geometry.width > i32::MAX as u32
        || geometry.height > i32::MAX as u32
        || !geometry.scale.is_finite()
        || geometry.scale <= 0.0
        || radius < 0
        || !geometry.contains(x, y)
    {
        return Err("屏幕尺寸或目标坐标无效。".into());
    }
    let scale = f64::from(geometry.scale);
    let left = ((f64::from(x) - f64::from(radius)) / scale - f64::from(geometry.x))
        .floor()
        .clamp(0.0, f64::from(geometry.width - 1)) as i32;
    let top = ((f64::from(y) - f64::from(radius)) / scale - f64::from(geometry.y))
        .floor()
        .clamp(0.0, f64::from(geometry.height - 1)) as i32;
    let right = ((f64::from(x) + f64::from(radius) + 1.0) / scale - f64::from(geometry.x))
        .ceil()
        .clamp(f64::from(left + 1), f64::from(geometry.width)) as i32;
    let bottom = ((f64::from(y) + f64::from(radius) + 1.0) / scale - f64::from(geometry.y))
        .ceil()
        .clamp(f64::from(top + 1), f64::from(geometry.height)) as i32;
    // Match screenshots' Linux calculation, which scales the global origin.
    let origin_x = ((left as f32 + geometry.x as f32) * geometry.scale) as i32;
    let origin_y = ((top as f32 + geometry.y as f32) * geometry.scale) as i32;
    let center_x = u32::try_from(i64::from(x) - i64::from(origin_x))
        .map_err(|_| "屏幕像素坐标无效。".to_string())?;
    let center_y = u32::try_from(i64::from(y) - i64::from(origin_y))
        .map_err(|_| "屏幕像素坐标无效。".to_string())?;
    Ok(CapturePlan {
        left,
        top,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
        center_x,
        center_y,
    })
}

fn capture_region(x: i32, y: i32, radius: i32) -> Result<(RgbaImage, u32, u32), String> {
    #[cfg(target_os = "linux")]
    let screen = Screen::all()
        .map_err(|error| format!("定位屏幕失败：{error}"))?
        .into_iter()
        .find(|screen| ScreenGeometry::for_screen(screen).contains(x, y))
        .ok_or_else(|| "目标坐标不在任何屏幕上。".to_string())?;
    #[cfg(not(target_os = "linux"))]
    let screen = Screen::from_point(x, y).map_err(|error| format!("定位屏幕失败：{error}"))?;
    let geometry = ScreenGeometry::for_screen(&screen);
    #[cfg(target_os = "macos")]
    let (x, y) = (
        (x as f32 * geometry.scale) as i32,
        (y as f32 * geometry.scale) as i32,
    );
    let plan = capture_plan(geometry, x, y, radius)?;
    #[cfg(windows)]
    let result =
        screen.capture_area_ignore_area_check(plan.left, plan.top, plan.width, plan.height);
    #[cfg(not(windows))]
    let result = screen.capture_area(plan.left, plan.top, plan.width, plan.height);
    let image = result.map_err(|error| format!("读取屏幕像素失败：{error}"))?;
    if plan.center_x >= image.width() || plan.center_y >= image.height() {
        return Err("截屏返回的像素区域不完整，请检查显示缩放设置。".into());
    }
    Ok((image, plan.center_x, plan.center_y))
}

pub fn connect_input() -> Result<Enigo, String> {
    Enigo::new(&Settings::default()).map_err(|error| format!("初始化鼠标控制失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_monitor_coordinates_remain_screen_relative() {
        let geometry = ScreenGeometry {
            x: -1920,
            y: -100,
            width: 1920,
            height: 1080,
            scale: 1.0,
        };
        let plan = capture_plan(geometry, -1500, 200, 8).unwrap();
        assert_eq!(
            (plan.left, plan.top, plan.width, plan.height),
            (412, 292, 17, 17)
        );
        assert_eq!((plan.center_x, plan.center_y), (8, 8));
    }

    #[test]
    fn scaled_capture_keeps_the_exact_physical_center() {
        let geometry = ScreenGeometry {
            x: -1280,
            y: 0,
            width: 1280,
            height: 720,
            scale: 1.5,
        };
        let plan = capture_plan(geometry, -1501, 301, 8).unwrap();
        let origin_x = ((plan.left + geometry.x) as f32 * geometry.scale) as i32;
        let origin_y = ((plan.top + geometry.y) as f32 * geometry.scale) as i32;
        assert_eq!(origin_x + plan.center_x as i32, -1501);
        assert_eq!(origin_y + plan.center_y as i32, 301);
        assert!(plan.center_x >= 8 && plan.center_y >= 8);
    }

    #[test]
    fn edge_capture_clips_without_shifting_the_sampled_pixel() {
        let geometry = ScreenGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
        };
        let plan = capture_plan(geometry, 0, 1079, 8).unwrap();
        assert_eq!(
            (plan.left, plan.top, plan.width, plan.height),
            (0, 1071, 9, 9)
        );
        assert_eq!((plan.center_x, plan.center_y), (0, 8));
        assert!(capture_plan(geometry, -1, 1, 8).is_err());
    }
}
