//! A temporary, non-blocking desktop selection session. The confirmation click is
//! consumed so it cannot activate the application underneath the sampled pixel.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug)]
pub enum Event {
    Selected(Selection),
    Cancelled,
    Error(String),
}

pub struct PickerSession {
    cancel: Arc<AtomicBool>,
    events: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
}

pub fn start() -> Result<PickerSession, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let (events_sender, events) = mpsc::channel();
    let (ready_sender, ready) = mpsc::sync_channel(1);
    let worker_cancel = Arc::clone(&cancel);
    let worker = thread::Builder::new()
        .name("pointory-selection".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(|| backend::run(&worker_cancel, ready_sender));
            let event = match result {
                Ok(Ok(Some(point))) => Event::Selected(point),
                Ok(Ok(None)) => Event::Cancelled,
                Ok(Err(error)) => Event::Error(error),
                Err(_) => Event::Error("桌面取样异常，已结束取样。".into()),
            };
            let _ = events_sender.send(event);
        })
        .map_err(|error| format!("无法启动桌面取样：{error}"))?;
    let mut session = PickerSession {
        cancel,
        events,
        worker: Some(worker),
    };
    match ready.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => Ok(session),
        Ok(Err(error)) => {
            session.cancel();
            session.join_finished();
            Err(error)
        }
        Err(error) => {
            session.cancel();
            session.join_finished();
            Err(format!("桌面取样未能及时就绪：{error}"))
        }
    }
}

impl PickerSession {
    pub fn try_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    fn join_finished(&mut self) {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.worker.take().unwrap().join();
        }
    }
}

impl Drop for PickerSession {
    fn drop(&mut self) {
        self.cancel();
        // A disconnected display server must not freeze the UI on shutdown.
        // Workers observe cancellation before installing hooks/grabs and at every
        // message-pump iteration; native guards release them on every exit path.
        self.join_finished();
    }
}

struct ClickGesture {
    armed: bool,
    pending: Option<Selection>,
    selected: Option<Selection>,
}

impl ClickGesture {
    fn new(left_is_down: bool) -> Self {
        Self {
            armed: !left_is_down,
            pending: None,
            selected: None,
        }
    }

    #[cfg(any(windows, test))]
    fn observe_released(&mut self) {
        self.armed = true;
    }

    fn left_down(&mut self, point: Selection) -> bool {
        if !self.armed || self.selected.is_some() {
            return false;
        }
        // Preserve the original press coordinate if an extra down is delivered.
        self.pending.get_or_insert(point);
        true
    }

    fn left_up(&mut self) -> bool {
        self.armed = true;
        if let Some(point) = self.pending.take() {
            self.selected = Some(point);
            true
        } else {
            false
        }
    }
}

#[cfg(any(windows, target_os = "linux", test))]
struct EscapeGesture {
    armed: bool,
    pending: bool,
    cancelled: bool,
}

#[cfg(any(windows, target_os = "linux", test))]
impl EscapeGesture {
    fn new(escape_is_down: bool) -> Self {
        Self {
            armed: !escape_is_down,
            pending: false,
            cancelled: false,
        }
    }
    fn observe_released(&mut self) {
        self.armed = true;
    }
    fn key_down(&mut self) -> bool {
        if !self.armed {
            return false;
        }
        self.pending = true;
        true
    }
    fn key_up(&mut self) -> bool {
        self.armed = true;
        if self.pending {
            self.pending = false;
            self.cancelled = true;
            true
        } else {
            false
        }
    }
}

#[cfg(windows)]
mod backend {
    use super::*;
    use std::{cell::RefCell, ptr, time::Instant};
    use windows_sys::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON},
            WindowsAndMessaging::{
                CallNextHookEx, DispatchMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
                PM_REMOVE, PeekMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
                WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_SYSKEYDOWN, WM_SYSKEYUP,
            },
        },
    };

    struct HookState {
        gesture: ClickGesture,
        escape: EscapeGesture,
        cancel: Arc<AtomicBool>,
    }

    thread_local! {
        static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
    }

    struct Hooks {
        mouse: HHOOK,
        keyboard: HHOOK,
    }

    impl Drop for Hooks {
        fn drop(&mut self) {
            // Low-level callbacks are delivered to this same thread. Removing
            // the hook before dropping its thread-local state avoids stale data.
            unsafe {
                if !self.keyboard.is_null() {
                    UnhookWindowsHookEx(self.keyboard);
                }
                UnhookWindowsHookEx(self.mouse);
            }
            STATE.with(|state| state.borrow_mut().take());
        }
    }

    unsafe extern "system" fn mouse_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code >= 0 {
            let consumed = STATE.with(|state| {
                let mut state = state.borrow_mut();
                let Some(state) = state.as_mut() else {
                    return false;
                };
                match message as u32 {
                    WM_LBUTTONDOWN => {
                        if state.cancel.load(Ordering::Acquire) {
                            return false;
                        }
                        // The OS provides MSLLHOOKSTRUCT for this hook kind.
                        let event = unsafe { &*(data as *const MSLLHOOKSTRUCT) };
                        state.gesture.left_down(Selection {
                            x: event.pt.x,
                            y: event.pt.y,
                        })
                    }
                    WM_LBUTTONUP => state.gesture.left_up(),
                    _ => false,
                }
            });
            if consumed {
                return 1;
            }
        }
        unsafe { CallNextHookEx(ptr::null_mut(), code, message, data) }
    }

    unsafe extern "system" fn keyboard_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code >= 0 {
            let event = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
            if event.vkCode == u32::from(VK_ESCAPE) {
                let consumed = STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let Some(state) = state.as_mut() else {
                        return false;
                    };
                    match message as u32 {
                        WM_KEYDOWN | WM_SYSKEYDOWN => {
                            if state.cancel.load(Ordering::Acquire) && !state.escape.pending {
                                false
                            } else {
                                state.escape.key_down()
                            }
                        }
                        WM_KEYUP | WM_SYSKEYUP => state.escape.key_up(),
                        _ => false,
                    }
                });
                if consumed {
                    return 1;
                }
            }
        }
        unsafe { CallNextHookEx(ptr::null_mut(), code, message, data) }
    }

    pub fn run(
        cancel: &Arc<AtomicBool>,
        ready: SyncSender<Result<(), String>>,
    ) -> Result<Option<Selection>, String> {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let left_is_down = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) } < 0;
        let escape_is_down = unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE)) } < 0;
        STATE.with(|state| {
            *state.borrow_mut() = Some(HookState {
                gesture: ClickGesture::new(left_is_down),
                escape: EscapeGesture::new(escape_is_down),
                cancel: Arc::clone(cancel),
            });
        });
        let handle = unsafe {
            SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(mouse_hook),
                GetModuleHandleW(ptr::null()),
                0,
            )
        };
        if handle.is_null() {
            STATE.with(|state| state.borrow_mut().take());
            let error = format!(
                "无法捕获鼠标左键，请检查桌面访问权限：{}",
                std::io::Error::last_os_error()
            );
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
        let mut hooks = Hooks {
            mouse: handle,
            keyboard: ptr::null_mut(),
        };
        hooks.keyboard = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(ptr::null()),
                0,
            )
        };
        if hooks.keyboard.is_null() {
            let error = format!(
                "无法捕获取样取消键，请检查桌面访问权限：{}",
                std::io::Error::last_os_error()
            );
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let _ = ready.send(Ok(()));
        let mut message = MSG::default();
        let mut cancel_started = None;
        loop {
            // Low-level hooks run while this thread pumps its message queue.
            while unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            let (selected, escape_pending, escaped, owns_release) = STATE.with(|state| {
                let mut state = state.borrow_mut();
                let state = state.as_mut().unwrap();
                if !state.gesture.armed && unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) } >= 0 {
                    state.gesture.observe_released();
                }
                // Only complete an already-held startup key here. Cancellation
                // itself always waits for its swallowed keyboard-hook key-up.
                if !state.escape.armed && unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE)) } >= 0 {
                    state.escape.observe_released();
                }
                (
                    state.gesture.selected,
                    state.escape.pending,
                    state.escape.cancelled,
                    state.gesture.pending.is_some() || state.escape.pending,
                )
            });
            if cancel.load(Ordering::Acquire) {
                let started = cancel_started.get_or_insert_with(Instant::now);
                // External stop/drop finishes owned releases when possible, but
                // cannot leave a background hook alive for an indefinitely held
                // physical key. A swallowed down is never replayed.
                if !owns_release || started.elapsed() >= Duration::from_millis(500) {
                    return Ok(None);
                }
            } else if escaped && !owns_release {
                return Ok(None);
            } else if selected.is_some() && !escape_pending && !escaped {
                // Selection is returned only after swallowing the release, then
                // hooks are removed before the event reaches the UI thread.
                return Ok(selected);
            }
            thread::park_timeout(Duration::from_millis(4));
        }
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use super::*;
    use x11rb::{
        CURRENT_TIME, NONE,
        connection::Connection,
        protocol::{
            Event as XEvent,
            xproto::{ConnectionExt, EventMask, GrabMode, GrabStatus, KeyButMask},
        },
    };

    pub fn run(
        cancel: &Arc<AtomicBool>,
        ready: SyncSender<Result<(), String>>,
    ) -> Result<Option<Selection>, String> {
        let result = initialize_and_select(cancel, &ready);
        if let Err(error) = &result {
            let _ = ready.try_send(Err(error.clone()));
        }
        result
    }

    fn key_is_down(connection: &impl Connection, keycode: u8) -> Result<bool, String> {
        let keymap = connection
            .query_keymap()
            .map_err(|error| format!("无法读取键盘状态：{error}"))?
            .reply()
            .map_err(|error| format!("无法读取键盘状态：{error}"))?;
        Ok(keymap.keys[usize::from(keycode) / 8] & (1 << (keycode % 8)) != 0)
    }

    fn initialize_and_select(
        cancel: &AtomicBool,
        ready: &SyncSender<Result<(), String>>,
    ) -> Result<Option<Selection>, String> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return Err(
                "当前 Wayland 会话不支持全桌面左键取样，请在 X11 会话下使用位置取点和吸取颜色。"
                    .into(),
            );
        }
        let (connection, screen_index) =
            x11rb::connect(None).map_err(|error| format!("无法连接 X11 桌面：{error}"))?;
        let setup = connection.setup();
        let root = setup.roots[screen_index].root;
        let min_keycode = setup.min_keycode;
        let keyboard = connection
            .get_keyboard_mapping(min_keycode, setup.max_keycode - min_keycode + 1)
            .map_err(|error| format!("无法读取键盘映射：{error}"))?
            .reply()
            .map_err(|error| format!("无法读取键盘映射：{error}"))?;
        if keyboard.keysyms_per_keycode == 0 {
            return Err("X11 键盘映射无效。".into());
        }
        let escape_code = keyboard
            .keysyms
            .chunks(usize::from(keyboard.keysyms_per_keycode))
            .position(|keysyms| keysyms.contains(&0xff1b))
            .map(|index| usize::from(min_keycode) + index)
            .and_then(|code| u8::try_from(code).ok())
            .ok_or_else(|| "无法定位 Escape 取消键。".to_string())?;
        // The launching click may still hold an implicit pointer grab in Slint.
        // Wait for its release before requesting an explicit desktop grab.
        for _ in 0..250 {
            if cancel.load(Ordering::Acquire) {
                return Ok(None);
            }
            let pointer = connection
                .query_pointer(root)
                .map_err(|error| format!("无法读取鼠标状态：{error}"))?
                .reply()
                .map_err(|error| format!("无法读取鼠标状态：{error}"))?;
            if !pointer.mask.contains(KeyButMask::BUTTON1) {
                break;
            }
            thread::park_timeout(Duration::from_millis(4));
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let pointer = connection
            .grab_pointer(
                false,
                root,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                NONE,
                NONE,
                CURRENT_TIME,
            )
            .map_err(|error| format!("无法捕获桌面鼠标：{error}"))?
            .reply()
            .map_err(|error| format!("无法捕获桌面鼠标：{error}"))?;
        if pointer.status != GrabStatus::SUCCESS {
            return Err("桌面鼠标正被其他程序占用，请松开鼠标后重试。".into());
        }
        let keyboard = connection
            .grab_keyboard(false, root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(|error| format!("无法捕获取样取消键：{error}"))?
            .reply()
            .map_err(|error| format!("无法捕获取样取消键：{error}"))?;
        if keyboard.status != GrabStatus::SUCCESS {
            return Err("桌面键盘正被其他程序占用，无法启动取样。".into());
        }
        // Dropping the connection releases both grabs, including error paths.
        connection
            .flush()
            .map_err(|error| format!("无法启动桌面取样：{error}"))?;
        let mut escape = EscapeGesture::new(key_is_down(&connection, escape_code)?);
        let _ = ready.send(Ok(()));
        let mut gesture = ClickGesture::new(false);
        let result = loop {
            if cancel.load(Ordering::Acquire) {
                break Ok(None);
            }
            if !escape.armed {
                match key_is_down(&connection, escape_code) {
                    Ok(false) => escape.observe_released(),
                    Ok(true) => {}
                    Err(error) => break Err(error),
                }
            }
            if escape.cancelled && !escape.pending && gesture.pending.is_none() {
                break Ok(None);
            }
            if gesture.selected.is_some() && !escape.pending && !escape.cancelled {
                break Ok(gesture.selected);
            }
            match connection.poll_for_event() {
                Ok(Some(XEvent::ButtonPress(event))) if event.detail == 1 => {
                    gesture.left_down(Selection {
                        x: i32::from(event.root_x),
                        y: i32::from(event.root_y),
                    });
                }
                Ok(Some(XEvent::ButtonRelease(event))) if event.detail == 1 => {
                    gesture.left_up();
                }
                Ok(Some(XEvent::KeyPress(event))) if event.detail == escape_code => {
                    escape.key_down();
                }
                Ok(Some(XEvent::KeyRelease(event))) if event.detail == escape_code => {
                    // Classic X11 auto-repeat emits a release/press pair while
                    // the physical key remains down. Confirm the server's
                    // current keymap before treating that synthetic release as
                    // the end of the swallowed Escape group.
                    match key_is_down(&connection, escape_code) {
                        Ok(false) => {
                            escape.key_up();
                        }
                        Ok(true) => {}
                        Err(error) => break Err(error),
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => thread::park_timeout(Duration::from_millis(4)),
                Err(error) => break Err(format!("桌面取样连接中断：{error}")),
            }
        };
        let _ = connection.ungrab_keyboard(CURRENT_TIME);
        let _ = connection.ungrab_pointer(CURRENT_TIME);
        let _ = connection.flush();
        result
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod backend {
    use super::*;

    pub fn run(
        _cancel: &Arc<AtomicBool>,
        ready: SyncSender<Result<(), String>>,
    ) -> Result<Option<Selection>, String> {
        let error = "当前平台不支持桌面左键取样。".to_string();
        let _ = ready.send(Err(error.clone()));
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launching_press_and_release_do_not_confirm_selection() {
        let mut gesture = ClickGesture::new(true);
        assert!(!gesture.left_down(Selection { x: 5, y: 9 }));
        assert!(!gesture.left_up());
        assert_eq!(gesture.selected, None);
        assert!(gesture.left_down(Selection { x: 10, y: 20 }));
        assert!(gesture.left_up());
        assert_eq!(gesture.selected, Some(Selection { x: 10, y: 20 }));
    }

    #[test]
    fn confirmation_consumes_both_edges_and_waits_for_release() {
        let point = Selection { x: -1200, y: 481 };
        let mut gesture = ClickGesture::new(false);
        assert!(gesture.left_down(point));
        assert_eq!(gesture.selected, None);
        assert!(gesture.left_up());
        assert_eq!(gesture.selected, Some(point));
        assert!(!gesture.left_down(Selection { x: 0, y: 0 }));
    }

    #[test]
    fn repeated_down_does_not_replace_original_sample_position() {
        let mut gesture = ClickGesture::new(false);
        let point = Selection { x: 2, y: 3 };
        assert!(gesture.left_down(point));
        assert!(gesture.left_down(Selection { x: 50, y: 60 }));
        assert!(gesture.left_up());
        assert_eq!(gesture.selected, Some(point));
    }

    #[test]
    fn observing_release_arms_a_session_without_a_matching_hook_event() {
        let mut gesture = ClickGesture::new(true);
        gesture.observe_released();
        assert!(gesture.left_down(Selection { x: 4, y: 7 }));
        assert!(gesture.left_up());
        assert_eq!(gesture.selected, Some(Selection { x: 4, y: 7 }));
    }

    #[test]
    fn escape_cancellation_consumes_repeat_and_waits_for_key_release() {
        let mut gesture = EscapeGesture::new(false);
        assert!(gesture.key_down());
        assert!(gesture.key_down());
        assert!(gesture.pending);
        assert!(!gesture.cancelled);
        assert!(gesture.key_up());
        assert!(!gesture.pending);
        assert!(gesture.cancelled);
    }

    #[test]
    fn already_held_escape_is_ignored_until_its_release() {
        let mut gesture = EscapeGesture::new(true);
        assert!(!gesture.key_down());
        assert!(!gesture.key_up());
        assert!(!gesture.cancelled);
        assert!(gesture.key_down());
        assert!(gesture.key_up());
        assert!(gesture.cancelled);
    }

    #[test]
    fn escape_and_mouse_release_can_both_be_consumed_before_cancelling() {
        let mut mouse = ClickGesture::new(false);
        let mut escape = EscapeGesture::new(false);
        assert!(mouse.left_down(Selection { x: 10, y: 20 }));
        assert!(escape.key_down());
        assert!(escape.key_up());
        assert!(escape.cancelled);
        assert!(mouse.pending.is_some());
        assert!(mouse.left_up());
        assert!(mouse.pending.is_none());
        assert!(escape.cancelled);
    }
}
