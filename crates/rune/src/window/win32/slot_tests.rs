#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows_sys::Win32::System::Threading::{CreateEventW, CreateSemaphoreW, GetCurrentProcess, ReleaseSemaphore, SetEvent};

use super::{Slot, Take};

struct Event(HANDLE);

impl Event {
    fn new(signalled: bool) -> Self {
        // SAFETY: anonymous manual-reset event owned by this guard.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, i32::from(signalled), std::ptr::null()) };
        assert!(!handle.is_null());
        Self(handle)
    }
}

fn take(slot: &mut Slot, generation: u64, object: Option<HANDLE>) -> Take {
    slot.take(generation, object, Some(|| {}))
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: closes the handle created in `Event::new`.
        unsafe { CloseHandle(self.0); }
    }
}

#[test]
fn slot_resets_when_swap_chain_waitable_changes_or_disappears() {
    let old_waitable = Event::new(false);
    let new_waitable = Event::new(true);

    let mut slot = Slot::default();
    assert_eq!(take(&mut slot, 1, Some(old_waitable.0)), Take::Later, "even the first frame waits");
    assert!(slot.waiting());

    // Swap chain was reconfigured or replaced (new waitable object, or surface lost/none).
    assert_eq!(
        take(&mut slot, 1, Some(new_waitable.0)),
        Take::Draw,
        "a new swap chain must not stay blocked on the replaced swap chain's wait"
    );
    assert!(!slot.waiting());

    slot.presented();
    assert_eq!(take(&mut slot, 2, Some(old_waitable.0)), Take::Later);
    assert_eq!(
        take(&mut slot, 2, None),
        Take::Draw,
        "losing the waitable handle must clear the stale wait"
    );
    assert!(!slot.waiting());
}

#[test]
fn slot_resets_when_surface_configure_generation_advances() {
    let waitable = Event::new(false);
    let ready = Event::new(true);
    let mut slot = Slot::default();

    // Waiting on generation 1, then swap chain reconfigured (generation 2) with the same handle.
    assert_eq!(take(&mut slot, 1, Some(waitable.0)), Take::Later);
    assert!(slot.waiting());
    assert_eq!(take(&mut slot, 2, Some(waitable.0)), Take::Later);
    assert!(!slot.came_back(), "an old queued wake cannot release the replacement wait");

    // A ready permit for generation 2 cannot be reused for generation 3.
    assert_eq!(take(&mut slot, 2, Some(ready.0)), Take::Draw);
    assert_eq!(take(&mut slot, 3, Some(waitable.0)), Take::Later);
    slot.reset();
    assert!(!slot.waiting());
    assert_eq!(take(&mut slot, 3, Some(waitable.0)), Take::Later);
}

#[test]
fn slot_draws_immediately_when_signalled_or_after_wait_completes() {
    let signalled = Event::new(true);
    let unsignalled = Event::new(false);
    let mut slot = Slot::default();

    assert_eq!(take(&mut slot, 1, Some(signalled.0)), Take::Draw);

    slot.presented();
    let (send, receive) = std::sync::mpsc::channel();
    assert_eq!(slot.take(1, Some(unsignalled.0), Some(move || { let _ = send.send(()); })), Take::Later);
    // SAFETY: this test owns the event for the duration of the wait.
    assert_ne!(unsafe { SetEvent(unsignalled.0) }, 0);
    receive.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    assert!(slot.came_back());
    assert!(!slot.waiting());
    assert_eq!(take(&mut slot, 1, Some(unsignalled.0)), Take::Draw);
}

#[test]
fn synchronous_resize_claims_each_generation_without_banking_permits() {
    // SAFETY: the test owns an anonymous semaphore with one initial permit, like a DXGI chain.
    let ready = Event(unsafe { CreateSemaphoreW(std::ptr::null(), 1, 64, std::ptr::null()) });
    assert!(!ready.0.is_null());
    let mut slot = Slot::default();
    for generation in 1..=32 {
        slot.reset();
        assert_eq!(slot.take(generation, Some(ready.0), None::<fn()>), Take::Draw);
        slot.presented();
        // SAFETY: models the compositor replacing the permit when the frame is released.
        assert_ne!(unsafe { ReleaseSemaphore(ready.0, 1, std::ptr::null_mut()) }, 0);
    }
    assert_eq!(take(&mut slot, 32, Some(ready.0)), Take::Draw);
    assert_eq!(take(&mut slot, 32, Some(ready.0)), Take::Draw, "an acquire retry reuses the same permit");
    slot.presented();
    assert_eq!(take(&mut slot, 32, Some(ready.0)), Take::Later, "no spare permits may admit a burst");
}

#[test]
fn in_place_reconfigure_keeps_its_wait_across_dxgi_handle_replacement() {
    // SAFETY: both anonymous semaphore handles belong to this test and close on drop.
    let semaphore = Event(unsafe { CreateSemaphoreW(std::ptr::null(), 0, 64, std::ptr::null()) });
    assert!(!semaphore.0.is_null());
    let mut duplicate = Event(std::ptr::null_mut());
    let process = unsafe { GetCurrentProcess() };
    assert_ne!(unsafe {
        DuplicateHandle(process, semaphore.0, process, &mut duplicate.0, 0, 0, DUPLICATE_SAME_ACCESS)
    }, 0);
    let (send, receive) = std::sync::mpsc::channel();
    let mut slot = Slot::default();
    assert_eq!(slot.take(1, Some(semaphore.0), Some(move || { let _ = send.send(()); })), Take::Later);
    slot.reconfigured(true);
    assert_eq!(take(&mut slot, 2, Some(duplicate.0)), Take::Later);
    assert_ne!(unsafe { ReleaseSemaphore(semaphore.0, 1, std::ptr::null_mut()) }, 0);
    receive.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    assert!(slot.came_back());
    assert_eq!(take(&mut slot, 2, Some(duplicate.0)), Take::Draw);
    slot.presented();
    assert_eq!(take(&mut slot, 2, Some(duplicate.0)), Take::Later);
    slot.reconfigured(false);
    assert!(!slot.waiting());
}

#[test]
fn ordinary_draw_drains_surplus_permits() {
    // SAFETY: the test owns this anonymous semaphore.
    let semaphore = Event(unsafe { CreateSemaphoreW(std::ptr::null(), 5, 64, std::ptr::null()) });
    assert!(!semaphore.0.is_null());
    let mut slot = Slot::default();
    assert_eq!(take(&mut slot, 1, Some(semaphore.0)), Take::Draw);
    slot.presented();
    assert_eq!(take(&mut slot, 1, Some(semaphore.0)), Take::Later);
}
