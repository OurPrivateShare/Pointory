use crate::{
    model::{MouseButton, RgbColor, Task},
    platform,
};
use enigo::{Button, Direction, Enigo, Mouse};
use rand::thread_rng;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// All tasks share one mouse. Sampling, positioning and clicking must stay together.
static INPUT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default)]
pub struct TaskRuntime {
    pub running: bool,
    pub clicks: u64,
    pub skipped: u64,
    pub message: String,
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn signal_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.thread.thread().unpark();
    }
}

#[derive(Default)]
pub struct Engine {
    workers: Mutex<HashMap<String, Worker>>,
    runtime: Arc<Mutex<HashMap<String, TaskRuntime>>>,
}

impl Engine {
    pub fn start(&self, task: Task) -> Result<(), String> {
        task.validate()?;
        self.reap_finished();
        let mut workers = self.workers.lock().expect("worker lock poisoned");
        if workers.contains_key(&task.id) {
            return Err("这个任务已经在运行。".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let runtime = Arc::clone(&self.runtime);
        let task_id = task.id.clone();
        self.set_runtime(
            &task_id,
            TaskRuntime {
                running: true,
                message: "正在启动".into(),
                ..TaskRuntime::default()
            },
        );
        let thread = match thread::Builder::new()
            .name(format!("pointory-{}", task.id))
            .spawn(move || {
                let id = task.id.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_task(task, &worker_stop, &runtime);
                }));
                if result.is_err() {
                    update_runtime(&runtime, &id, |state| {
                        state.running = false;
                        state.message = "任务执行异常，已停止。".into();
                    });
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let message = format!("无法启动任务线程：{error}");
                self.set_runtime(
                    &task_id,
                    TaskRuntime {
                        message: message.clone(),
                        ..TaskRuntime::default()
                    },
                );
                return Err(message);
            }
        };
        workers.insert(task_id, Worker { stop, thread });
        Ok(())
    }

    // Join the final native input call before callers edit, delete or restart a task.
    pub fn stop(&self, task_id: &str) {
        let mut workers = self.workers.lock().expect("worker lock poisoned");
        if let Some(worker) = workers.remove(task_id) {
            worker.signal_stop();
            let _ = worker.thread.join();
        }
    }

    pub fn stop_all(&self) {
        let mut workers = self.workers.lock().expect("worker lock poisoned");
        for worker in workers.values() {
            worker.signal_stop();
        }
        for (_, worker) in workers.drain() {
            let _ = worker.thread.join();
        }
    }

    pub fn forget(&self, task_id: &str) {
        self.stop(task_id);
        self.runtime
            .lock()
            .expect("runtime lock poisoned")
            .remove(task_id);
    }

    pub fn runtime(&self, task_id: &str) -> TaskRuntime {
        self.runtime
            .lock()
            .expect("runtime lock poisoned")
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn active_count(&self) -> usize {
        self.reap_finished();
        self.workers.lock().expect("worker lock poisoned").len()
    }

    pub fn shutdown(&self) {
        self.stop_all();
    }

    fn set_runtime(&self, task_id: &str, state: TaskRuntime) {
        self.runtime
            .lock()
            .expect("runtime lock poisoned")
            .insert(task_id.to_string(), state);
    }

    fn reap_finished(&self) {
        let mut workers = self.workers.lock().expect("worker lock poisoned");
        let finished = workers
            .iter()
            .filter(|(_, worker)| worker.thread.is_finished())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in finished {
            if let Some(worker) = workers.remove(&id) {
                let _ = worker.thread.join();
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

trait TaskInput {
    fn position(&mut self) -> Result<(i32, i32), String>;
    fn sample(&mut self, x: i32, y: i32) -> Result<RgbColor, String>;
    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String>;
    fn click(&mut self, button: MouseButton) -> Result<(), String>;
}

impl TaskInput for Enigo {
    fn position(&mut self) -> Result<(i32, i32), String> {
        platform::mouse_position(self)
    }
    fn sample(&mut self, x: i32, y: i32) -> Result<RgbColor, String> {
        platform::sample_pixel(x, y)
    }
    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
        platform::move_mouse(self, x, y)
    }
    fn click(&mut self, button: MouseButton) -> Result<(), String> {
        self.button(to_enigo_button(button), Direction::Click)
            .map_err(|error| format!("鼠标点击失败：{error}"))
    }
}

fn run_task(task: Task, stop: &AtomicBool, runtime: &Mutex<HashMap<String, TaskRuntime>>) {
    if stop.load(Ordering::Acquire) {
        finish(runtime, &task.id, "已停止".into());
        return;
    }
    match platform::connect_input() {
        Ok(mut input) => execute_task(&task, stop, runtime, &mut input),
        Err(error) => finish(runtime, &task.id, error),
    }
}

enum Iteration {
    Clicked,
    Skipped,
    Stopped,
}

fn perform_iteration(
    task: &Task,
    stop: &AtomicBool,
    input: &mut impl TaskInput,
) -> Result<Iteration, String> {
    let _input_guard = INPUT_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    if stop.load(Ordering::Acquire) {
        return Ok(Iteration::Stopped);
    }
    // Following clicks without a color condition need no cursor-position support.
    if let Some(expected) = task.color {
        let point = match task.target() {
            Some(point) => point,
            None => input.position()?,
        };
        if !expected.matches(input.sample(point.0, point.1)?, task.color_tolerance) {
            return Ok(Iteration::Skipped);
        }
    }
    if stop.load(Ordering::Acquire) {
        return Ok(Iteration::Stopped);
    }
    if let Some((x, y)) = task.target() {
        input.move_to(x, y)?;
    }
    if stop.load(Ordering::Acquire) {
        return Ok(Iteration::Stopped);
    }
    input.click(task.mouse_button)?;
    Ok(Iteration::Clicked)
}

fn execute_task(
    task: &Task,
    stop: &AtomicBool,
    runtime: &Mutex<HashMap<String, TaskRuntime>>,
    input: &mut impl TaskInput,
) {
    let mut rng = thread_rng();
    let outcome = loop {
        match perform_iteration(task, stop, input) {
            Ok(Iteration::Stopped) => break "已停止".into(),
            Ok(Iteration::Skipped) => {
                update_runtime(runtime, &task.id, |state| {
                    state.skipped += 1;
                    state.message = "等待颜色匹配".into();
                });
            }
            Ok(Iteration::Clicked) => {
                let reached_limit = update_runtime(runtime, &task.id, |state| {
                    state.clicks += 1;
                    state.message = "运行中".into();
                    task.max_clicks > 0 && state.clicks >= task.max_clicks
                });
                if reached_limit {
                    break "已完成".into();
                }
            }
            Err(error) => break error,
        }
        wait_or_stop(stop, task.random_interval_ms(&mut rng));
    };
    finish(runtime, &task.id, outcome);
}

fn finish(runtime: &Mutex<HashMap<String, TaskRuntime>>, task_id: &str, outcome: String) {
    update_runtime(runtime, task_id, |state| {
        state.running = false;
        state.message = outcome;
    });
}

fn wait_or_stop(stop: &AtomicBool, milliseconds: u32) {
    let deadline = Instant::now() + Duration::from_millis(milliseconds as u64);
    while !stop.load(Ordering::Acquire) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        // park_timeout may wake spuriously or consume an earlier unpark token.
        thread::park_timeout(remaining);
    }
}

fn update_runtime<R>(
    runtime: &Mutex<HashMap<String, TaskRuntime>>,
    task_id: &str,
    update: impl FnOnce(&mut TaskRuntime) -> R,
) -> R {
    let mut runtime = runtime.lock().expect("runtime lock poisoned");
    update(runtime.entry(task_id.to_string()).or_default())
}

fn to_enigo_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[derive(Default)]
    struct FakeInput {
        clicks: usize,
        moves: Vec<(i32, i32)>,
        stop_on_move: Option<Arc<AtomicBool>>,
        stop_on_sample: Option<Arc<AtomicBool>>,
        fail_click: bool,
    }

    impl TaskInput for FakeInput {
        fn position(&mut self) -> Result<(i32, i32), String> {
            Err("当前输入后端不支持读取鼠标坐标".into())
        }
        fn sample(&mut self, _: i32, _: i32) -> Result<RgbColor, String> {
            if let Some(stop) = &self.stop_on_sample {
                stop.store(true, Ordering::Release);
            }
            Ok(RgbColor {
                red: 0,
                green: 0,
                blue: 0,
            })
        }
        fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
            self.moves.push((x, y));
            if let Some(stop) = &self.stop_on_move {
                stop.store(true, Ordering::Release);
            }
            Ok(())
        }
        fn click(&mut self, _: MouseButton) -> Result<(), String> {
            if self.fail_click {
                return Err("鼠标点击失败：测试错误".into());
            }
            self.clicks += 1;
            Ok(())
        }
    }

    #[test]
    fn following_click_does_not_require_cursor_position_and_respects_limit() {
        let mut task = Task::new(1);
        task.min_interval_ms = 20;
        task.max_clicks = 2;
        let runtime = Mutex::new(HashMap::new());
        let mut input = FakeInput::default();
        execute_task(&task, &AtomicBool::new(false), &runtime, &mut input);
        let state = runtime.lock().unwrap().get(&task.id).unwrap().clone();
        assert_eq!(input.clicks, 2);
        assert_eq!(state.clicks, 2);
        assert!(!state.running);
        assert_eq!(state.message, "已完成");
    }

    #[test]
    fn stopping_during_positioning_prevents_the_click() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut task = Task::new(1);
        task.x = Some(-500);
        task.y = Some(200);
        let mut input = FakeInput {
            stop_on_move: Some(Arc::clone(&stop)),
            ..Default::default()
        };
        assert!(matches!(
            perform_iteration(&task, &stop, &mut input),
            Ok(Iteration::Stopped)
        ));
        assert_eq!(input.moves, vec![(-500, 200)]);
        assert_eq!(input.clicks, 0);
    }

    #[test]
    fn color_mismatch_skips_without_moving_or_clicking() {
        let stop = Arc::new(AtomicBool::new(false));
        let mut task = Task::new(1);
        task.x = Some(5);
        task.y = Some(8);
        task.color = Some(RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        });
        let runtime = Mutex::new(HashMap::new());
        let mut input = FakeInput {
            stop_on_sample: Some(Arc::clone(&stop)),
            ..Default::default()
        };
        execute_task(&task, &stop, &runtime, &mut input);
        assert_eq!(input.clicks, 0);
        assert!(input.moves.is_empty());
        assert_eq!(runtime.lock().unwrap().get(&task.id).unwrap().skipped, 1);
    }

    #[test]
    fn native_failure_is_retained_in_finished_runtime() {
        let task = Task::new(1);
        let runtime = Mutex::new(HashMap::new());
        let mut input = FakeInput {
            fail_click: true,
            ..Default::default()
        };
        execute_task(&task, &AtomicBool::new(false), &runtime, &mut input);
        let runtime = runtime.lock().unwrap();
        let state = runtime.get(&task.id).unwrap();
        assert!(!state.running);
        assert_eq!(state.clicks, 0);
        assert_eq!(state.message, "鼠标点击失败：测试错误");
    }

    #[test]
    fn early_unpark_does_not_shorten_an_interval() {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            thread::current().unpark();
            let started = Instant::now();
            wait_or_stop(&AtomicBool::new(false), 50);
            sender.send(started.elapsed()).unwrap();
        });
        assert!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap() >= Duration::from_millis(50)
        );
        worker.join().unwrap();
    }

    #[test]
    fn stop_and_forget_join_workers_and_release_runtime() {
        let engine = Engine::default();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            wait_or_stop(&worker_stop, 86_400_000);
            sender.send(()).unwrap();
        });
        engine.workers.lock().unwrap().insert(
            "test".into(),
            Worker {
                stop,
                thread: worker,
            },
        );
        engine.set_runtime(
            "test",
            TaskRuntime {
                running: true,
                ..Default::default()
            },
        );
        engine.forget("test");
        assert!(receiver.try_recv().is_ok());
        assert_eq!(engine.active_count(), 0);
        assert!(!engine.runtime.lock().unwrap().contains_key("test"));
    }
}
