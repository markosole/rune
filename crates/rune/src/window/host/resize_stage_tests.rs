use super::*;

#[test]
fn confirming_a_proposal_requires_matching_client_event_and_dpi() {
    let physical = PhysicalSize::new(960, 800);
    let old = PhysicalSize::new(960, 720);
    let proposed = ResizeSize { physical, scale: 1.25 };
    assert!(proposed.matches(physical, physical, 1.25));
    assert!(!proposed.matches(physical, old, 1.25));
    assert!(!proposed.matches(old, physical, 1.25));
    assert!(!proposed.matches(physical, physical, 1.5));
    assert_eq!(proposed.viewport(), (768.0, 640.0));
}
