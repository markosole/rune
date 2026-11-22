//! Makes a submitted live-resize frame visible before the native sizing turn continues.

#![allow(unsafe_code)]

/// Runs any pending surface handover first. A normal frame then stays asynchronous, while a frame
/// for a native sizing turn waits for DWM to consume every DirectX update queued before the call.
pub(in crate::window) fn complete_present_after(
    resizing: bool,
    handover: impl FnOnce() -> bool,
) -> bool {
    complete_present_after_with(resizing, handover, || {
        // SAFETY: DwmFlush has no pointer arguments or ownership transfer and is called on the
        // window thread immediately after its swap-chain present.
        unsafe { windows_sys::Win32::Graphics::Dwm::DwmFlush() }
    })
}

fn complete_present_after_with(
    resizing: bool,
    handover: impl FnOnce() -> bool,
    flush: impl FnOnce() -> i32,
) -> bool {
    if !handover() || !resizing { return false; }
    flush() >= 0
}

#[cfg(test)]
pub(super) fn test_complete_present_after(
    resizing: bool,
    handover: impl FnOnce() -> bool,
    flush: impl FnOnce() -> i32,
) -> bool {
    complete_present_after_with(resizing, handover, flush)
}
