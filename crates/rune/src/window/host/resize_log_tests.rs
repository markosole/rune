use super::resize_log::{Drawable, MAX_RECORDS, Trace};

#[test]
fn generations_supersede_unpresented_work_and_drain_in_order() {
    let mut trace = Trace::default();
    let first = trace.begin(true, [1600, 900], [1600, 900], 1.5, 10).unwrap();
    trace.configured(
        Some(first),
        Drawable {
            size: [1600, 900],
            surface_generation: 7,
            reconfigured: true,
        },
        11,
    );
    trace.viewport(Some(first), [1066.667, 600.0], 1.5, 12);
    let second = trace.begin(true, [960, 640], [960, 640], 1.5, 20).unwrap();
    trace.configured(
        Some(second),
        Drawable {
            size: [960, 640],
            surface_generation: 8,
            reconfigured: true,
        },
        21,
    );
    trace.viewport(Some(second), [640.0, 426.667], 1.5, 22);
    trace.root(Some(second), Some([640.0, 426.667]), 23);
    trace.render_started(Some(second), [960, 640], 24);
    trace.render_finished(Some(second), [960, 640], 25);
    trace.present_submitted(
        Some(second),
        Drawable {
            size: [960, 640],
            surface_generation: 8,
            reconfigured: false,
        },
        26,
    );
    trace.present_completed(
        Some(second),
        Drawable {
            size: [960, 640],
            surface_generation: 8,
            reconfigured: false,
        },
        27,
    );

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(trace.current(), None);
    let batch = trace.drain_through(Some(second)).unwrap();
    assert_eq!(batch.dropped, 0);
    assert_eq!(batch.records.len(), 2);
    assert_eq!(batch.records[0].superseded.unwrap().value, second);
    assert_eq!(batch.records[1].present_submitted.unwrap().value.size, [960, 640]);
    assert_eq!(batch.records[1].present_completed.unwrap().value.size, [960, 640]);
}

#[test]
fn resize_records_are_hard_bounded_and_report_drops() {
    let mut trace = Trace::default();
    let mut latest = 0;
    for i in 0..MAX_RECORDS + 5 {
        latest = trace
            .begin(
                true,
                [800 + i as u32, 560],
                [800 + i as u32, 560],
                1.0,
                i as u64,
            )
            .unwrap();
    }
    assert_eq!(trace.records().len(), MAX_RECORDS);
    trace.present_submitted(
        Some(latest),
        Drawable {
            size: [864, 560],
            surface_generation: 99,
            reconfigured: false,
        },
        100,
    );
    let batch = trace.drain_through(Some(latest)).unwrap();
    assert_eq!(batch.dropped, 5);
    assert_eq!(batch.records.len(), MAX_RECORDS);
    assert_eq!(batch.records.last().unwrap().generation, latest);
}

#[test]
fn disabled_trace_keeps_no_generation_or_storage() {
    let mut trace = Trace::default();
    assert_eq!(trace.begin(false, [800, 560], [800, 560], 1.0, 1), None);
    assert_eq!(trace.current(), None);
    assert!(trace.records().is_empty());
}

#[test]
fn unavailable_gpu_event_does_not_extend_an_enabled_trace() {
    let mut trace = Trace::default();
    let generation = trace.begin(true, [1600, 900], [1600, 900], 1.0, 1).unwrap();
    assert_eq!(trace.begin(false, [1200, 800], [1200, 800], 1.0, 2), None);
    assert_eq!(trace.current(), Some(generation));
    assert_eq!(trace.records().len(), 1);
    assert_eq!(trace.records()[0].client.value.size, [1600, 900]);
}

#[test]
fn json_keeps_every_boundary_and_timestamp() {
    let mut trace = Trace::default();
    let generation = trace.begin(true, [1200, 800], [1198, 798], 2.0, 1).unwrap();
    trace.configured(
        Some(generation),
        Drawable {
            size: [1200, 800],
            surface_generation: 4,
            reconfigured: true,
        },
        2,
    );
    trace.viewport(Some(generation), [600.0, 400.0], 2.0, 3);
    trace.root(Some(generation), Some([600.0, 400.0]), 4);
    trace.render_started(Some(generation), [1200, 800], 5);
    trace.render_finished(Some(generation), [1200, 800], 6);
    trace.present_submitted(
        Some(generation),
        Drawable {
            size: [1200, 800],
            surface_generation: 4,
            reconfigured: false,
        },
        7,
    );
    trace.present_completed(
        Some(generation),
        Drawable {
            size: [1200, 800],
            surface_generation: 4,
            reconfigured: false,
        },
        8,
    );
    let json = trace.drain_through(Some(generation)).unwrap().json();
    let record = &json["records"][0];
    assert_eq!(record["client"]["at"], 1);
    assert_eq!(record["client"]["value"]["event_physical"], serde_json::json!([1200, 800]));
    assert_eq!(record["client"]["value"]["physical"], serde_json::json!([1198, 798]));
    assert_eq!(record["drawable"]["at"], 2);
    assert_eq!(record["viewport"]["at"], 3);
    assert_eq!(record["root"]["at"], 4);
    assert_eq!(record["render_started"]["at"], 5);
    assert_eq!(record["render_finished"]["at"], 6);
    assert_eq!(record["present_submitted"]["at"], 7);
    assert_eq!(record["present_completed"]["at"], 8);
}

#[test]
fn final_generation_without_present_has_a_pending_snapshot() {
    let mut trace = Trace::default();
    let generation = trace.begin(true, [1280, 720], [1280, 720], 1.0, 10).unwrap();
    trace.configured(
        Some(generation),
        Drawable {
            size: [1280, 720],
            surface_generation: 11,
            reconfigured: false,
        },
        11,
    );
    trace.viewport(Some(generation), [1280.0, 720.0], 1.0, 12);

    let pending = trace.pending_current().unwrap();
    assert_eq!(pending["generation"], generation);
    assert_eq!(pending["drawable"]["value"]["reconfigured"], false);
    assert!(pending["present_submitted"].is_null());
    assert_eq!(trace.current(), Some(generation));
}
