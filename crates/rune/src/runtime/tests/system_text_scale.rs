use std::cell::Cell;

use crate::{Cx, El, Env, Runtime, el, memo};

#[derive(Default)]
struct Probe {
    builds: Cell<u32>,
    seen: Cell<f32>,
}

fn view(s: &Probe, cx: &mut Cx<Probe>) -> El {
    let scale = cx.system_text_scale();
    s.seen.set(scale);
    el().child(memo("scaled", 0, || {
        s.builds.set(s.builds.get() + 1);
        el().key("scaled").absolute(0.0, 0.0).size(40.0 * scale, 10.0 * scale)
    }))
}

fn size(rt: &Runtime<Probe>) -> [f32; 2] {
    let rect = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("scaled")).expect("scaled element").rect;
    [rect[2], rect[3]]
}

#[test]
fn system_text_scale_defaults_to_one_and_rebuilds_layout_and_memos_at_two() {
    let mut rt = Runtime::new(Probe::default(), view, (200.0, 100.0), Env::new(true, 2.0));
    assert_eq!(rt.system_text_scale(), 1.0);
    rt.frame(0.0);
    assert_eq!((rt.state.seen.get(), size(&rt)), (1.0, [40.0, 10.0]));
    let first_builds = rt.state.builds.get();

    rt.set_system_text_scale(2.0);
    assert_eq!(rt.system_text_scale(), 2.0);
    rt.frame(0.0);
    rt.settle_all();
    rt.frame(0.0);
    assert_eq!((rt.state.seen.get(), size(&rt)), (2.0, [80.0, 20.0]));
    assert!(rt.state.builds.get() > first_builds, "the unchanged-version memo follows the host setting");

    let builds = rt.state.builds.get();
    rt.set_system_text_scale(2.0);
    rt.frame(0.0);
    assert_eq!(rt.state.builds.get(), builds, "the same normalized value queues no commit");

    rt.set_system_text_scale(f32::NAN);
    assert_eq!(rt.system_text_scale(), 1.0, "invalid host values use the platform-safe default");
    rt.frame(0.0);
    rt.settle_all();
    rt.frame(0.0);
    assert_eq!((rt.state.seen.get(), size(&rt)), (1.0, [40.0, 10.0]));
}
