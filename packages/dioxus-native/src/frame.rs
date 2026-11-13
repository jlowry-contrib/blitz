//! The frame clock, and the time stamps of input events.

use std::future::Future;
use std::sync::Arc;

use blitz_traits::shell::{FrameTime, Presented, ShellProvider};
use dioxus_core::{try_consume_context, use_hook};
use dioxus_html::{PointerData, TouchData};
use dioxus_native_dom::{NativePointerData, NativeTouchData};
use dioxus_signals::{ReadSignal, Signal};

/// A handle that steps a window running on the manual clock.
#[derive(Clone, Debug)]
pub struct FrameClock {
    _private: (),
}

impl FrameClock {
    /// Advances the clock by `dt` seconds; currently resolves at once with an unpainted frame.
    pub fn step(&self, dt: f64) -> impl Future<Output = Presented> + use<> {
        let _ = dt;
        std::future::ready(Presented::new(0, 0.0, false))
    }
}

/// Returns the current frame's time; currently a signal fixed at [`FrameTime::default`].
pub fn use_frame_time() -> ReadSignal<FrameTime> {
    use_hook(|| ReadSignal::new(Signal::new(FrameTime::default())))
}

/// Asks for one more frame; currently requests a redraw on either clock.
pub fn request_animation_frame() {
    if let Some(shell) = try_consume_context::<Arc<dyn ShellProvider>>() {
        shell.request_redraw();
    }
}

/// Returns the window's manual frame clock; currently always `None`.
pub fn use_frame_clock() -> Option<FrameClock> {
    None
}

/// Returns a pointer event's time stamp in seconds on the frame clock, if Blitz produced it.
pub fn pointer_time_stamp(data: &PointerData) -> Option<f64> {
    data.downcast::<NativePointerData>()
        .map(NativePointerData::time_stamp)
}

/// Returns a touch event's time stamp in seconds on the frame clock, if Blitz produced it.
pub fn touch_time_stamp(data: &TouchData) -> Option<f64> {
    data.downcast::<NativeTouchData>()
        .map(NativeTouchData::time_stamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_time_defaults_to_zero() {
        assert_eq!(FrameTime::default(), FrameTime::new(0.0, 0.0, 0));
    }
}
