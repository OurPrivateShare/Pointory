#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod model;
mod persistence;
mod platform;

slint::include_modules!();

use engine::{Engine, TaskRuntime};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use model::{AppConfig, MouseButton, RgbColor, Task};
use platform::PixelPreview;
use slint::{ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone)]
enum HotkeyAction {
    RunTask(String),
    PickTarget,
    StopAll,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerMode {
    Off = 0,
    Position = 1,
    Color = 2,
}

struct AppState {
    config: AppConfig,
    selected_id: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = persistence::load().unwrap_or_else(|error| {
        eprintln!("{error}");
        AppConfig::default()
    });
    if config.tasks.is_empty() {
        config.tasks.push(Task::new(1));
    }

    let selected_id = config.tasks.first().map(|task| task.id.clone());
    let state = Rc::new(RefCell::new(AppState {
        config,
        selected_id,
    }));
    let engine = Rc::new(Engine::default());
    let picker_mode = Arc::new(AtomicU8::new(PickerMode::Off as u8));
    let picker_preview = Arc::new(Mutex::new(None::<PixelPreview>));
    start_picker_service(Arc::clone(&picker_mode), Arc::clone(&picker_preview));

    let hotkey_manager = match GlobalHotKeyManager::new() {
        Ok(manager) => Some(Rc::new(manager)),
        Err(error) => {
            eprintln!("无法初始化全局快捷键：{error}");
            None
        }
    };
    let registered_hotkeys = Rc::new(RefCell::new(Vec::<HotKey>::new()));
    let hotkey_actions = Rc::new(RefCell::new(HashMap::<u32, HotkeyAction>::new()));

    let ui = AppWindow::new()?;
    populate_editor(&ui, &state.borrow());
    refresh_task_rows(&ui, &state.borrow(), &engine);
    register_hotkeys(
        hotkey_manager.as_deref(),
        &registered_hotkeys,
        &hotkey_actions,
        &state.borrow().config,
    );

    {
        let weak_ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        let manager = hotkey_manager.clone();
        let registered = Rc::clone(&registered_hotkeys);
        let actions = Rc::clone(&hotkey_actions);
        weak_ui.upgrade().unwrap().on_add_task(move || {
            let Some(ui) = weak_ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            if let Err(error) = collect_editor(&ui, &mut state) {
                ui.set_status_message(error.into());
                return;
            }
            let number = state.config.tasks.len() + 1;
            let task = Task::new(number);
            state.selected_id = Some(task.id.clone());
            state.config.tasks.push(task);
            match persistence::save(&state.config) {
                Ok(()) => ui.set_status_message("新任务已创建并保存。".into()),
                Err(error) => ui.set_status_message(error.into()),
            }
            populate_editor(&ui, &state);
            refresh_task_rows(&ui, &state, &engine);
            if let Some(manager) = manager.as_deref() {
                register_hotkeys(Some(manager), &registered, &actions, &state.config);
            }
        });
    }

    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        let manager = hotkey_manager.clone();
        let registered = Rc::clone(&registered_hotkeys);
        let actions = Rc::clone(&hotkey_actions);
        ui.upgrade().unwrap().on_select_task(move |id| {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            match collect_editor(&ui, &mut state) {
                Ok(()) => {
                    if state.config.tasks.iter().any(|task| task.id == id.as_str()) {
                        state.selected_id = Some(id.to_string());
                        let _ = persistence::save(&state.config);
                        populate_editor(&ui, &state);
                        refresh_task_rows(&ui, &state, &engine);
                    }
                    if let Some(manager) = manager.as_deref() {
                        register_hotkeys(Some(manager), &registered, &actions, &state.config);
                    }
                }
                Err(error) => ui.set_status_message(error.into()),
            }
        });
    }

    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        let manager = hotkey_manager.clone();
        let registered = Rc::clone(&registered_hotkeys);
        let actions = Rc::clone(&hotkey_actions);
        ui.upgrade().unwrap().on_delete_task(move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            if let Some(id) = state.selected_id.clone() {
                engine.stop(&id);
                state.config.tasks.retain(|task| task.id != id);
                state.selected_id = state.config.tasks.first().map(|task| task.id.clone());
                match persistence::save(&state.config) {
                    Ok(()) => ui.set_status_message("任务已删除并保存。".into()),
                    Err(error) => ui.set_status_message(error.into()),
                }
                populate_editor(&ui, &state);
                refresh_task_rows(&ui, &state, &engine);
                if let Some(manager) = manager.as_deref() {
                    register_hotkeys(Some(manager), &registered, &actions, &state.config);
                }
            }
        });
    }

    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        let manager = hotkey_manager.clone();
        let registered = Rc::clone(&registered_hotkeys);
        let actions = Rc::clone(&hotkey_actions);
        ui.upgrade().unwrap().on_save_task(move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            match collect_editor(&ui, &mut state) {
                Ok(()) => {
                    match persistence::save(&state.config) {
                        Ok(()) => ui.set_status_message("任务设置已保存。".into()),
                        Err(error) => ui.set_status_message(error.into()),
                    }
                    refresh_task_rows(&ui, &state, &engine);
                    if let Some(manager) = manager.as_deref() {
                        register_hotkeys(Some(manager), &registered, &actions, &state.config);
                    }
                }
                Err(error) => ui.set_status_message(error.into()),
            }
        });
    }

    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        ui.upgrade().unwrap().on_stop_task(move || {
            let selected = state.borrow().selected_id.clone();
            if let Some(id) = selected {
                engine.stop(&id);
                if let Some(ui) = ui.upgrade() {
                    ui.set_status_message("已发送停止信号给当前任务。".into());
                }
            }
        });
    }

    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        ui.upgrade().unwrap().on_run_task(move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            match collect_editor(&ui, &mut state) {
                Ok(()) => {
                    if let Err(error) = persistence::save(&state.config) {
                        ui.set_status_message(error.into());
                        return;
                    }
                    if let Some(task) = selected_task(&state).cloned() {
                        match engine.start(task) {
                            Ok(()) => ui.set_status_message(
                                "任务已启动；按 Ctrl + Alt + Esc 可立即停止。".into(),
                            ),
                            Err(error) => ui.set_status_message(error.into()),
                        }
                    }
                    refresh_task_rows(&ui, &state, &engine);
                }
                Err(error) => ui.set_status_message(error.into()),
            }
        });
    }

    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        ui.upgrade().unwrap().on_run_batch(move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            match collect_editor(&ui, &mut state) {
                Ok(()) => {
                    if let Err(error) = persistence::save(&state.config) {
                        ui.set_status_message(error.into());
                        return;
                    }
                    let tasks = state
                        .config
                        .tasks
                        .iter()
                        .filter(|task| task.include_in_batch)
                        .cloned()
                        .collect::<Vec<_>>();
                    if tasks.is_empty() {
                        ui.set_status_message("尚未勾选任何批量任务。".into());
                        return;
                    }
                    let mut started = 0;
                    let mut errors = Vec::new();
                    for task in tasks {
                        match engine.start(task) {
                            Ok(()) => started += 1,
                            Err(error) => errors.push(error),
                        }
                    }
                    if errors.is_empty() {
                        ui.set_status_message(
                            format!(
                                "已启动 {started} 个批量任务。请按 Ctrl + Alt + Esc 随时停止。"
                            )
                            .into(),
                        );
                    } else {
                        ui.set_status_message(
                            format!("已启动 {started} 个任务；{}", errors.join(" ")).into(),
                        );
                    }
                    refresh_task_rows(&ui, &state, &engine);
                }
                Err(error) => ui.set_status_message(error.into()),
            }
        });
    }

    {
        let ui = ui.as_weak();
        let engine = Rc::clone(&engine);
        ui.upgrade().unwrap().on_stop_all(move || {
            engine.stop_all();
            if let Some(ui) = ui.upgrade() {
                ui.set_status_message("已发送停止信号给所有任务。".into());
            }
        });
    }

    {
        let ui = ui.as_weak();
        ui.upgrade().unwrap().on_clear_target(move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_target_configured(false);
                ui.set_target_x("".into());
                ui.set_target_y("".into());
                ui.set_status_message("已清除固定位置；保存后将跟随当前鼠标。".into());
            }
        });
    }

    {
        let ui = ui.as_weak();
        let picker_mode = Arc::clone(&picker_mode);
        ui.upgrade().unwrap().on_pick_position(move || {
            picker_mode.store(PickerMode::Position as u8, Ordering::Release);
            if let Some(ui) = ui.upgrade() {
                ui.set_picker_active(true);
                ui.set_status_message("取点模式：将鼠标移至目标位置，再按 F8 确认。".into());
            }
        });
    }

    {
        let ui = ui.as_weak();
        let picker_mode = Arc::clone(&picker_mode);
        ui.upgrade().unwrap().on_pick_color(move || {
            picker_mode.store(PickerMode::Color as u8, Ordering::Release);
            if let Some(ui) = ui.upgrade() {
                ui.set_picker_active(true);
                ui.set_status_message("吸色模式：将鼠标移至目标颜色，再按 F8 确认。".into());
            }
        });
    }

    {
        let ui = ui.as_weak();
        let picker_mode = Arc::clone(&picker_mode);
        ui.upgrade().unwrap().on_cancel_picker(move || {
            picker_mode.store(PickerMode::Off as u8, Ordering::Release);
            if let Some(ui) = ui.upgrade() {
                ui.set_picker_active(false);
                ui.set_status_message("已取消取样。".into());
            }
        });
    }

    {
        let ui = ui.as_weak();
        ui.upgrade().unwrap().on_cycle_button(move || {
            let Some(ui) = ui.upgrade() else { return };
            let next = MouseButton::from_label(&ui.get_mouse_button().to_string()).cycle();
            ui.set_mouse_button(next.label().into());
        });
    }

    let timer = Timer::default();
    {
        let ui = ui.as_weak();
        let state = Rc::clone(&state);
        let engine = Rc::clone(&engine);
        let actions = Rc::clone(&hotkey_actions);
        let picker_mode = Arc::clone(&picker_mode);
        let picker_preview = Arc::clone(&picker_preview);
        timer.start(TimerMode::Repeated, Duration::from_millis(120), move || {
            if let Some(preview) = picker_preview
                .lock()
                .expect("picker preview lock poisoned")
                .clone()
            {
                if let Some(ui) = ui.upgrade() {
                    update_preview(&ui, &preview);
                }
            }

            while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                if event.state() != HotKeyState::Pressed {
                    continue;
                }
                let action = actions.borrow().get(&event.id()).cloned();
                let Some(action) = action else { continue };
                match action {
                    HotkeyAction::RunTask(id) => {
                        let task = state
                            .borrow()
                            .config
                            .tasks
                            .iter()
                            .find(|task| task.id == id)
                            .cloned();
                        if let Some(task) = task {
                            if let Err(error) = engine.start(task) {
                                if let Some(ui) = ui.upgrade() {
                                    ui.set_status_message(error.into());
                                }
                            }
                        }
                    }
                    HotkeyAction::StopAll => {
                        engine.stop_all();
                        if let Some(ui) = ui.upgrade() {
                            ui.set_status_message("全局安全键已触发：所有任务正在停止。".into());
                        }
                    }
                    HotkeyAction::PickTarget => {
                        if picker_mode.load(Ordering::Acquire) != PickerMode::Off as u8 {
                            let preview = picker_preview
                                .lock()
                                .expect("picker preview lock poisoned")
                                .clone()
                                .or_else(|| capture_current_preview().ok());
                            if let Some(preview) = preview {
                                let mode =
                                    picker_mode.swap(PickerMode::Off as u8, Ordering::AcqRel);
                                if let Some(ui) = ui.upgrade() {
                                    complete_pick(&ui, mode, &preview);
                                }
                            } else if let Some(ui) = ui.upgrade() {
                                ui.set_status_message(
                                    "读取鼠标位置失败，请检查桌面输入权限。".into(),
                                );
                            }
                        }
                    }
                }
            }

            let state = state.borrow();
            if let Some(ui) = ui.upgrade() {
                refresh_task_rows(&ui, &state, &engine);
                ui.set_active_count(engine.active_count().to_string().into());
                if let Some(task) = selected_task(&state) {
                    let runtime = engine.runtime(&task.id);
                    ui.set_current_state(runtime_label(&runtime).into());
                } else {
                    ui.set_current_state("就绪".into());
                }
            }
        });
    }

    ui.run()?;
    engine.shutdown();
    picker_mode.store(PickerMode::Off as u8, Ordering::Release);
    Ok(())
}

fn start_picker_service(mode: Arc<AtomicU8>, latest: Arc<Mutex<Option<PixelPreview>>>) {
    thread::Builder::new()
        .name("pointory-picker".into())
        .spawn(move || {
            let mut input = None;
            loop {
                if mode.load(Ordering::Acquire) == PickerMode::Off as u8 {
                    input = None;
                    thread::sleep(Duration::from_millis(140));
                    continue;
                }
                if input.is_none() {
                    input = platform::connect_input().ok();
                }
                if let Some(enigo) = input.as_mut() {
                    if let Ok((x, y)) = platform::mouse_position(enigo) {
                        if let Ok(preview) = platform::capture_preview(x, y) {
                            *latest.lock().expect("picker preview lock poisoned") = Some(preview);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(140));
            }
        })
        .expect("failed to start picker service");
}

fn capture_current_preview() -> Result<PixelPreview, String> {
    let mut enigo = platform::connect_input()?;
    let (x, y) = platform::mouse_position(&mut enigo)?;
    platform::capture_preview(x, y)
}

fn complete_pick(ui: &AppWindow, mode: u8, preview: &PixelPreview) {
    if mode == PickerMode::Position as u8 {
        ui.set_target_x(preview.x.to_string().into());
        ui.set_target_y(preview.y.to_string().into());
        ui.set_target_configured(true);
        ui.set_status_message(
            format!("已取点：({}, {})。保存设置后生效。", preview.x, preview.y).into(),
        );
    } else if mode == PickerMode::Color as u8 {
        ui.set_color_hex(preview.color.to_hex().into());
        ui.set_match_color(true);
        ui.set_status_message(
            format!("已吸取颜色 {}。保存设置后生效。", preview.color.to_hex()).into(),
        );
    }
    ui.set_picker_active(false);
    update_preview(ui, preview);
}

fn update_preview(ui: &AppWindow, preview: &PixelPreview) {
    let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &preview.rgba,
        preview.width,
        preview.height,
    );
    ui.set_magnifier_image(slint::Image::from_rgba8(pixels));
    ui.set_preview_position(
        format!(
            "({}, {}) · {}",
            preview.x,
            preview.y,
            preview.color.to_hex()
        )
        .into(),
    );
    ui.set_preview_color(preview.color.to_hex().into());
    ui.set_preview_swatch(slint::Color::from_rgb_u8(
        preview.color.red,
        preview.color.green,
        preview.color.blue,
    ));
}

fn populate_editor(ui: &AppWindow, state: &AppState) {
    let Some(task) = selected_task(state) else {
        ui.set_task_name("".into());
        ui.set_target_x("".into());
        ui.set_target_y("".into());
        ui.set_target_configured(false);
        ui.set_color_hex("".into());
        ui.set_tolerance("12".into());
        ui.set_minimum_interval("1000".into());
        ui.set_maximum_interval("0".into());
        ui.set_hotkey("".into());
        ui.set_maximum_clicks("0".into());
        ui.set_mouse_button("左键".into());
        ui.set_match_color(false);
        ui.set_include_in_batch(false);
        return;
    };

    ui.set_task_name(task.name.clone().into());
    ui.set_target_x(
        task.x
            .map(|value| value.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_target_y(
        task.y
            .map(|value| value.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_target_configured(task.target().is_some());
    ui.set_color_hex(task.color.map(RgbColor::to_hex).unwrap_or_default().into());
    if let Some(color) = task.color {
        ui.set_preview_swatch(slint::Color::from_rgb_u8(
            color.red,
            color.green,
            color.blue,
        ));
    }
    ui.set_tolerance(task.color_tolerance.to_string().into());
    ui.set_minimum_interval(task.min_interval_ms.to_string().into());
    ui.set_maximum_interval(task.max_interval_ms.to_string().into());
    ui.set_hotkey(task.hotkey.clone().into());
    ui.set_maximum_clicks(task.max_clicks.to_string().into());
    ui.set_mouse_button(task.mouse_button.label().into());
    ui.set_match_color(task.color.is_some());
    ui.set_include_in_batch(task.include_in_batch);
}

fn collect_editor(ui: &AppWindow, state: &mut AppState) -> Result<(), String> {
    let Some(id) = state.selected_id.as_ref() else {
        return Ok(());
    };
    let task = state
        .config
        .tasks
        .iter_mut()
        .find(|task| task.id == *id)
        .ok_or_else(|| "所选任务已不存在。".to_string())?;

    task.name = ui.get_task_name().to_string().trim().to_string();
    if ui.get_target_configured() {
        task.x = Some(parse_field(&ui.get_target_x().to_string(), "X 坐标")?);
        task.y = Some(parse_field(&ui.get_target_y().to_string(), "Y 坐标")?);
    } else {
        task.x = None;
        task.y = None;
    }

    task.color_tolerance = parse_field(&ui.get_tolerance().to_string(), "颜色容差")?;
    task.min_interval_ms = parse_field(&ui.get_minimum_interval().to_string(), "最短间隔")?;
    task.max_interval_ms = parse_field(&ui.get_maximum_interval().to_string(), "最长间隔")?;
    task.hotkey = ui.get_hotkey().to_string().trim().to_string();
    task.max_clicks = parse_field(&ui.get_maximum_clicks().to_string(), "点击次数")?;
    task.mouse_button = MouseButton::from_label(&ui.get_mouse_button().to_string());
    task.include_in_batch = ui.get_include_in_batch();
    task.color = if ui.get_match_color() {
        Some(
            RgbColor::from_hex(&ui.get_color_hex().to_string())
                .ok_or_else(|| "颜色值应为 #RRGGBB 格式，请重新吸取颜色。".to_string())?,
        )
    } else {
        None
    };

    if !task.hotkey.is_empty() {
        let parsed = parse_hotkey(&task.hotkey)?;
        if parsed.id() == parse_hotkey("CTRL+ALT+ESC")?.id()
            || parsed.id() == parse_hotkey("CTRL+SHIFT+P")?.id()
            || parsed.id() == parse_hotkey("F8")?.id()
        {
            return Err("该快捷键已被全局停止或取点功能占用。".into());
        }
    }
    task.validate()
}

fn parse_field<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .trim()
        .parse::<T>()
        .map_err(|_| format!("{label}格式不正确，请检查输入。"))
}

fn selected_task(state: &AppState) -> Option<&Task> {
    let id = state.selected_id.as_ref()?;
    state.config.tasks.iter().find(|task| task.id == *id)
}

fn refresh_task_rows(ui: &AppWindow, state: &AppState, engine: &Engine) {
    let rows = state
        .config
        .tasks
        .iter()
        .map(|task| {
            let runtime = engine.runtime(&task.id);
            TaskRow {
                id: task.id.clone().into(),
                name: task.name.clone().into(),
                state: runtime_label(&runtime).into(),
                hotkey: task.hotkey.clone().into(),
                target: task
                    .target()
                    .map(|(x, y)| format!("{x},{y}"))
                    .unwrap_or_else(|| "鼠标".into())
                    .into(),
                clicks: runtime.clicks.to_string().into(),
                selected: state.selected_id.as_deref() == Some(task.id.as_str()),
                include_in_batch: task.include_in_batch,
            }
        })
        .collect::<Vec<_>>();
    ui.set_task_rows(ModelRc::new(VecModel::from(rows)));
}

fn runtime_label(runtime: &TaskRuntime) -> String {
    if runtime.running {
        runtime.message.clone().if_empty("运行中")
    } else if runtime.message.is_empty() {
        "就绪".into()
    } else {
        runtime.message.clone()
    }
}

trait EmptyFallback {
    fn if_empty(self, fallback: &str) -> String;
}

impl EmptyFallback for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.into()
        } else {
            self
        }
    }
}

fn parse_hotkey(value: &str) -> Result<HotKey, String> {
    value
        .trim()
        .parse::<HotKey>()
        .map_err(|_| format!("快捷键“{value}”无效，示例：CTRL+ALT+F6。"))
}

fn register_hotkeys(
    manager: Option<&GlobalHotKeyManager>,
    registered: &Rc<RefCell<Vec<HotKey>>>,
    actions: &Rc<RefCell<HashMap<u32, HotkeyAction>>>,
    config: &AppConfig,
) {
    let mut registered_hotkeys = registered.borrow_mut();
    let mut hotkey_actions = actions.borrow_mut();
    hotkey_actions.clear();
    if let Some(manager) = manager {
        if !registered_hotkeys.is_empty() {
            let _ = manager.unregister_all(&registered_hotkeys);
        }
        registered_hotkeys.clear();

        for (label, action) in [
            ("CTRL+ALT+ESC", HotkeyAction::StopAll),
            ("CTRL+SHIFT+P", HotkeyAction::PickTarget),
            ("F8", HotkeyAction::PickTarget),
        ] {
            if let Ok(hotkey) = label.parse::<HotKey>() {
                if manager.register(hotkey).is_ok() {
                    hotkey_actions.insert(hotkey.id(), action);
                    registered_hotkeys.push(hotkey);
                }
            }
        }

        let mut seen = hotkey_actions.keys().copied().collect::<HashSet<_>>();
        for task in &config.tasks {
            if task.hotkey.trim().is_empty() {
                continue;
            }
            let Ok(hotkey) = parse_hotkey(&task.hotkey) else {
                continue;
            };
            if !seen.insert(hotkey.id()) {
                continue;
            }
            if manager.register(hotkey).is_ok() {
                hotkey_actions.insert(hotkey.id(), HotkeyAction::RunTask(task.id.clone()));
                registered_hotkeys.push(hotkey);
            }
        }
    }
}
