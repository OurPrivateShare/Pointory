use crate::{
    model::{MouseButton, Task},
    platform,
};
use enigo::{Button, Coordinate, Direction, Mouse};
use rand::thread_rng;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

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
        if workers
            .get(&task.id)
            .is_some_and(|worker| !worker.thread.is_finished())
        {
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
                clicks: 0,
                skipped: 0,
                message: "正在启动".into(),
            },
        );
        let thread = match thread::Builder::new()
            .name(format!("pointory-{}", task.id))
            .spawn(move || run_task(task, worker_stop, runtime))
        {
            Ok(thread) => thread,
            Err(error) => {
                self.set_runtime(
                    &task_id,
                    TaskRuntime {
                        running: false,
                        clicks: 0,
                        skipped: 0,
                        message: format!("无法启动任务线程：{error}"),
                    },
                );
                return Err(format!("无法启动任务线程：{error}"));
            }
        };
        workers.insert(task_id, Worker { stop, thread });
        Ok(())
    }

    pub fn stop(&self, task_id: &str) {
        if let Some(worker) = self
            .workers
            .lock()
            .expect("worker lock poisoned")
            .get(task_id)
        {
            worker.stop.store(true, Ordering::Release);
            worker.thread.thread().unpark();
        }
    }

    pub fn stop_all(&self) {
        let workers = self.workers.lock().expect("worker lock poisoned");
        for worker in workers.values() {
            worker.stop.store(true, Ordering::Release);
            worker.thread.thread().unpark();
        }
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
        self.workers
            .lock()
            .expect("worker lock poisoned")
            .values()
            .filter(|worker| !worker.thread.is_finished())
            .count()
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

fn run_task(task: Task, stop: Arc<AtomicBool>, runtime: Arc<Mutex<HashMap<String, TaskRuntime>>>) {
    let task_id = task.id.clone();
    let mut enigo = match platform::connect_input() {
        Ok(enigo) => enigo,
        Err(error) => {
            update_runtime(&runtime, &task_id, |state| {
                state.running = false;
                state.message = error;
            });
            return;
        }
    };
    let mut rng = thread_rng();
    let mut outcome = "已停止".to_string();

    while !stop.load(Ordering::Acquire) {
        let point = match task.target() {
            Some(point) => point,
            None => match platform::mouse_position(&mut enigo) {
                Ok(point) => point,
                Err(error) => {
                    outcome = error;
                    break;
                }
            },
        };

        if let Some(expected) = task.color {
            match platform::sample_pixel(point.0, point.1) {
                Ok(actual) if expected.matches(actual, task.color_tolerance) => {}
                Ok(_) => {
                    update_runtime(&runtime, &task_id, |state| {
                        state.skipped += 1;
                        state.message = "等待颜色匹配".into();
                    });
                    wait_or_stop(&stop, task.random_interval_ms(&mut rng));
                    continue;
                }
                Err(error) => {
                    outcome = error;
                    break;
                }
            }
        }

        let click_result = if task.target().is_some() {
            enigo
                .move_mouse(point.0, point.1, Coordinate::Abs)
                .and_then(|()| enigo.button(to_enigo_button(task.mouse_button), Direction::Click))
        } else {
            enigo.button(to_enigo_button(task.mouse_button), Direction::Click)
        };

        if let Err(error) = click_result {
            outcome = format!("鼠标点击失败：{error}");
            break;
        }

        let reached_limit = update_runtime(&runtime, &task_id, |state| {
            state.clicks += 1;
            state.message = "运行中".into();
            task.max_clicks > 0 && state.clicks >= task.max_clicks
        });
        if reached_limit {
            outcome = "已完成".into();
            break;
        }

        wait_or_stop(&stop, task.random_interval_ms(&mut rng));
    }

    update_runtime(&runtime, &task_id, |state| {
        state.running = false;
        if !state.message.starts_with("鼠标点击失败") && !state.message.starts_with("读取")
        {
            state.message = outcome;
        }
    });
}

fn wait_or_stop(stop: &AtomicBool, milliseconds: u32) {
    if !stop.load(Ordering::Acquire) {
        thread::park_timeout(Duration::from_millis(milliseconds as u64));
    }
}

fn update_runtime<R>(
    runtime: &Mutex<HashMap<String, TaskRuntime>>,
    task_id: &str,
    update: impl FnOnce(&mut TaskRuntime) -> R,
) -> R {
    let mut runtime = runtime.lock().expect("runtime lock poisoned");
    update(update_or_insert(&mut runtime, task_id))
}

fn update_or_insert<'a>(
    runtime: &'a mut HashMap<String, TaskRuntime>,
    task_id: &str,
) -> &'a mut TaskRuntime {
    runtime.entry(task_id.to_string()).or_default()
}

fn to_enigo_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}
