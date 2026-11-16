#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod hotkeys;
mod instance;
mod magnifier;
mod model;
mod persistence;
mod picker;
mod platform;
mod preview;

slint::include_modules!();

use engine::Engine;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use hotkeys::{Action, Hotkeys};
use model::{AppConfig, MouseButton, Position, RgbColor, Task};
use preview::{PreviewService, Update};
use slint::winit_030::WinitWindowAccessor;
use slint::{Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum PickerMode {
    Position,
    Color,
}

struct ActivePick {
    mode: PickerMode,
    token: u64,
    input: Option<picker::PickerSession>,
    color_sample: Option<picker::Selection>,
    preview_started: bool,
    ready_at: Instant,
}

#[derive(Clone)]
struct Draft {
    task: Task,
    is_new: bool,
}

struct Controller {
    config: RefCell<AppConfig>,
    draft: RefCell<Option<Draft>>,
    deleting: RefCell<Option<String>>,
    engine: Engine,
    hotkeys: RefCell<Hotkeys>,
    picking: RefCell<Option<ActivePick>>,
    next_pick_token: Cell<u64>,
    preview: PreviewService,
    magnifier: MagnifierWindow,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    magnifier::configure_backend()?;
    let _instance = match instance::acquire() {
        Ok(lock) => lock,
        Err(message) => {
            let notice = NoticeWindow::new()?;
            notice.set_message(message.into());
            notice.on_dismiss({
                let weak = notice.as_weak();
                move || {
                    if let Some(notice) = weak.upgrade() {
                        let _ = notice.hide();
                    }
                }
            });
            notice.run()?;
            return Ok(());
        }
    };
    let (config, load_error) = match persistence::load() {
        Ok(config) => (config, None),
        Err(error) => (AppConfig::default(), Some(error)),
    };
    let hotkeys = Hotkeys::new(&config);
    let warning = [load_error.unwrap_or_default(), hotkeys.warning.clone()]
        .into_iter()
        .filter(|message| !message.is_empty())
        .collect::<Vec<_>>()
        .join("；");
    let controller = Rc::new(Controller {
        config: RefCell::new(config),
        draft: RefCell::new(None),
        deleting: RefCell::new(None),
        engine: Engine::default(),
        hotkeys: RefCell::new(hotkeys),
        picking: RefCell::new(None),
        next_pick_token: Cell::new(0),
        preview: PreviewService::new()?,
        magnifier: magnifier::new_window()?,
    });
    let ui = AppWindow::new()?;
    if !warning.is_empty() {
        ui.set_status_message(warning.into());
    }
    controller.refresh(&ui);

    macro_rules! bind {
        ($callback:ident, $method:ident) => {{
            let weak = ui.as_weak();
            let controller = Rc::clone(&controller);
            ui.$callback(move || {
                if let Some(ui) = weak.upgrade() {
                    controller.$method(&ui);
                }
            });
        }};
    }
    macro_rules! bind_id {
        ($callback:ident, $method:ident) => {{
            let weak = ui.as_weak();
            let controller = Rc::clone(&controller);
            ui.$callback(move |id| {
                if let Some(ui) = weak.upgrade() {
                    controller.$method(&ui, id.as_str());
                }
            });
        }};
    }
    bind!(on_add_task, add);
    bind_id!(on_edit_task, edit);
    bind_id!(on_toggle_task, toggle);
    bind_id!(on_delete_task, request_delete);
    bind!(on_confirm_delete, delete);
    bind!(on_cancel_delete, cancel_delete);
    bind!(on_save_task, save);
    bind!(on_cancel_editor, cancel_editor);
    bind!(on_run_batch, run_batch);
    bind!(on_stop_all, stop_all);
    bind!(on_pick_position, pick_position);
    bind!(on_pick_color, pick_color);
    bind!(on_cancel_picker, cancel_picker);
    ui.on_cycle_button({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_mouse_button(
                    MouseButton::from_label(ui.get_mouse_button().as_str())
                        .cycle()
                        .label()
                        .into(),
                );
            }
        }
    });

    ui.window().on_close_requested({
        let controller = Rc::clone(&controller);
        move || {
            controller.shutdown_picker();
            controller.engine.shutdown();
            slint::CloseRequestResponse::HideWindow
        }
    });
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), {
        let weak = ui.as_weak();
        let controller = Rc::clone(&controller);
        move || {
            if let Some(ui) = weak.upgrade() {
                controller.tick(&ui);
            }
        }
    });
    let result = ui.run();
    timer.stop();
    controller.engine.shutdown();
    controller.shutdown_picker();
    result?;
    Ok(())
}

impl Controller {
    fn add(&self, ui: &AppWindow) {
        let number = self.config.borrow().tasks.len();
        self.open_editor(
            ui,
            Draft {
                task: Task::new(number),
                is_new: true,
            },
        );
    }

    fn edit(&self, ui: &AppWindow, id: &str) {
        if self.engine.runtime(id).running {
            ui.set_status_message("请先停止任务，再编辑设置。".into());
            return;
        }
        let task = self
            .config
            .borrow()
            .tasks
            .iter()
            .find(|task| task.id == id)
            .cloned();
        if let Some(task) = task {
            self.open_editor(
                ui,
                Draft {
                    task,
                    is_new: false,
                },
            );
        }
    }

    fn open_editor(&self, ui: &AppWindow, draft: Draft) {
        self.cancel_picker(ui);
        populate_editor(ui, &draft.task);
        ui.set_is_new(draft.is_new);
        ui.set_simple_editor(draft.task.is_builtin());
        ui.set_editor_error("".into());
        ui.set_picker_active(false);
        *self.draft.borrow_mut() = Some(draft);
        ui.set_editor_open(true);
    }

    fn cancel_editor(&self, ui: &AppWindow) {
        self.cancel_picker(ui);
        self.draft.borrow_mut().take();
        ui.set_picker_active(false);
        ui.set_editor_open(false);
    }

    fn save(&self, ui: &AppWindow) {
        let result = (|| {
            let draft = self.draft.borrow().clone().ok_or("没有正在编辑的任务。")?;
            if self.engine.runtime(&draft.task.id).running {
                return Err("请先停止任务，再保存设置。".into());
            }
            let task = collect_editor(ui, &draft.task)?;
            let mut next = self.config.borrow().clone();
            if draft.is_new {
                next.tasks.push(task);
            } else {
                let slot = next
                    .tasks
                    .iter_mut()
                    .find(|item| item.id == task.id)
                    .ok_or("任务已不存在。")?;
                *slot = task;
            }
            self.commit(next)
        })();
        match result {
            Ok(()) => {
                self.cancel_editor(ui);
                let warning = self.hotkeys.borrow().warning.clone();
                ui.set_status_message(if warning.is_empty() {
                    "任务已保存。快捷键可启动或停止任务。".into()
                } else {
                    format!("任务已保存。{warning}").into()
                });
                self.refresh(ui);
            }
            Err(error) => ui.set_editor_error(error.into()),
        }
    }

    fn commit(&self, next: AppConfig) -> Result<(), String> {
        next.validate()?;
        hotkeys::validate(&next)?;
        self.hotkeys.borrow_mut().replace(&next)?;
        if let Err(error) = persistence::save(&next) {
            let rollback = self.hotkeys.borrow_mut().replace(&self.config.borrow());
            return Err(match rollback {
                Ok(()) => error,
                Err(rollback) => {
                    self.hotkeys
                        .borrow_mut()
                        .align_actions(&self.config.borrow());
                    format!("{error}；快捷键恢复失败：{rollback}")
                }
            });
        }
        *self.config.borrow_mut() = next;
        Ok(())
    }

    fn request_delete(&self, ui: &AppWindow, id: &str) {
        let config = self.config.borrow();
        let Some(task) = config.tasks.iter().find(|task| task.id == id) else {
            return;
        };
        if task.is_builtin() {
            return;
        }
        *self.deleting.borrow_mut() = Some(task.id.clone());
        ui.set_delete_name(task.name.clone().into());
        ui.set_delete_open(true);
    }

    fn cancel_delete(&self, ui: &AppWindow) {
        self.deleting.borrow_mut().take();
        ui.set_delete_open(false);
    }

    fn delete(&self, ui: &AppWindow) {
        let Some(id) = self.deleting.borrow().clone() else {
            return;
        };
        let mut next = self.config.borrow().clone();
        next.tasks.retain(|task| task.id != id || task.is_builtin());
        self.engine.stop(&id);
        match self.commit(next) {
            Ok(()) => {
                self.engine.forget(&id);
                self.cancel_delete(ui);
                ui.set_status_message("任务已删除。".into());
            }
            Err(error) => {
                self.cancel_delete(ui);
                ui.set_status_message(error.into());
            }
        }
        self.refresh(ui);
    }

    fn toggle(&self, ui: &AppWindow, id: &str) {
        if self.engine.runtime(id).running {
            self.engine.stop(id);
            ui.set_status_message("任务已停止。".into());
        } else {
            if ui.get_editor_open() || ui.get_delete_open() {
                return;
            }
            let task = self
                .config
                .borrow()
                .tasks
                .iter()
                .find(|task| task.id == id)
                .cloned();
            if let Some(task) = task {
                match self.engine.start(task) {
                    Ok(()) => ui.set_status_message(
                        "任务已启动；再次按任务快捷键可停止。Ctrl + Alt + Esc 停止全部。".into(),
                    ),
                    Err(error) => ui.set_status_message(error.into()),
                }
            }
        }
        self.refresh(ui);
    }

    fn run_batch(&self, ui: &AppWindow) {
        if ui.get_editor_open() || ui.get_delete_open() {
            return;
        }
        let tasks = self
            .config
            .borrow()
            .tasks
            .iter()
            .filter(|task| task.include_in_batch)
            .cloned()
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            ui.set_status_message("请先在任务编辑弹窗中勾选“加入批量执行”。".into());
            return;
        }
        let mut started = 0;
        let mut failures = Vec::new();
        for task in tasks {
            if self.engine.runtime(&task.id).running {
                continue;
            }
            let name = task.name.clone();
            match self.engine.start(task) {
                Ok(()) => started += 1,
                Err(error) => failures.push(format!("{name}：{error}")),
            }
        }
        ui.set_status_message(
            format!("已启动 {started} 个批量任务。{}", failures.join("；")).into(),
        );
        self.refresh(ui);
    }

    fn stop_all(&self, ui: &AppWindow) {
        self.engine.stop_all();
        self.cancel_picker(ui);
        ui.set_status_message("全部任务已停止。".into());
        self.refresh(ui);
    }

    fn pick_position(&self, ui: &AppWindow) {
        self.begin_pick(ui, PickerMode::Position);
    }
    fn pick_color(&self, ui: &AppWindow) {
        self.begin_pick(ui, PickerMode::Color);
    }

    fn begin_pick(&self, ui: &AppWindow, mode: PickerMode) {
        if !ui.get_editor_open() || ui.get_simple_editor() {
            return;
        }
        self.engine.stop_all();
        self.cancel_picker(ui);
        let input = match picker::start() {
            Ok(input) => input,
            Err(error) => {
                ui.set_editor_error(error.into());
                return;
            }
        };
        let token = self.next_pick_token.get().wrapping_add(1);
        self.next_pick_token.set(token);
        *self.picking.borrow_mut() = Some(ActivePick {
            mode,
            token,
            input: Some(input),
            color_sample: None,
            preview_started: false,
            ready_at: Instant::now() + Duration::from_millis(250),
        });
        ui.set_editor_error("".into());
        ui.set_picker_active(true);
        ui.window().set_minimized(true);
    }

    fn cancel_picker(&self, ui: &AppWindow) {
        let active = self.picking.borrow_mut().take().is_some();
        self.preview.stop();
        let _ = self.magnifier.hide();
        ui.set_picker_active(false);
        if active {
            ui.window().set_minimized(false);
            ui.window()
                .with_winit_window(|window| window.focus_window());
        }
    }

    fn shutdown_picker(&self) {
        self.picking.borrow_mut().take();
        self.preview.stop();
        let _ = self.magnifier.hide();
    }

    fn poll_picker(&self, ui: &AppWindow) {
        let event = self
            .picking
            .borrow()
            .as_ref()
            .and_then(|active| active.input.as_ref())
            .and_then(picker::PickerSession::try_event);
        match event {
            Some(picker::Event::Selected(point)) => {
                let selection = {
                    let mut picking = self.picking.borrow_mut();
                    picking.as_mut().map(|active| {
                        active.input.take();
                        active.mode
                    })
                };
                if let Some(mode) = selection {
                    let _ = self.magnifier.hide();
                    match mode {
                        PickerMode::Position => {
                            ui.set_target_x(point.x.to_string().into());
                            ui.set_target_y(point.y.to_string().into());
                            self.cancel_picker(ui);
                            ui.set_editor_error("".into());
                        }
                        PickerMode::Color => {
                            self.preview.stop();
                            if let Some(active) = self.picking.borrow_mut().as_mut() {
                                active.color_sample = Some(point);
                            }
                        }
                    }
                }
            }
            Some(picker::Event::Cancelled) => self.cancel_picker(ui),
            Some(picker::Event::Error(error)) => {
                self.cancel_picker(ui);
                ui.set_editor_error(error.into());
            }
            None => {}
        }
        {
            let mut picking = self.picking.borrow_mut();
            if let Some(active) = picking.as_mut()
                && active.input.is_none()
                && Instant::now() >= active.ready_at
                && let Some(point) = active.color_sample.take()
            {
                // Let the minimize animation finish before reading the desktop,
                // even if the confirmation click arrived unusually quickly.
                active.ready_at = Instant::now() + Duration::from_secs(5);
                self.preview.sample_color(active.token, point.x, point.y);
            }
        }
        let timed_out = self.picking.borrow().as_ref().is_some_and(|active| {
            active.input.is_none()
                && active.color_sample.is_none()
                && Instant::now() >= active.ready_at
        });
        if timed_out {
            self.cancel_picker(ui);
            ui.set_editor_error("吸取颜色超时，请重新取样或手工填写颜色。".into());
        }
        {
            let mut picking = self.picking.borrow_mut();
            if let Some(active) = picking.as_mut()
                && active.input.is_some()
                && !active.preview_started
                && Instant::now() >= active.ready_at
            {
                active.preview_started = true;
                self.preview.start(active.token);
            }
        }
        if let Some(update) = self.preview.take_update() {
            let active_token = self.picking.borrow().as_ref().map(|active| active.token);
            match update {
                Update::Frame { token, preview } if active_token == Some(token) => {
                    // Keep the lens off the sampled pixel and let clicks pass through it.
                    if self
                        .picking
                        .borrow()
                        .as_ref()
                        .is_some_and(|active| active.input.is_some())
                        && let Err(error) = magnifier::update(&self.magnifier, &preview)
                    {
                        self.preview.stop();
                        let _ = self.magnifier.hide();
                        ui.set_editor_error(
                            format!("放大镜不可用：{error}；仍可左键确认位置，或 Esc 取消。")
                                .into(),
                        );
                    }
                }
                Update::Color { token, result } if active_token == Some(token) => {
                    self.cancel_picker(ui);
                    match result {
                        Ok(color) => {
                            ui.set_color_hex(color.to_hex().into());
                            ui.set_match_color(true);
                            ui.set_editor_error("".into());
                        }
                        Err(error) => ui.set_editor_error(error.into()),
                    }
                }
                Update::Error { token, message } if active_token == Some(token) => {
                    self.preview.stop();
                    let _ = self.magnifier.hide();
                    ui.set_editor_error(
                        format!("放大镜不可用：{message}；仍可左键确认位置，或 Esc 取消。").into(),
                    );
                }
                _ => {}
            }
        }
    }

    fn tick(&self, ui: &AppWindow) {
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state() != HotKeyState::Pressed {
                continue;
            }
            let action = self.hotkeys.borrow().action(event.id());
            match action {
                Some(Action::ToggleTask(id)) => self.toggle(ui, &id),
                Some(Action::StopAll) => self.stop_all(ui),
                _ => {}
            }
        }
        self.poll_picker(ui);
        self.refresh(ui);
    }

    fn refresh(&self, ui: &AppWindow) {
        ui.set_hotkey_warning(self.hotkeys.borrow().warning.clone().into());
        ui.set_active_count(self.engine.active_count().to_string().into());
        let rows = self
            .config
            .borrow()
            .tasks
            .iter()
            .map(|task| {
                let runtime = self.engine.runtime(&task.id);
                TaskRow {
                    id: task.id.clone().into(),
                    name: task.name.clone().into(),
                    state: if runtime.message.is_empty() {
                        "就绪".into()
                    } else {
                        runtime.message.into()
                    },
                    hotkey: if task.hotkey.is_empty() {
                        "未设置".into()
                    } else {
                        task.hotkey.clone().into()
                    },
                    target: task
                        .target()
                        .map(|(x, y)| format!("({x}, {y})"))
                        .unwrap_or_else(|| "跟随鼠标".into())
                        .into(),
                    interval: if task.max_interval_ms > task.min_interval_ms {
                        format!("{}–{} ms", task.min_interval_ms, task.max_interval_ms)
                    } else {
                        format!("{} ms", task.min_interval_ms)
                    }
                    .into(),
                    button: task.mouse_button.label().into(),
                    limit: if task.max_clicks == 0 {
                        "持续点击".into()
                    } else {
                        format!("{} 次", task.max_clicks).into()
                    },
                    color: task
                        .color
                        .map(|color| format!("颜色 {} ±{}", color.to_hex(), task.color_tolerance))
                        .unwrap_or_default()
                        .into(),
                    clicks: runtime.clicks.to_string().into(),
                    running: runtime.running,
                    builtin: task.is_builtin(),
                    include_in_batch: task.include_in_batch,
                }
            })
            .collect::<Vec<_>>();
        let current = ui.get_task_rows();
        if current.row_count() != rows.len() {
            ui.set_task_rows(ModelRc::new(VecModel::from(rows)));
        } else {
            for (index, row) in rows.into_iter().enumerate() {
                if current.row_data(index).as_ref() != Some(&row) {
                    current.set_row_data(index, row);
                }
            }
        }
    }
}

fn populate_editor(ui: &AppWindow, task: &Task) {
    ui.set_task_name(task.name.clone().into());
    ui.set_target_x(
        task.position
            .map(|point| point.x.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_target_y(
        task.position
            .map(|point| point.y.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_color_hex(task.color.map(RgbColor::to_hex).unwrap_or_default().into());
    ui.set_tolerance(task.color_tolerance.to_string().into());
    ui.set_minimum_interval(task.min_interval_ms.to_string().into());
    ui.set_maximum_interval(task.max_interval_ms.to_string().into());
    ui.set_hotkey(task.hotkey.clone().into());
    ui.set_maximum_clicks(task.max_clicks.to_string().into());
    ui.set_mouse_button(task.mouse_button.label().into());
    ui.set_match_color(task.color.is_some());
    ui.set_include_in_batch(task.include_in_batch);
}

fn collect_editor(ui: &AppWindow, original: &Task) -> Result<Task, String> {
    let mut task = original.clone();
    task.min_interval_ms = parse_field(ui.get_minimum_interval().as_str(), "点击间隔")?;
    task.hotkey = ui.get_hotkey().trim().to_string();
    if !task.is_builtin() {
        task.name = ui.get_task_name().trim().to_string();
        if ui.get_target_x().trim().is_empty() || ui.get_target_y().trim().is_empty() {
            return Err("请先使用“位置取点”，或填写 X、Y 坐标。".into());
        }
        task.position = Some(Position {
            x: parse_field(ui.get_target_x().as_str(), "X 坐标")?,
            y: parse_field(ui.get_target_y().as_str(), "Y 坐标")?,
        });
        if ui.get_match_color() {
            task.color_tolerance = parse_field(ui.get_tolerance().as_str(), "颜色容差")?;
        }
        task.max_interval_ms = parse_field(ui.get_maximum_interval().as_str(), "最长间隔")?;
        task.max_clicks = parse_field(ui.get_maximum_clicks().as_str(), "点击次数")?;
        task.mouse_button = MouseButton::from_label(ui.get_mouse_button().as_str());
        task.include_in_batch = ui.get_include_in_batch();
        task.color = if ui.get_match_color() {
            Some(RgbColor::from_hex(ui.get_color_hex().as_str()).ok_or("颜色格式应为 #RRGGBB。")?)
        } else {
            None
        };
    }
    task.validate()?;
    Ok(task)
}

fn parse_field<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("{label}格式不正确，请输入有效整数。"))
}
