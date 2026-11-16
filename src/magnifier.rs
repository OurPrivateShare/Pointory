use crate::{MagnifierWindow, platform};
use slint::{
    ComponentHandle, PhysicalPosition, Rgba8Pixel, SharedPixelBuffer,
    winit_030::{WinitWindowAccessor, winit::window::WindowLevel},
};
use std::cell::Cell;

const WINDOW_TITLE: &str = "Pointory · 屏幕取样";
const OFFSET: i64 = 24;
const CAPTURE_MARGIN: i64 = 9;

thread_local! {
    // Slint's attributes hook runs before the component title is applied.
    static CREATING_MAGNIFIER: Cell<bool> = const { Cell::new(false) };
}

pub fn configure_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|attributes| {
            if attributes.title != WINDOW_TITLE && !CREATING_MAGNIFIER.get() {
                return attributes;
            }
            let attributes = attributes
                .with_active(false)
                .with_window_level(WindowLevel::AlwaysOnTop);
            #[cfg(windows)]
            {
                use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
                // Excluding this utility window prevents feedback when the pointer
                // crosses its previous position before the next preview arrives.
                attributes
                    .with_skip_taskbar(true)
                    .with_content_protected(true)
            }
            #[cfg(not(windows))]
            {
                attributes
            }
        })
        .select()
}

pub fn new_window() -> Result<MagnifierWindow, slint::PlatformError> {
    CREATING_MAGNIFIER.with(|flag| {
        struct Reset<'a>(&'a Cell<bool>, bool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _reset = Reset(flag, flag.replace(true));
        MagnifierWindow::new()
    })
}

pub fn update(window: &MagnifierWindow, preview: &platform::PixelPreview) -> Result<(), String> {
    let image = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &preview.rgba,
        preview.width,
        preview.height,
    );
    window.set_magnifier_image(slint::Image::from_rgba8(image));
    window.set_preview_position(
        format!(
            "({}, {}) · {}",
            preview.x,
            preview.y,
            preview.color.to_hex()
        )
        .into(),
    );
    window.set_error_message("".into());
    let bounds = platform::screen_bounds(preview.x, preview.y)?;
    position(window, preview, bounds)?;
    if !window.window().is_visible() {
        window
            .show()
            .map_err(|error| format!("无法显示取样放大镜：{error}"))?;
        let click_through = window.window().with_winit_window(|native| {
            native
                .set_cursor_hittest(false)
                .map_err(|error| format!("取样放大镜无法设置鼠标穿透：{error}"))
        });
        match click_through {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                let _ = window.hide();
                return Err(error);
            }
            None => {
                let _ = window.hide();
                return Err("取样放大镜的系统窗口尚未就绪。".into());
            }
        }
        // Moving to a differently scaled monitor can change the physical size.
        position(window, preview, bounds)?;
    }
    Ok(())
}

fn position(
    window: &MagnifierWindow,
    preview: &platform::PixelPreview,
    bounds: (i32, i32, u32, u32),
) -> Result<(), String> {
    let size = window.window().size();
    let scale = window.window().scale_factor().max(0.1);
    let width = if size.width == 0 {
        (208.0 * scale).ceil() as u32
    } else {
        size.width
    };
    let height = if size.height == 0 {
        (210.0 * scale).ceil() as u32
    } else {
        size.height
    };
    let (x, y) = placement((preview.x, preview.y), (width, height), bounds)?;
    window.window().set_position(PhysicalPosition::new(x, y));
    Ok(())
}

fn placement(
    cursor: (i32, i32),
    size: (u32, u32),
    bounds: (i32, i32, u32, u32),
) -> Result<(i32, i32), String> {
    let (left, top) = (i64::from(bounds.0), i64::from(bounds.1));
    let (right, bottom) = (left + i64::from(bounds.2), top + i64::from(bounds.3));
    let (width, height) = (i64::from(size.0), i64::from(size.1));
    if width == 0 || height == 0 || width > i64::from(bounds.2) || height > i64::from(bounds.3) {
        return Err("屏幕可用空间不足，无法显示取样放大镜。".into());
    }
    let (cursor_x, cursor_y) = (i64::from(cursor.0), i64::from(cursor.1));
    let x = if cursor_x + OFFSET + width <= right {
        cursor_x + OFFSET
    } else {
        cursor_x - OFFSET - width
    }
    .clamp(left, right - width);
    let y = if cursor_y + OFFSET + height <= bottom {
        cursor_y + OFFSET
    } else {
        cursor_y - OFFSET - height
    }
    .clamp(top, bottom - height);
    // Never put the magnifier over the 17x17 pixel sampling region.
    if x < cursor_x + CAPTURE_MARGIN
        && x + width > cursor_x - CAPTURE_MARGIN
        && y < cursor_y + CAPTURE_MARGIN
        && y + height > cursor_y - CAPTURE_MARGIN
    {
        return Err("当前位置无法避让取样区域，已隐藏放大镜。".into());
    }
    Ok((
        i32::try_from(x).map_err(|_| "放大镜横坐标超出系统范围。")?,
        i32::try_from(y).map_err(|_| "放大镜纵坐标超出系统范围。")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_cursor_uses_bottom_right_without_covering_sampling_region() {
        assert_eq!(
            placement((500, 400), (208, 210), (0, 0, 1920, 1080)).unwrap(),
            (524, 424)
        );
    }

    #[test]
    fn display_edges_flip_to_left_and_above() {
        assert_eq!(
            placement((1900, 1060), (208, 210), (0, 0, 1920, 1080)).unwrap(),
            (1668, 826)
        );
        assert_eq!(
            placement((0, 0), (208, 210), (0, 0, 1920, 1080)).unwrap(),
            (24, 24)
        );
    }

    #[test]
    fn negative_monitor_origins_and_scaled_window_sizes_are_physical() {
        let bounds = (-1920, -120, 1920, 1080);
        assert_eq!(
            placement((-300, 800), (416, 420), bounds).unwrap(),
            (-740, 356)
        );
        assert_eq!(
            placement((-1900, -100), (312, 315), bounds).unwrap(),
            (-1876, -76)
        );
    }

    #[test]
    fn insufficient_space_hides_instead_of_covering_the_cursor() {
        assert!(placement((20, 20), (208, 210), (0, 0, 100, 100)).is_err());
        assert!(placement((100, 100), (208, 210), (0, 0, 220, 220)).is_err());
    }
}
