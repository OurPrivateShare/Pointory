//! Opt-in UI snapshots rendered entirely in memory. This fixture never starts
//! the click engine, installs input hooks, or creates a desktop window.

use crate::{AppWindow, TaskRow};
use slint::{
    Color, ComponentHandle, ModelRc, PhysicalSize, VecModel,
    platform::{
        Platform, PlatformError, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};
use std::{cell::Cell, path::Path, rc::Rc, time::Duration};

struct PreviewPlatform {
    clock: Rc<Cell<Duration>>,
}

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }

    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }
}

fn reference_rows() -> Vec<TaskRow> {
    let rows = [
        (
            "鼠标连点器",
            "跟随鼠标",
            "1000 ms",
            "F1",
            "持续点击",
            "",
            false,
            false,
        ),
        (
            "任务 1",
            "(1281, 585)",
            "1000 ms",
            "",
            "持续点击",
            "#181818",
            false,
            false,
        ),
        (
            "任务 2",
            "(1254, 565)",
            "1000 ms",
            "",
            "持续点击",
            "",
            true,
            false,
        ),
        (
            "任务 3",
            "(988, 547)",
            "1000 ms",
            "",
            "持续点击",
            "#181818",
            true,
            false,
        ),
        (
            "任务 4",
            "(984, 544)",
            "1000–1211 ms",
            "F2",
            "111 次",
            "#FFFFFF",
            true,
            true,
        ),
    ];
    rows.into_iter()
        .enumerate()
        .map(
            |(index, (name, target, interval, hotkey, limit, color, batch, running))| TaskRow {
                id: format!("preview-{index}").into(),
                name: name.into(),
                state: if running { "运行中" } else { "就绪" }.into(),
                hotkey: hotkey.into(),
                target: target.into(),
                interval: interval.into(),
                button: "左键".into(),
                limit: limit.into(),
                color: color.into(),
                color_tolerance: "12".into(),
                swatch: if color == "#FFFFFF" {
                    Color::from_rgb_u8(255, 255, 255)
                } else {
                    Color::from_rgb_u8(24, 24, 24)
                },
                has_color: !color.is_empty(),
                clicks: if running { "38" } else { "0" }.into(),
                running,
                builtin: index == 0,
                include_in_batch: batch,
            },
        )
        .collect()
}

fn reference_window() -> AppWindow {
    let ui = AppWindow::new().expect("create software preview component");
    ui.set_task_rows(ModelRc::new(VecModel::from(reference_rows())));
    ui.set_total_count("5".into());
    ui.set_active_count("1".into());
    ui.set_save_status("已保存".into());
    ui.set_status_message("任务已保存，快捷键可启动或停止任务。".into());
    ui
}

fn save_snapshot(
    ui: &AppWindow,
    directory: &Path,
    filename: &str,
    size: PhysicalSize,
    clock: &Cell<Duration>,
) {
    ui.window().set_size(size);
    ui.show().expect("show in-memory software window");
    clock.set(clock.get() + Duration::from_secs(1));
    slint::platform::update_timers_and_animations();
    // The first render establishes layout and native-control caches. Advancing
    // the virtual clock removes transition frames before the saved render.
    let _ = ui
        .window()
        .take_snapshot()
        .expect("warm up software renderer");
    clock.set(clock.get() + Duration::from_millis(500));
    slint::platform::update_timers_and_animations();
    let image = ui.window().take_snapshot().expect("render UI snapshot");
    assert_eq!((image.width(), image.height()), (size.width, size.height));
    let first_pixel = &image.as_bytes()[..4];
    assert!(
        image
            .as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel != first_pixel),
        "preview was a uniform image: {filename}"
    );
    let path = directory.join(filename);
    screenshots::image::save_buffer(
        &path,
        image.as_bytes(),
        image.width(),
        image.height(),
        screenshots::image::ColorType::Rgba8,
    )
    .expect("write UI preview PNG");
    ui.hide().expect("hide in-memory software window");
    println!("{}", path.display());
}

fn open_custom_editor(ui: &AppWindow) {
    ui.set_editor_open(true);
    ui.set_simple_editor(false);
    ui.set_is_new(false);
    ui.set_task_name("领取奖励".into());
    ui.set_mouse_button("左键".into());
    ui.set_hotkey("F2".into());
    ui.set_target_x("1281".into());
    ui.set_target_y("585".into());
    ui.set_minimum_interval("1000".into());
    ui.set_maximum_interval("1211".into());
    ui.set_maximum_clicks("111".into());
    ui.set_random_interval(true);
    ui.set_limit_clicks(true);
    ui.set_match_color(true);
    ui.set_color_hex("#181818".into());
    ui.set_tolerance("12".into());
    ui.set_include_in_batch(true);
}

#[test]
#[ignore = "manual visual QA; writes software-rendered PNGs without desktop access"]
fn render_ui_previews() {
    let clock = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(PreviewPlatform {
        clock: Rc::clone(&clock),
    }))
    .expect("install isolated software preview platform");
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/validation/ui-0.4.0");
    std::fs::create_dir_all(&directory).expect("create visual validation directory");
    let large = PhysicalSize::new(1060, 820);

    let ui = reference_window();
    save_snapshot(&ui, &directory, "main-1060x820.png", large, &clock);
    save_snapshot(
        &ui,
        &directory,
        "main-920x620.png",
        PhysicalSize::new(920, 620),
        &clock,
    );

    open_custom_editor(&ui);
    save_snapshot(&ui, &directory, "custom-editor.png", large, &clock);
    save_snapshot(
        &ui,
        &directory,
        "custom-editor-920x620.png",
        PhysicalSize::new(920, 620),
        &clock,
    );
    ui.set_editor_error("快捷键 F2 已被另一个任务使用，请更换快捷键。".into());
    save_snapshot(&ui, &directory, "custom-error.png", large, &clock);
    save_snapshot(
        &ui,
        &directory,
        "custom-error-920x620.png",
        PhysicalSize::new(920, 620),
        &clock,
    );

    ui.set_simple_editor(true);
    ui.set_is_new(false);
    ui.set_editor_error("".into());
    ui.set_minimum_interval("1000".into());
    ui.set_hotkey("F1".into());
    save_snapshot(&ui, &directory, "builtin-editor.png", large, &clock);

    ui.set_editor_open(false);
    ui.set_search_query("不存在的任务".into());
    ui.set_task_rows(ModelRc::new(VecModel::from(Vec::<TaskRow>::new())));
    save_snapshot(&ui, &directory, "search-empty.png", large, &clock);
}
