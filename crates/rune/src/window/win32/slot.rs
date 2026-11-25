//! Windows: when a window's or an overlay's swap chain can take its next frame. It holds at most
//! one frame the compositor hasn't shown (`FRAME_LATENCY`), and its waitable object is signalled
//! when it can take another. wgpu doesn't wait on it
//! (`Dx12UseFrameLatencyWaitableObject::DontWait`): a frame that blocked in `acquire` would be
//! built from the input that came before the wait, and the input that came during it would wait a
//! frame more. Instead a surface that wants a frame while the swap chain is full asks a
//! thread-pool wait to wake the loop when it isn't; the loop then handles every input that came
//! meanwhile, and the frame is built from all of it. Claim the first permit too, including after
//! DXGI resizes a reused swap chain. Skipping it can leave extra permits that allow later FIFO
//! frames to pile up. Nothing waits, and no thread runs, while the surface draws nothing.

#![allow(unsafe_code)]

use std::{ffi::c_void, sync::atomic::{AtomicBool, Ordering}};

use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE, WaitForSingleObject,
};

/// How long a registered wait waits before it gives the frame anyway: a swap chain that was
/// made again or lost never signals the object the wait holds.
const GIVE_UP_MS: u32 = 100;
const MAX_DRAIN: usize = 64;

/// One surface's swap chain, as the next frame sees it: an app window's or an overlay's.
#[derive(Default)]
pub(crate) enum Slot {
    /// This frame has not claimed a permit.
    #[default]
    Unclaimed,
    /// Can draw or retry this frame until the host actually presents it.
    Ready { binding: Option<(u64, usize)> },
    /// The wait that asks for the next frame is registered (dropping it unregisters it).
    Waiting { wait: Wait, binding: Option<(u64, usize)> },
}

/// What the surface's next frame does (`Slot::take`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Take {
    /// Draw now.
    Draw,
    /// Don't: the registered wait asks for the frame (`WakeEvent::FrameSlot`).
    Later,
}

impl Slot {
    /// Whether a thread-pool wait is currently registered for swap-chain room.
    pub(crate) fn waiting(&self) -> bool {
        matches!(self, Slot::Waiting { .. })
    }

    /// Takes room on `surface`'s swap chain, using its configure generation and DX12 waitable
    /// handle so any reconfiguration or replacement automatically invalidates stale state.
    pub(crate) fn take_surface(&mut self, surface: &wgpu::Surface, wake: impl Fn() + Send + 'static) -> Take {
        let (generation, waitable) = surface_slot(surface);
        self.take(generation, waitable, Some(wake))
    }

    /// Claim a ready permit for a synchronous frame, such as a size-matched native resize. Those
    /// frames can draw within the current message even if the compositor still owns the permit.
    pub(crate) fn take_without_wait(&mut self, surface: &wgpu::Surface) {
        let (generation, waitable) = surface_slot(surface);
        self.take(generation, waitable, None::<fn()>);
    }

    /// The frame about to draw takes the swap chain's room, or waits for it. `generation` is the
    /// surface's last successful configure number; `waitable` is the swap chain's object; `wake`
    /// asks the loop for the frame from the thread-pool thread. `None` draws synchronously.
    fn take(&mut self, generation: u64, waitable: Option<HANDLE>, wake: Option<impl Fn() + Send + 'static>) -> Take {
        let Some(object) = waitable else { self.reset(); return Take::Draw; };
        let binding = (generation, object as usize);
        self.came_back();
        match self {
            Slot::Ready { binding: old } if old.is_none_or(|old| old == binding) => {
                *old = Some(binding);
                return Take::Draw;
            }
            Slot::Waiting { binding: old, .. } if old.is_none_or(|old| old == binding) && wake.is_some() => {
                *old = Some(binding);
                return Take::Later;
            }
            _ => self.reset(),
        }
        // SAFETY: zero-timeout wait on a live swap-chain handle claims one available permit.
        let claimed = unsafe { WaitForSingleObject(object, 0) } == WAIT_OBJECT_0;
        if let Some(wake) = wake {
            if claimed {
                // Missed or synchronous frames may leave extra counts on a reused semaphore.
                // Drain them only on ordinary draws, and bound the probes for other handle types.
                for _ in 0..MAX_DRAIN {
                    // SAFETY: this is the live swap-chain handle and the wait cannot block.
                    if unsafe { WaitForSingleObject(object, 0) } != WAIT_OBJECT_0 { break; }
                }
            } else if let Some(wait) = Wait::register(object, Box::new(wake)) {
                *self = Slot::Waiting { wait, binding: Some(binding) };
                return Take::Later;
            }
        }
        *self = Slot::Ready { binding: Some(binding) };
        Take::Draw
    }

    /// Use this wait's completion. A queued wake from a replaced swap chain cannot grant room.
    pub(crate) fn came_back(&mut self) -> bool {
        if let Slot::Waiting { wait, binding } = self && wait.completed() {
            *self = Slot::Ready { binding: *binding };
            return true;
        }
        false
    }

    /// A frame was presented on `surface`.
    pub(crate) fn presented(&mut self) { self.reset(); }

    /// A successful in-place resize keeps the kernel semaphore even when DXGI replaces its
    /// handle. Rebind the existing reservation or wait to the new generation on the next draw.
    pub(crate) fn reconfigured(&mut self, ok: bool) {
        if !ok { self.reset(); return; }
        match self {
            Slot::Ready { binding } | Slot::Waiting { binding, .. } => *binding = None,
            Slot::Unclaimed => {}
        }
    }

    /// Discard the old frame's reservation and cancel any outstanding wait.
    pub(crate) fn reset(&mut self) { *self = Slot::Unclaimed; }
}

fn surface_slot(surface: &wgpu::Surface) -> (u64, Option<HANDLE>) {
    let generation = crate::window::surface::configure::generation(surface);
    // SAFETY: the live swap chain's object, used here or duplicated before registering a wait.
    let waitable = unsafe { surface.as_hal::<wgpu::hal::api::Dx12>().and_then(|h| h.waitable_handle()) };
    (generation, waitable.map(|h| h.0).filter(|h| !h.is_null()))
}

/// A one-shot thread-pool wait on a copy of the swap chain's object, so wgpu closing its handle
/// (a reconfigure makes a new one) never leaves the wait on a closed handle.
pub(crate) struct Wait {
    wait: HANDLE,
    object: HANDLE,
    callback: *mut Callback,
}

struct Callback {
    wake: Box<dyn Fn() + Send>,
    done: AtomicBool,
}

impl Wait {
    fn register(object: HANDLE, wake: Box<dyn Fn() + Send>) -> Option<Wait> {
        let mut copy: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a live handle of this process into this process.
        let ok = unsafe { DuplicateHandle(GetCurrentProcess(), object, GetCurrentProcess(), &mut copy, 0, 0, DUPLICATE_SAME_ACCESS) };
        if ok == 0 { return None; }
        let callback = Box::into_raw(Box::new(Callback { wake, done: AtomicBool::new(false) }));
        let mut wait: HANDLE = std::ptr::null_mut();
        // SAFETY: `callback` stays at this address through registration and unregistration.
        let ok = unsafe { RegisterWaitForSingleObject(&mut wait, copy, Some(came), callback.cast(), GIVE_UP_MS, WT_EXECUTEONLYONCE | WT_EXECUTEINWAITTHREAD) };
        if ok == 0 {
            // SAFETY: both are ours and unused.
            unsafe {
                drop(Box::from_raw(callback));
                CloseHandle(copy);
            }
            return None;
        }
        Some(Wait { wait, object: copy, callback })
    }

    fn completed(&self) -> bool {
        // SAFETY: this wait owns the context, and the callback shares only atomic state here.
        unsafe { (*self.callback).done.load(Ordering::Acquire) }
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        // SAFETY: waits for a running callback to finish before dropping its context.
        unsafe {
            UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE);
            drop(Box::from_raw(self.callback));
            CloseHandle(self.object);
        }
    }
}

unsafe extern "system" fn came(context: *mut c_void, _timed_out: bool) {
    // SAFETY: the callback context stays alive until this wait has been unregistered.
    let callback = unsafe { &*context.cast::<Callback>() };
    callback.done.store(true, Ordering::Release);
    (callback.wake)();
}

#[cfg(test)]
#[path = "slot_tests.rs"]
mod tests;
