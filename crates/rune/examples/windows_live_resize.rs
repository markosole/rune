//! Windows-only native live-resize reproducer.
//!
//! After its first frame it presses the real bottom window edge, walks it through a bounded
//! sequence of physical positions in one sizing gesture, and releases it. Run with
//! `RUNE_FRAME_LOG=... cargo run -p rune --release --features frame-profile
//! --example windows_live_resize`.

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use std::{
        collections::BTreeSet,
        panic::{AssertUnwindSafe, catch_unwind},
        time::Duration,
    };

    use rune::*;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::System::Threading::{
        AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, VK_LBUTTON, mouse_event,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetClientRect, GetForegroundWindow, GetWindowRect,
        GetWindowThreadProcessId, IsWindowVisible, SendMessageTimeoutW, SetCursorPos,
        SetForegroundWindow, HTBOTTOM, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NCLBUTTONDOWN,
    };
    use windows_sys::core::BOOL;

    const STEP: Duration = Duration::from_millis(24);
    const BUTTON_WAIT: Duration = Duration::from_millis(250);
    const MAX_ATTEMPTS: usize = 3;

    #[derive(Default)]
    struct State;

    #[derive(Default)]
    struct FindWindow {
        pid: u32,
        found: HWND,
    }

    unsafe extern "system" fn find_window(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `find_process_window` owns this value for the duration of `EnumWindows`.
        let find = unsafe { &mut *(data as *mut FindWindow) };
        let mut pid = 0;
        // SAFETY: `hwnd` came from EnumWindows and `pid` is writable for this call.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        // SAFETY: `hwnd` came from EnumWindows and remains valid during the callback.
        let visible = unsafe { IsWindowVisible(hwnd) } != 0;
        if pid == find.pid && visible {
            find.found = hwnd;
            return 0;
        }
        1
    }

    fn find_process_window() -> Result<HWND, String> {
        let mut find = FindWindow {
            // SAFETY: process IDs require no additional lifetime or ownership.
            pid: unsafe { GetCurrentProcessId() },
            ..Default::default()
        };
        // SAFETY: callback and context remain valid until this synchronous enumeration returns.
        unsafe { EnumWindows(Some(find_window), &mut find as *mut FindWindow as LPARAM) };
        if find.found.is_null() {
            Err("could not find the Rune fixture window".into())
        } else {
            Ok(find.found)
        }
    }

    fn rect(hwnd: HWND, client: bool) -> Result<RECT, String> {
        let mut rect = RECT::default();
        // SAFETY: the window remains alive while the application handle keeps the example open.
        let ok = unsafe {
            if client {
                GetClientRect(hwnd, &mut rect)
            } else {
                GetWindowRect(hwnd, &mut rect)
            }
        };
        if ok == 0 {
            Err(format!(
                "could not read native {} geometry",
                if client { "client" } else { "window" },
            ))
        } else {
            Ok(rect)
        }
    }

    fn client_size(hwnd: HWND) -> Result<(i32, i32), String> {
        let rect = rect(hwnd, true)?;
        Ok((rect.right - rect.left, rect.bottom - rect.top))
    }

    fn checked(ok: BOOL, operation: &str) -> Result<(), String> {
        if ok == 0 { Err(format!("{operation} failed")) } else { Ok(()) }
    }

    fn left_button_down() -> bool {
        // SAFETY: reading a virtual-key state has no pointer, ownership, or lifetime requirements.
        unsafe { GetAsyncKeyState(VK_LBUTTON as i32) < 0 }
    }

    fn set_left_button(down: bool) -> Result<(), String> {
        // SAFETY: the fixture injects one bounded left-button transition and verifies its state.
        unsafe {
            mouse_event(
                if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP },
                0,
                0,
                0,
                0,
            );
        }
        let due = rune::time::now() + BUTTON_WAIT;
        while rune::time::now() < due {
            if left_button_down() == down { return Ok(()); }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(format!(
            "left button did not reach the verified {} state",
            if down { "down" } else { "up" },
        ))
    }

    fn focus_window(hwnd: HWND) -> Result<(), String> {
        // Foreground-lock rules permit a thread attached to the current foreground input queue to
        // activate the fixture. Detach before injecting any mouse input.
        // SAFETY: this reads the process-wide foreground window without taking ownership.
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_null() { return Err("GetForegroundWindow returned no window".into()); }
        // SAFETY: `foreground` was returned by the system and the null process-ID pointer is valid.
        let foreground_thread = unsafe { GetWindowThreadProcessId(foreground, std::ptr::null_mut()) };
        if foreground_thread == 0 { return Err("could not identify the foreground thread".into()); }
        // SAFETY: reading the caller's thread ID has no ownership or lifetime requirements.
        let worker_thread = unsafe { GetCurrentThreadId() };
        let attached = worker_thread != foreground_thread;
        if attached {
            // SAFETY: both IDs name live threads with input queues for this bounded fixture.
            let result = unsafe { AttachThreadInput(worker_thread, foreground_thread, 1) };
            checked(result, "AttachThreadInput(attach)")?;
        }
        // SAFETY: `hwnd` remains owned by the live Rune application during this worker call.
        let brought = unsafe { BringWindowToTop(hwnd) };
        // SAFETY: `hwnd` remains owned by the live Rune application during this worker call.
        let focused = unsafe { SetForegroundWindow(hwnd) };
        if attached {
            // SAFETY: detaches the exact pair successfully attached above.
            let result = unsafe { AttachThreadInput(worker_thread, foreground_thread, 0) };
            checked(result, "AttachThreadInput(detach)")?;
        }
        checked(brought, "BringWindowToTop")?;
        checked(focused, "SetForegroundWindow")
    }

    fn move_sizing_cursor(
        hwnd: usize,
        x: i32,
        bottom: i32,
        initial: (i32, i32),
    ) -> Result<BTreeSet<(i32, i32)>, String> {
        let movement = (|| -> Result<BTreeSet<(i32, i32)>, String> {
            let hwnd = hwnd as HWND;
            std::thread::sleep(Duration::from_millis(80));
            let mut sizes = BTreeSet::from([initial]);
            let offsets = (1..=20)
                .map(|step| -8 * step)
                .chain((0..20).rev().map(|step| -8 * step));
            for offset in offsets {
                // SAFETY: this walks the cursor through the bounded edge-drag sequence.
                let moved = unsafe { SetCursorPos(x, bottom + offset) };
                checked(moved, "SetCursorPos(gesture)")?;
                std::thread::sleep(STEP);
                sizes.insert(client_size(hwnd)?);
            }
            Ok(sizes)
        })();
        let released = set_left_button(false);
        match (movement, released) {
            (Ok(sizes), Ok(())) => Ok(sizes),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(release)) => Err(format!("{error}; release failed: {release}")),
        }
    }

    fn drag_bottom_edge_once(hwnd: HWND, attempt: usize) -> Result<usize, String> {
        let outer = rect(hwnd, false)?;
        let initial = client_size(hwnd)?;
        eprintln!(
            "LIVE_RESIZE_BEGIN attempt={} outer={}x{} client={}x{}",
            attempt,
            outer.right - outer.left,
            outer.bottom - outer.top,
            initial.0,
            initial.1,
        );
        let x = outer.left + (outer.right - outer.left) / 2;
        let bottom = outer.bottom - 5;
        set_left_button(false)?;
        focus_window(hwnd)?;
        // SAFETY: the bounded fixture moves the real cursor to its own live window edge.
        let moved = unsafe { SetCursorPos(x, bottom) };
        checked(moved, "SetCursorPos(edge)")?;
        set_left_button(true)?;
        let hwnd_value = hwnd as usize;
        let mover = std::thread::Builder::new()
            .name("native-resize-mover".into())
            .spawn(move || move_sizing_cursor(hwnd_value, x, bottom, initial));
        let mover = match mover {
            Ok(mover) => mover,
            Err(error) => {
                if let Err(release) = set_left_button(false) {
                    eprintln!("LIVE_RESIZE_NOTE spawn-failure release failed: {release}");
                }
                return Err(format!("could not start native resize mover: {error}"));
            }
        };
        let point = ((bottom as u16 as u32) << 16) | u32::from(x as u16);
        let mut message_result = 0;
        // SAFETY: `hwnd` is live; the bounded timeout prevents a hung modal loop, while the mover
        // thread supplies the matching cursor motion and verified button release.
        let sent = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_NCLBUTTONDOWN,
                HTBOTTOM as usize,
                point as LPARAM,
                SMTO_BLOCK | SMTO_ABORTIFHUNG,
                5000,
                &mut message_result,
            )
        };
        let mut sizes = mover.join().map_err(|_| "native resize mover panicked".to_string())??;
        if sent == 0 { return Err("SendMessageTimeoutW(WM_NCLBUTTONDOWN) failed or timed out".into()); }
        std::thread::sleep(Duration::from_millis(1000));
        let final_size = client_size(hwnd)?;
        sizes.insert(final_size);
        if !sizes.iter().any(|&size| size != initial) {
            return Err("native client geometry never changed during the sizing gesture".into());
        }
        if sizes.len() < 3 {
            return Err(format!(
                "native sizing gesture produced only {} distinct client sizes",
                sizes.len(),
            ));
        }
        eprintln!(
            "LIVE_RESIZE_END client={}x{} distinct_client_sizes={}",
            final_size.0,
            final_size.1,
            sizes.len(),
        );
        Ok(sizes.len())
    }

    fn drag_bottom_edge(hwnd: HWND) -> Result<usize, String> {
        let mut failures = Vec::new();
        for attempt in 1..=MAX_ATTEMPTS {
            match drag_bottom_edge_once(hwnd, attempt) {
                Ok(distinct) => return Ok(distinct),
                Err(error) => {
                    // Normalize input again even if the attempt failed before its ordinary release.
                    let release = set_left_button(false);
                    let detail = match release {
                        Ok(()) => error,
                        Err(release) => format!("{error}; cleanup failed: {release}"),
                    };
                    eprintln!("LIVE_RESIZE_RETRY attempt={attempt} error={detail}");
                    failures.push(detail);
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
        }
        Err(format!(
            "all {MAX_ATTEMPTS} native sizing attempts failed: {}",
            failures.join(" | "),
        ))
    }

    fn bar(label: &str, color: Color) -> El {
        row()
            .w_full()
            .h(52.0)
            .center()
            .fill(color)
            .child(text(label).color(Color::WHITE))
    }

    fn view(_: &State, _: &mut Cx<State>) -> El {
        col()
            .w_full()
            .h_full()
            .fill(oklch(0.18, 0.02, 260.0))
            .child(bar("top edge", oklch(0.5, 0.15, 260.0)))
            .child(el().grow(1.0).center().child(text("one native sizing loop").color(Color::WHITE)))
            .child(bar("bottom edge", oklch(0.55, 0.2, 25.0)))
    }

    pub(super) fn run() {
        let spec = AppSpec {
            min_size: Some((400.0, 320.0)),
            ..AppSpec::new("Rune Windows live resize", (960.0, 720.0), State::default, view)
        };
        let app = App::new(spec);
        let handle = app.handle();
        let mut started = false;
        app.on_ready(move |_| {
            if started { return; }
            started = true;
            let handle = handle.clone();
            let failure_handle = handle.clone();
            let spawned = std::thread::Builder::new().name("native-resize-fixture".into()).spawn(move || {
                // Let the CPU first frame hand over to the instrumented GPU surface.
                std::thread::sleep(Duration::from_millis(2500));
                let result = match catch_unwind(AssertUnwindSafe(|| {
                    find_process_window().and_then(drag_bottom_edge)
                })) {
                    Ok(result) => result,
                    Err(_) => Err("native sizing worker panicked".into()),
                };
                match result {
                    Ok(distinct) => {
                        eprintln!("LIVE_RESIZE_OK distinct_client_sizes={distinct}");
                        handle.quit();
                    }
                    Err(error) => {
                        eprintln!("LIVE_RESIZE_FAILED {error}");
                        handle.quit();
                        std::process::exit(1);
                    }
                }
            });
            if let Err(error) = spawned {
                let message = format!("could not start native sizing worker: {error}");
                eprintln!("LIVE_RESIZE_FAILED {message}");
                failure_handle.quit();
                std::process::exit(1);
            }
        })
        .run();
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_live_resize runs only on Windows");
}
