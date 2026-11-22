use std::cell::{Cell, RefCell};

use super::live_resize::test_complete_present_after;

#[test]
fn normal_present_never_waits_for_the_compositor() {
    let handover = Cell::new(false);
    let called = Cell::new(false);
    assert!(!test_complete_present_after(
        false,
        || {
            handover.set(true);
            true
        },
        || {
            called.set(true);
            0
        },
    ));
    assert!(handover.get());
    assert!(!called.get());
}

#[test]
fn live_resize_waits_only_after_handover_commit() {
    let order = RefCell::new(Vec::new());
    assert!(test_complete_present_after(
        true,
        || {
            order.borrow_mut().push("handover");
            true
        },
        || {
            order.borrow_mut().push("flush");
            0
        },
    ));
    assert_eq!(&*order.borrow(), &["handover", "flush"]);
}

#[test]
fn failed_handover_never_claims_present_completion() {
    let called = Cell::new(false);
    assert!(!test_complete_present_after(
        true,
        || false,
        || {
            called.set(true);
            0
        },
    ));
    assert!(!called.get());
}

#[test]
fn a_failed_compositor_wait_is_not_reported_complete() {
    assert!(!test_complete_present_after(true, || true, || -1));
}
