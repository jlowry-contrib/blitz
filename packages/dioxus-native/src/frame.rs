//! The frame clock, and the time stamps of input events.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use blitz_traits::shell::{FrameTime, Presented, ShellProvider};
use dioxus_core::{try_consume_context, use_hook};
use dioxus_html::{PointerData, TouchData};
use dioxus_native_dom::{FrameState, NativePointerData, NativeTouchData};
use dioxus_signals::{ReadSignal, Signal};
use futures_channel::oneshot;

/// A handle that steps a window running on the manual clock.
#[derive(Clone, Debug)]
pub struct FrameClock {
    inner: blitz_shell::FrameClock,
    latest: Arc<Mutex<Presented>>,
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FrameClock>();
};

impl FrameClock {
    pub(crate) fn new(inner: blitz_shell::FrameClock) -> Self {
        Self {
            inner,
            latest: Arc::new(Mutex::new(Presented::new(0, 0.0, false))),
        }
    }

    /// Advances the window by `dt` seconds and resolves once that frame has been drawn.
    ///
    /// An acknowledgement lost in transit resolves to `Presented::new(frame, t, false)`, where
    /// `frame` and `t` are the latest this clock has seen.
    pub fn step(&self, dt: f64) -> impl Future<Output = Presented> + Send + use<> {
        let (sender, receiver) = oneshot::channel();
        self.inner.step(
            dt,
            Box::new(move |presented| {
                let _ = sender.send(presented);
            }),
        );
        let latest = Arc::clone(&self.latest);
        async move { settle(&latest, receiver.await.ok()) }
    }
}

/// Records a received acknowledgement as the latest, or stands in for a lost one.
fn settle(latest: &Mutex<Presented>, received: Option<Presented>) -> Presented {
    let mut latest = latest.lock().unwrap_or_else(PoisonError::into_inner);
    match received {
        // The shell reports a lost acknowledgement as frame 0, which no step ever runs.
        Some(presented) if presented.frame != 0 => {
            *latest = presented;
            presented
        }
        _ => Presented::new(latest.frame, latest.t, false),
    }
}

/// Returns the current frame's time, written before each render.
pub fn use_frame_time() -> ReadSignal<FrameTime> {
    use_hook(|| match try_consume_context::<FrameState>() {
        Some(state) => state.time(),
        None => ReadSignal::new(Signal::new(FrameTime::default())),
    })
}

/// Asks for one more frame on the wall clock; on the manual clock, only a step draws.
pub fn request_animation_frame() {
    if let Some(state) = try_consume_context::<FrameState>() {
        state.request_frame();
    }
    // An idle window on the wall clock has no frame coming to read the request, so wake it.
    if try_consume_context::<FrameClock>().is_none()
        && let Some(shell) = try_consume_context::<Arc<dyn ShellProvider>>()
    {
        shell.request_redraw();
    }
}

/// Returns the window's manual frame clock, or `None` on the wall clock.
pub fn use_frame_clock() -> Option<FrameClock> {
    use_hook(try_consume_context::<FrameClock>)
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

    #[test]
    fn a_received_acknowledgement_becomes_the_latest() {
        let latest = Mutex::new(Presented::new(0, 0.0, false));
        let presented = Presented::new(2, 0.5, true);
        assert_eq!(settle(&latest, Some(presented)), presented);
        assert_eq!(*latest.lock().unwrap(), presented);
    }

    #[test]
    fn a_lost_acknowledgement_repeats_the_latest_frame_unpainted() {
        let latest = Mutex::new(Presented::new(3, 0.75, true));
        let lost = Presented::new(3, 0.75, false);
        assert_eq!(settle(&latest, None), lost);
        assert_eq!(settle(&latest, Some(Presented::new(0, 0.0, false))), lost);
        assert_eq!(*latest.lock().unwrap(), Presented::new(3, 0.75, true));
    }
}
