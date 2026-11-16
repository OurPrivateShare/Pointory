use crate::model::RgbColor;
use enigo::{Enigo, Mouse, Settings};
use screenshots::Screen;

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

pub fn sample_pixel(x: i32, y: i32) -> Result<RgbColor, String> {
    let screen = Screen::from_point(x, y).map_err(|error| format!("定位屏幕失败：{error}"))?;
    let info = screen.display_info;
    let local_x = x.saturating_sub(info.x);
    let local_y = y.saturating_sub(info.y);
    let image = screen
        .capture_area(local_x, local_y, 1, 1)
        .map_err(|error| format!("读取屏幕像素失败：{error}"))?;
    let pixel = image.get_pixel(0, 0).0;
    Ok(RgbColor {
        red: pixel[0],
        green: pixel[1],
        blue: pixel[2],
    })
}

pub fn capture_preview(x: i32, y: i32) -> Result<PixelPreview, String> {
    let screen = Screen::from_point(x, y).map_err(|error| format!("定位屏幕失败：{error}"))?;
    let info = screen.display_info;
    let width = info.width as i32;
    let height = info.height as i32;
    if width <= 0 || height <= 0 {
        return Err("屏幕尺寸无效。".into());
    }

    let center_x = x.saturating_sub(info.x).clamp(0, width - 1);
    let center_y = y.saturating_sub(info.y).clamp(0, height - 1);
    let left = (center_x - LENS_RADIUS).max(0);
    let top = (center_y - LENS_RADIUS).max(0);
    let right = (center_x + LENS_RADIUS + 1).min(width);
    let bottom = (center_y + LENS_RADIUS + 1).min(height);
    let source = screen
        .capture_area(left, top, (right - left) as u32, (bottom - top) as u32)
        .map_err(|error| format!("放大镜截屏失败：{error}"))?;
    let center = source
        .get_pixel((center_x - left) as u32, (center_y - top) as u32)
        .0;
    let color = RgbColor {
        red: center[0],
        green: center[1],
        blue: center[2],
    };

    let source_width = source.width() as i32;
    let source_height = source.height() as i32;
    let preview_width = ((LENS_RADIUS * 2 + 1) as usize * LENS_SCALE) as u32;
    let preview_height = preview_width;
    let mut rgba = vec![0; preview_width as usize * preview_height as usize * 4];

    for source_y in -LENS_RADIUS..=LENS_RADIUS {
        for source_x in -LENS_RADIUS..=LENS_RADIUS {
            let image_x = center_x - left + source_x;
            let image_y = center_y - top + source_y;
            let pixel = if image_x >= 0
                && image_y >= 0
                && image_x < source_width
                && image_y < source_height
            {
                source.get_pixel(image_x as u32, image_y as u32).0
            } else {
                [15, 24, 28, 255]
            };
            let output_x = (source_x + LENS_RADIUS) as usize * LENS_SCALE;
            let output_y = (source_y + LENS_RADIUS) as usize * LENS_SCALE;
            for dy in 0..LENS_SCALE {
                for dx in 0..LENS_SCALE {
                    let index = ((output_y + dy) * preview_width as usize + output_x + dx) * 4;
                    rgba[index..index + 4].copy_from_slice(&pixel);
                }
            }
        }
    }

    Ok(PixelPreview {
        x,
        y,
        color,
        width: preview_width,
        height: preview_height,
        rgba,
    })
}

pub fn connect_input() -> Result<Enigo, String> {
    Enigo::new(&Settings::default()).map_err(|error| format!("初始化鼠标控制失败：{error}"))
}
