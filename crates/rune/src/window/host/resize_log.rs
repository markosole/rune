//! Correlated `frame-profile` diagnostics for a native window resize.

use std::collections::VecDeque;

use crate::machine::Effects;

pub(super) const MAX_RECORDS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Timed<T> {
    pub(super) at: u64,
    pub(super) value: T,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Client {
    pub(super) size: [u32; 2],
    pub(super) scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Drawable {
    pub(super) size: [u32; 2],
    pub(super) surface_generation: u64,
    pub(super) reconfigured: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Record {
    pub(super) generation: u64,
    pub(super) client: Timed<Client>,
    pub(super) drawable: Option<Timed<Drawable>>,
    pub(super) viewport: Option<Timed<([f32; 2], f32)>>,
    pub(super) root: Option<Timed<Option<[f32; 2]>>>,
    pub(super) render_started: Option<Timed<[u32; 2]>>,
    pub(super) render_finished: Option<Timed<[u32; 2]>>,
    pub(super) present_submitted: Option<Timed<Drawable>>,
    pub(super) superseded: Option<Timed<u64>>,
}

impl Record {
    fn terminal(&self) -> bool {
        self.present_submitted.is_some() || self.superseded.is_some()
    }

    fn json(&self) -> serde_json::Value {
        let timed =
            |at: u64, value: serde_json::Value| serde_json::json!({ "at": at, "value": value });
        serde_json::json!({
            "generation": self.generation,
            "client": timed(self.client.at, serde_json::json!({
                "physical": self.client.value.size,
                "scale": self.client.value.scale,
            })),
            "drawable": self.drawable.map(|v| timed(v.at, serde_json::json!({
                "physical": v.value.size,
                "surface_generation": v.value.surface_generation,
                "reconfigured": v.value.reconfigured,
            }))),
            "viewport": self.viewport.map(|v| timed(v.at, serde_json::json!({
                "dip": v.value.0,
                "scale": v.value.1,
            }))),
            "root": self.root.map(|v| timed(v.at, serde_json::json!(v.value))),
            "render_started": self.render_started.map(|v| timed(v.at, serde_json::json!(v.value))),
            "render_finished": self.render_finished.map(|v| timed(v.at, serde_json::json!(v.value))),
            "present_submitted": self.present_submitted.map(|v| timed(v.at, serde_json::json!({
                "physical": v.value.size,
                "surface_generation": v.value.surface_generation,
                "reconfigured": v.value.reconfigured,
            }))),
            "superseded": self.superseded.map(|v| timed(v.at, serde_json::json!(v.value))),
        })
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Batch {
    pub(super) dropped: u64,
    pub(super) records: Vec<Record>,
}

impl Batch {
    pub(super) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "dropped": self.dropped,
            "records": self.records.iter().map(Record::json).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Disabled,
    Idle {
        next: u64,
    },
    Tracking {
        next: u64,
        current: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Enable,
    Resize,
    PresentSubmitted(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Began(u64),
    Superseded { generation: u64, by: u64 },
}

fn step(state: &mut State, event: Event) -> Effects<Effect> {
    match (*state, event) {
        (State::Disabled, Event::Enable) => {
            *state = State::Idle { next: 1 };
            Effects::none()
        }
        (State::Disabled, _) => Effects::none(),
        (State::Idle { next }, Event::Enable) | (State::Tracking { next, .. }, Event::Enable) => {
            let _ = next;
            Effects::none()
        }
        (State::Idle { next }, Event::Resize) => {
            *state = State::Tracking {
                next: next.checked_add(1).expect("resize generation overflow"),
                current: next,
            };
            Effects::one(Effect::Began(next))
        }
        (State::Tracking { next, current }, Event::Resize) => {
            *state = State::Tracking {
                next: next.checked_add(1).expect("resize generation overflow"),
                current: next,
            };
            Effects::two(
                Effect::Superseded {
                    generation: current,
                    by: next,
                },
                Effect::Began(next),
            )
        }
        (State::Tracking { next, current }, Event::PresentSubmitted(generation))
            if current == generation =>
        {
            *state = State::Idle { next };
            Effects::none()
        }
        (State::Idle { .. } | State::Tracking { .. }, Event::PresentSubmitted(_)) => Effects::none(),
    }
}

#[derive(Debug, Default)]
pub(super) struct Trace {
    state: State,
    records: VecDeque<Record>,
    dropped: u64,
}

impl Trace {
    pub(super) fn begin(
        &mut self,
        enabled: bool,
        size: [u32; 2],
        scale: f32,
        at: u64,
    ) -> Option<u64> {
        if !enabled { return None; }
        let _ = step(&mut self.state, Event::Enable);
        let mut began = None;
        for effect in step(&mut self.state, Event::Resize) {
            match effect {
                Effect::Began(generation) => {
                    if self.records.len() == MAX_RECORDS {
                        self.records.pop_front();
                        self.dropped = self.dropped.saturating_add(1);
                    }
                    self.records.push_back(Record {
                        generation,
                        client: Timed {
                            at,
                            value: Client { size, scale },
                        },
                        drawable: None,
                        viewport: None,
                        root: None,
                        render_started: None,
                        render_finished: None,
                        present_submitted: None,
                        superseded: None,
                    });
                    began = Some(generation);
                }
                Effect::Superseded { generation, by } => {
                    if let Some(record) = self.record_mut(generation) {
                        record.superseded = Some(Timed { at, value: by });
                    }
                }
            }
        }
        began
    }

    pub(super) fn current(&self) -> Option<u64> {
        match self.state {
            State::Tracking { current, .. } => Some(current),
            State::Disabled | State::Idle { .. } => None,
        }
    }

    pub(super) fn configured(&mut self, generation: Option<u64>, value: Drawable, at: u64) {
        if let Some(record) = generation.and_then(|generation| self.record_mut(generation)) {
            record.drawable = Some(Timed { at, value });
        }
    }

    pub(super) fn viewport(&mut self, generation: Option<u64>, dip: [f32; 2], scale: f32, at: u64) {
        if let Some(record) = generation.and_then(|generation| self.record_mut(generation)) {
            record.viewport = Some(Timed {
                at,
                value: (dip, scale),
            });
        }
    }

    pub(super) fn root(&mut self, generation: Option<u64>, size: Option<[f32; 2]>, at: u64) {
        if let Some(record) = generation.and_then(|generation| self.record_mut(generation)) {
            record.root = Some(Timed { at, value: size });
        }
    }

    pub(super) fn render_started(&mut self, generation: Option<u64>, size: [u32; 2], at: u64) {
        if let Some(record) = generation.and_then(|generation| self.record_mut(generation)) {
            record.render_started = Some(Timed { at, value: size });
        }
    }

    pub(super) fn render_finished(&mut self, generation: Option<u64>, size: [u32; 2], at: u64) {
        if let Some(record) = generation.and_then(|generation| self.record_mut(generation)) {
            record.render_finished = Some(Timed { at, value: size });
        }
    }

    pub(super) fn present_submitted(&mut self, generation: Option<u64>, value: Drawable, at: u64) {
        let Some(generation) = generation else { return };
        if let Some(record) = self.record_mut(generation) {
            record.present_submitted = Some(Timed { at, value });
        }
        let _ = step(&mut self.state, Event::PresentSubmitted(generation));
    }

    /// The latest generation when the synchronous resize draw returned without submitting it.
    pub(super) fn pending_current(&self) -> Option<serde_json::Value> {
        let generation = self.current()?;
        let record = self.records.iter().find(|record| record.generation == generation)?;
        (!record.terminal()).then(|| record.json())
    }

    pub(super) fn drain_through(&mut self, generation: Option<u64>) -> Option<Batch> {
        let generation = generation?;
        let count = self
            .records
            .iter()
            .take_while(|record| record.generation <= generation && record.terminal())
            .count();
        let records = self.records.drain(..count).collect::<Vec<_>>();
        let dropped = std::mem::take(&mut self.dropped);
        (!records.is_empty() || dropped > 0).then_some(Batch { dropped, records })
    }

    fn record_mut(&mut self, generation: u64) -> Option<&mut Record> {
        self.records
            .iter_mut()
            .find(|record| record.generation == generation)
    }

    #[cfg(test)]
    pub(super) fn records(&self) -> &VecDeque<Record> {
        &self.records
    }
}

#[cfg(feature = "frame-profile")]
pub(super) fn root_size(rt: &crate::runtime::RuntimeCore) -> Option<[f32; 2]> {
    rt.tree.nodes.first().map(|node| {
        [
            node.target[2] - node.target[0],
            node.target[3] - node.target[1],
        ]
    })
}
