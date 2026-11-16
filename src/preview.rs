use crate::{
    model::RgbColor,
    platform::{self, PixelPreview},
};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(50);

pub enum Update {
    Frame {
        token: u64,
        preview: PixelPreview,
    },
    Color {
        token: u64,
        result: Result<RgbColor, String>,
    },
    Error {
        token: u64,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Stopped,
    Follow,
    Color { x: i32, y: i32 },
}

#[derive(Clone, Copy)]
struct Request {
    generation: u64,
    token: u64,
    mode: Mode,
}

struct State {
    request: Request,
    quit: bool,
    latest: Option<Update>,
}

impl State {
    fn new() -> Self {
        Self {
            request: Request {
                generation: 0,
                token: 0,
                mode: Mode::Stopped,
            },
            quit: false,
            latest: None,
        }
    }

    fn request(&mut self, token: u64, mode: Mode) {
        self.request.generation = self.request.generation.wrapping_add(1);
        self.request.token = token;
        self.request.mode = mode;
        self.latest = None;
    }

    fn publish(&mut self, request: Request, update: Update, stop_after: bool) -> bool {
        if self.quit || self.request.generation != request.generation {
            return false;
        }
        if stop_after {
            self.request.mode = Mode::Stopped;
        }
        // Frames replace the previous unread frame. No frame queue can build up
        // if the UI is slower than the small-region screen capture.
        self.latest = Some(update);
        true
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

pub struct PreviewService {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl PreviewService {
    pub fn new() -> Result<Self, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new()),
            wake: Condvar::new(),
        });
        let worker = thread::Builder::new()
            .name("pointory-preview".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || run(&shared)
            })
            .map_err(|error| format!("无法启动像素预览：{error}"))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub fn start(&self, token: u64) {
        self.command(token, Mode::Follow);
    }

    pub fn stop(&self) {
        let mut state = self.shared.state.lock().unwrap();
        let token = state.request.token;
        state.request(token, Mode::Stopped);
        self.shared.wake.notify_one();
    }

    pub fn sample_color(&self, token: u64, x: i32, y: i32) {
        self.command(token, Mode::Color { x, y });
    }

    pub fn take_update(&self) -> Option<Update> {
        self.shared.state.lock().unwrap().latest.take()
    }

    fn command(&self, token: u64, mode: Mode) {
        self.shared.state.lock().unwrap().request(token, mode);
        self.shared.wake.notify_one();
    }
}

impl Drop for PreviewService {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.quit = true;
            state.latest = None;
        }
        self.shared.wake.notify_one();
        // Screen-capture drivers may block inside a native call. Cancellation
        // invalidates its output; let the Arc-owned worker finish independently
        // rather than freezing the application's exit while waiting for it.
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

fn run(shared: &Shared) {
    // Input connections can be expensive on X11. Reuse one for all preview
    // frames and sessions; color confirmation needs no mouse connection.
    let mut input = None;
    loop {
        let request = {
            let mut state = shared.state.lock().unwrap();
            while !state.quit && state.request.mode == Mode::Stopped {
                state = shared.wake.wait(state).unwrap();
            }
            if state.quit {
                return;
            }
            state.request
        };

        // Native input and capture calls never hold the state mutex, so start,
        // stop, sampling requests and UI polling stay responsive during capture.
        let (update, stop_after) = match request.mode {
            Mode::Follow => {
                let result = (|| {
                    if input.is_none() {
                        input = Some(platform::connect_input()?);
                    }
                    let (x, y) = platform::mouse_position(input.as_mut().unwrap())?;
                    platform::capture_preview(x, y)
                })();
                match result {
                    Ok(preview) => (
                        Update::Frame {
                            token: request.token,
                            preview,
                        },
                        false,
                    ),
                    Err(message) => {
                        input = None;
                        (
                            Update::Error {
                                token: request.token,
                                message,
                            },
                            true,
                        )
                    }
                }
            }
            Mode::Color { x, y } => (
                Update::Color {
                    token: request.token,
                    result: platform::sample_pixel(x, y),
                },
                true,
            ),
            Mode::Stopped => continue,
        };
        let mut state = shared.state.lock().unwrap();
        if !state.publish(request, update, stop_after) {
            continue;
        }
        if !stop_after {
            // A new command interrupts the wait and is processed immediately.
            drop(shared.wake.wait_timeout(state, FRAME_INTERVAL).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(token: u64, x: i32) -> Update {
        Update::Frame {
            token,
            preview: PixelPreview {
                x,
                y: 0,
                color: RgbColor {
                    red: 1,
                    green: 2,
                    blue: 3,
                },
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 255],
            },
        }
    }

    #[test]
    fn new_session_rejects_previous_frames_and_errors() {
        let mut state = State::new();
        state.request(10, Mode::Follow);
        let old = state.request;
        state.request(11, Mode::Follow);
        assert!(!state.publish(old, frame(10, 99), false));
        assert!(!state.publish(
            old,
            Update::Error {
                token: 10,
                message: "old capture failed".into()
            },
            true
        ));
        assert_eq!(state.request.mode, Mode::Follow);
        assert!(state.latest.is_none());
    }

    #[test]
    fn confirmation_is_not_overwritten_by_an_inflight_preview() {
        let mut state = State::new();
        state.request(5, Mode::Follow);
        let preview = state.request;
        state.request(5, Mode::Color { x: -100, y: 20 });
        let confirmation = state.request;
        assert!(!state.publish(preview, frame(5, 900), false));
        assert!(state.publish(
            confirmation,
            Update::Color {
                token: 5,
                result: Ok(RgbColor {
                    red: 9,
                    green: 8,
                    blue: 7
                })
            },
            true
        ));
        assert_eq!(state.request.mode, Mode::Stopped);
        assert!(matches!(state.latest, Some(Update::Color { token: 5, .. })));
    }

    #[test]
    fn unread_frames_are_replaced_and_stop_discards_pending_work() {
        let mut state = State::new();
        state.request(1, Mode::Follow);
        let request = state.request;
        assert!(state.publish(request, frame(1, 100), false));
        assert!(state.publish(request, frame(1, 200), false));
        assert!(
            matches!(state.latest.as_ref(), Some(Update::Frame { preview, .. }) if preview.x == 200)
        );
        state.request(1, Mode::Stopped);
        assert!(state.latest.is_none());
        assert!(!state.publish(request, frame(1, 300), false));
    }

    #[test]
    fn shutdown_does_not_wait_for_a_blocked_capture_driver() {
        use std::sync::mpsc;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new()),
            wake: Condvar::new(),
        });
        let (release, blocked) = mpsc::channel::<()>();
        let worker = thread::spawn(move || {
            let _ = blocked.recv();
        });
        let service = PreviewService {
            shared,
            worker: Some(worker),
        };
        let (done, dropped) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(service);
            let _ = done.send(());
        });
        let completed = dropped.recv_timeout(Duration::from_secs(2));
        let _ = release.send(());
        dropper.join().unwrap();
        assert!(
            completed.is_ok(),
            "application exit waited for native screen capture"
        );
    }
}
