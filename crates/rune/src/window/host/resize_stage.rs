//! A Windows sizing gesture can prepare a GPU frame before its proposed bounds take effect.
//! Keep that drawable private until winit confirms the exact resulting client size.

use super::super::render::{SubmittedWindowFrame, present_window_frame};
use super::{FrameState, Host, WindowFrame, host_step};
use crate::frame::host_step::{FrameHost, StepInfo};
use winit::dpi::PhysicalSize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ResizeSize {
    physical: PhysicalSize<u32>,
    scale: f32,
}

impl ResizeSize {
    fn viewport(self) -> (f32, f32) {
        (self.physical.width as f32 / self.scale, self.physical.height as f32 / self.scale)
    }

    fn matches(self, event: PhysicalSize<u32>, client: PhysicalSize<u32>, scale: f32) -> bool {
        self.physical == event && self.physical == client && self.scale == scale
    }
}

struct PendingFrame {
    submitted: SubmittedWindowFrame,
    state: FrameState,
    info: StepInfo,
}

struct Proposal {
    size: ResizeSize,
    frame: Option<PendingFrame>,
}

#[derive(Default)]
pub(in crate::window) struct ResizeStage(Option<Proposal>);

impl ResizeStage {
    pub(in crate::window) fn pending(&self) -> bool { self.0.is_some() }
}

impl Host {
    pub(in crate::window) fn prepare_live_resize(&mut self, physical: PhysicalSize<u32>) {
        let (Some(w), Some((gpu, _))) = (&mut self.win, &self.gpu) else { return };
        if w.native.mode.cpu_draws() || w.surface.is_none() || !w.surface_ok || gpu.is_lost()
            || w.native.cloak.held() || !self.app.rt().live_visible() || w.minimized()
            || !crate::platform::win32::in_size_move(&w.window) { return; }
        let size = ResizeSize { physical, scale: w.window.scale_factor() as f32 };
        // A repeated proposal can keep the frame it has. A different proposal must release the
        // old drawable before `resize_surface` asks DXGI to replace the swap-chain buffers.
        if w.native.resize.0.as_ref().is_some_and(|pending| pending.size == size) { return; }
        let old = w.window.inner_size();
        if physical == old { return; }
        w.resize_surface(Some(gpu), physical);
        self.app.rt_mut().set_viewport(size.viewport(), size.scale);
        let mut frame = WindowFrame { host: self, resizing: true, stage_size: Some(size), state: FrameState::default() };
        host_step::run(&mut frame);
        let pending = frame.state.staged.take().map(|(submitted, info)| PendingFrame { submitted, info, state: frame.state });
        #[cfg(feature = "frame-profile")]
        if crate::frame::live_log::on() {
            eprintln!("rune resize prepare: t={} before={old:?} proposed={physical:?} ready={}", crate::frame::live_log::now(), pending.is_some());
        }
        if let Some(w) = frame.host.win.as_mut() {
            w.native.resize.0 = Some(Proposal { size, frame: pending });
        }
    }

    /// `true` only for a matching, successfully staged drawable. Every other `WM_SIZE`
    /// proceeds through the ordinary resize path after dropping the speculative drawable.
    pub(in crate::window) fn present_live_resize(&mut self, actual: PhysicalSize<u32>) -> bool {
        let Some(w) = &mut self.win else { return false };
        let Some(proposal) = w.native.resize.0.take() else { return false };
        let client = w.window.inner_size();
        if !proposal.size.matches(actual, client, w.window.scale_factor() as f32)
            || w.minimized() || !w.surface_ok || self.gpu.as_ref().is_none_or(|(gpu, _)| gpu.is_lost()) {
            // Damage planning has observed the staged pixels, even though they did not land.
            w.native.damage.reset();
            w.request_redraw();
            return false;
        }
        let Some(PendingFrame { mut submitted, state, info }) = proposal.frame else {
            w.native.damage.reset();
            w.request_redraw();
            return false;
        };
        #[cfg(feature = "frame-profile")]
        if crate::frame::live_log::on() {
            eprintln!("rune resize commit: t={} actual={actual:?}", crate::frame::live_log::now());
        }
        let mut frame = WindowFrame { host: self, resizing: true, stage_size: None, state };
        let wait = !crate::env::flag("RUNE_WIN32_STAGE_NO_FLUSH");
        submitted.prefer_latest = crate::env::flag("RUNE_WIN32_STAGE_LATEST_PRESENT");
        let completed = present_window_frame(frame.host, submitted, wait, frame.state.looped.as_ref(), &mut frame.state.profile, &info);
        host_step::record_presented(&mut frame, completed, &info);
        frame.settle(completed, &info);
        true
    }

    pub(in crate::window) fn end_live_resize(&mut self) {
        let Some(w) = self.win.as_mut() else { return };
        if w.native.resize.0.take().is_none() { return; }
        let actual = w.window.inner_size();
        if actual.width == 0 || actual.height == 0 { return; }
        let size = ResizeSize { physical: actual, scale: w.window.scale_factor() as f32 };
        w.native.damage.reset();
        w.resize_surface(self.gpu.as_ref().map(|(gpu, _)| &**gpu), actual);
        self.app.rt_mut().set_viewport(size.viewport(), size.scale);
        w.request_redraw();
    }
}

#[cfg(test)]
#[path = "resize_stage_tests.rs"]
mod tests;
