//! Dragging a window between displays with different scaling. Windows sends `WM_DPICHANGED`
//! with a rect it worked out for the new scale, and expects the window to take it. winit sizes
//! the window itself, keeps the pointer's place along the title bar, then nudges the window
//! until most of it is on the new display. Mid-drag that nudge moves the window out from
//! under the pointer; the move loop puts it back under the pointer, mostly on the old display,
//! which changes the scale back, and the window grows and shrinks at the boundary instead of
//! crossing it (hex#1355). While the user moves or sizes the window, it takes Windows' rect
//! after winit has handled the message, so winit still learns the new scale. A window with a
//! custom frame (`frame`) lands on Windows' rect in winit's own move, so it needs no second.
//!
//! The move or size loop is also when a resize draws its frame at once (`in_size_move`).

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsWindow, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_NCDESTROY,
};

const SUBCLASS: usize = 0x5255_4e44;

thread_local! {
    /// The windows on this thread inside a move or size loop.
    static MOVING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Makes `window` take the rect Windows suggests when its scale changes during a move or size.
pub(crate) fn follow_suggested_rect(window: &winit::window::Window) {
    let Some(hwnd) = super::hwnd(window) else { return };
    // SAFETY: the subclass reads no data, and goes at WM_NCDESTROY.
    unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS, 0); }
}

/// Whether the user is moving or sizing `window` by its frame (`WM_ENTERSIZEMOVE` until
/// `WM_EXITSIZEMOVE`), in the system's modal loop.
pub(crate) fn in_size_move(window: &winit::window::Window) -> bool {
    super::hwnd(window).is_some_and(moving)
}

fn moving(hwnd: HWND) -> bool {
    MOVING.with(|m| m.borrow().contains(&(hwnd as usize)))
}

/// Finishes the last composition before Windows starts changing the client rectangle. The first
/// live-resize `Surface::configure` otherwise has to drain a swap chain whose last buffer can
/// still belong to DWM; on a busy DirectComposition window that wait can hold the window thread
/// for hundreds of milliseconds while the native sizing loop keeps exposing new client area.
fn enter_size_move(hwnd: HWND) {
    enter_size_move_with(hwnd as usize, || {
        // SAFETY: DwmFlush has no pointer arguments or ownership transfer. WM_ENTERSIZEMOVE runs
        // on the window thread before the modal sizing loop can change the client rectangle.
        unsafe { DwmFlush() }
    });
}

fn enter_size_move_with(hwnd: usize, flush: impl FnOnce() -> i32) -> bool {
    MOVING.with(|m| {
        let mut m = m.borrow_mut();
        if !m.contains(&hwnd) { m.push(hwnd); }
    });
    flush() >= 0
}

fn exit_size_move(hwnd: HWND) {
    MOVING.with(|m| m.borrow_mut().retain(|&h| h != hwnd as usize));
}

unsafe extern "system" fn subclass(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match message {
        WM_ENTERSIZEMOVE => enter_size_move(hwnd),
        WM_EXITSIZEMOVE => exit_size_move(hwnd),
        // SAFETY: WM_DPICHANGED's lparam points to the suggested RECT.
        WM_DPICHANGED if moving(hwnd) && !super::frame::installed(hwnd) => unsafe {
            let suggested = *(lparam as *const RECT);
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if IsWindow(hwnd) != 0 {
                let RECT { left, top, right, bottom } = suggested;
                SetWindowPos(hwnd, std::ptr::null_mut(), left, top, right - left, bottom - top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            return result;
        },
        WM_NCDESTROY => {
            MOVING.with(|m| m.borrow_mut().retain(|&h| h != hwnd as usize));
            // SAFETY: removes this subclass from the window it's on.
            unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS); }
        }
        _ => {}
    }
    // SAFETY: forwards to the next window procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn entering_tracks_window_before_flushing_and_exit_forgets_it() {
        let hwnd = 0x5255_4e44usize;
        let flushed_while_moving = Cell::new(false);
        assert!(enter_size_move_with(hwnd, || {
            flushed_while_moving.set(MOVING.with(|m| m.borrow().contains(&hwnd)));
            0
        }));
        assert!(flushed_while_moving.get());
        assert!(MOVING.with(|m| m.borrow().contains(&hwnd)));
        exit_size_move(hwnd as HWND);
        assert!(!MOVING.with(|m| m.borrow().contains(&hwnd)));
    }

    #[test]
    fn entering_keeps_tracking_when_dwm_flush_fails() {
        let hwnd = 0x5255_4e45usize;
        assert!(!enter_size_move_with(hwnd, || -1));
        assert!(MOVING.with(|m| m.borrow().contains(&hwnd)));
        exit_size_move(hwnd as HWND);
    }
}
