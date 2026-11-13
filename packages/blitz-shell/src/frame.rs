//! Frame scheduling: the time source, manual steps and their acknowledgements.

use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Waker};

use anyrender::WindowRenderer;
use blitz_dom::Document;
use blitz_traits::shell::{ColorScheme, FrameTime, Presented};
use web_time::Instant;
use winit::window::WindowId;

use crate::event::{BlitzShellEvent, BlitzShellProxy};

/// The clock that drives a window's frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeSource {
    /// Frames follow the wall clock and the display.
    #[default]
    Wall,
    /// Frames advance only when a [`FrameClock`] steps them.
    Manual,
}

type AckFn = Box<dyn FnOnce(Presented) + Send>;

/// The acknowledgement of one step, called once with the frame that ran it.
///
/// Clones share one closure. If the last clone drops before anything acknowledges it, the
/// closure receives an unpainted frame 0 at time 0, so a driver never waits forever.
#[derive(Clone)]
pub struct StepAck(Arc<AckSlot>);

/// Owns the closure, so that only the last clone's drop can call it.
struct AckSlot(Mutex<Option<AckFn>>);

impl AckSlot {
    fn take(&self) -> Option<AckFn> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl Drop for AckSlot {
    fn drop(&mut self) {
        let ack = self
            .0
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(ack) = ack {
            ack(Presented::new(0, 0.0, false));
        }
    }
}

impl StepAck {
    /// Wraps the closure that receives the step's [`Presented`] frame.
    pub fn new(ack: impl FnOnce(Presented) + Send + 'static) -> Self {
        Self(Arc::new(AckSlot(Mutex::new(Some(Box::new(ack))))))
    }

    /// Calls the closure with `presented`, unless it has already been called.
    pub fn acknowledge(&self, presented: Presented) {
        if let Some(ack) = self.0.take() {
            ack(presented);
        }
    }
}

impl fmt::Debug for StepAck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pending = self
            .0
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        f.debug_struct("StepAck")
            .field("pending", &pending)
            .finish()
    }
}

/// A handle that steps a window on the manual clock, from any thread.
#[derive(Clone)]
pub struct FrameClock {
    proxy: BlitzShellProxy,
    window_id: WindowId,
}

impl FrameClock {
    pub(crate) fn new(proxy: BlitzShellProxy, window_id: WindowId) -> Self {
        Self { proxy, window_id }
    }

    /// Advances the window by `dt` seconds and draws one frame, then passes it to `ack`.
    pub fn step(&self, dt: f64, ack: Box<dyn FnOnce(Presented) + Send>) {
        self.proxy.send_event(BlitzShellEvent::Step {
            window_id: self.window_id,
            dt,
            ack: StepAck::new(ack),
        });
    }
}

impl fmt::Debug for FrameClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameClock")
            .field("window_id", &self.window_id)
            .finish_non_exhaustive()
    }
}

/// Called before each painted frame with the renderer, the frame time, the document scale
/// and the colour scheme.
pub type FrameHook<R> = Box<dyn FnMut(&mut R, FrameTime, f64, ColorScheme)>;

/// What the window should do once a frame has run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameOutcome {
    /// Whether to draw another frame.
    pub request_redraw: bool,
}

/// Runs a window's frames on its [`TimeSource`], independently of the window itself.
pub struct FrameScheduler<D: Document + ?Sized, R: WindowRenderer> {
    source: TimeSource,
    start: Instant,
    time: FrameTime,
    has_run: bool,
    steps: VecDeque<(f64, StepAck)>,
    waker: Option<Waker>,
    frame_hook: Option<FrameHook<R>>,
    _types: PhantomData<fn(&mut D, &mut R)>,
}

impl<D: Document + ?Sized, R: WindowRenderer> FrameScheduler<D, R> {
    /// Creates a scheduler whose clock starts now, at frame 0.
    pub fn new(source: TimeSource) -> Self {
        Self {
            source,
            start: Instant::now(),
            time: FrameTime::default(),
            has_run: false,
            steps: VecDeque::new(),
            waker: None,
            frame_hook: None,
            _types: PhantomData,
        }
    }

    /// Returns the clock this scheduler follows.
    pub fn time_source(&self) -> TimeSource {
        self.source
    }

    /// Returns the time of the latest frame.
    pub fn frame_time(&self) -> FrameTime {
        self.time
    }

    /// Returns the time, in seconds on this clock, to stamp an input event with.
    pub fn now(&self) -> f64 {
        match self.source {
            TimeSource::Wall => self.start.elapsed().as_secs_f64(),
            TimeSource::Manual => self.time.t,
        }
    }

    /// Sets the waker that frames poll the document with; without one, frames don't poll.
    pub fn set_waker(&mut self, waker: Option<Waker>) {
        self.waker = waker;
    }

    /// Sets the hook that each painted frame calls just before `paint`.
    pub fn set_frame_hook(&mut self, hook: Option<FrameHook<R>>) {
        self.frame_hook = hook;
    }

    /// Calls the frame hook, if one is set, for a paint outside [`frame`](Self::frame).
    pub fn run_hook(&mut self, renderer: &mut R, time: FrameTime, scale: f64, scheme: ColorScheme) {
        if let Some(hook) = &mut self.frame_hook {
            hook(renderer, time, scale, scheme);
        }
    }

    /// Returns whether steps are waiting for a frame.
    pub fn has_pending_steps(&self) -> bool {
        !self.steps.is_empty()
    }

    /// Queues a step of `dt` seconds; the next frame runs it and acknowledges it.
    ///
    /// On the wall clock, the frame keeps the wall time and ignores `dt`.
    pub fn step(&mut self, dt: f64, ack: StepAck) {
        self.steps.push_back((dt, ack));
    }

    /// Polls the document's pending work and returns whether to draw a frame for it.
    ///
    /// On the manual clock this is always `false`: only a step draws.
    pub fn poll(&mut self, doc: &mut D, cx: Option<Context<'_>>) -> bool {
        doc.poll(cx) && self.source == TimeSource::Wall
    }

    /// Runs one frame: advances the clock, polls, resolves, flushes events, then paints.
    ///
    /// `paint` runs, after the frame hook, only when `can_paint` is true and the document
    /// isn't waiting on render-blocking resources. The step this frame runs, if any, is
    /// acknowledged after `paint` returns.
    pub fn frame(
        &mut self,
        doc: &mut D,
        renderer: &mut R,
        can_paint: bool,
        paint: impl FnOnce(&mut D, &mut R),
    ) -> FrameOutcome {
        let step = self.steps.pop_front();
        self.advance(step.as_ref().map(|(dt, _)| *dt));
        let time = self.time;

        doc.begin_frame(time);
        if let Some(waker) = &self.waker {
            doc.poll(Some(Context::from_waker(waker)));
        }
        doc.inner_mut().resolve(time.t);
        doc.flush_queued_events();

        let painted = can_paint && !doc.inner().has_pending_critical_resources();
        if painted {
            let inner = doc.inner();
            let viewport = inner.viewport();
            let (scale, scheme) = (viewport.scale_f64(), viewport.color_scheme);
            drop(inner);
            self.run_hook(renderer, time, scale, scheme);
            paint(doc, renderer);
        }
        if let Some((_, ack)) = step {
            ack.acknowledge(Presented::new(time.frame, time.t, painted));
        }

        let request_redraw = self.source == TimeSource::Wall
            && painted
            && (doc.inner().is_animating() || renderer.wants_redraw() || doc.take_frame_request());
        FrameOutcome { request_redraw }
    }

    /// Moves the clock to this frame's time; a manual frame without a step keeps its time.
    fn advance(&mut self, step: Option<f64>) {
        match self.source {
            TimeSource::Manual => {
                if let Some(dt) = step {
                    self.time = FrameTime::new(self.time.t + dt, dt, self.time.frame + 1);
                }
            }
            TimeSource::Wall => {
                let t = self.now();
                self.time = if self.has_run {
                    FrameTime::new(t, t - self.time.t, self.time.frame + 1)
                } else {
                    FrameTime::new(t, 0.0, 0)
                };
                self.has_run = true;
            }
        }
    }
}

impl<D: Document + ?Sized, R: WindowRenderer> Drop for FrameScheduler<D, R> {
    fn drop(&mut self) {
        let time = self.time;
        for (_, ack) in self.steps.drain(..) {
            ack.acknowledge(Presented::new(time.frame, time.t, false));
        }
    }
}

impl<D: Document + ?Sized, R: WindowRenderer> fmt::Debug for FrameScheduler<D, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameScheduler")
            .field("source", &self.source)
            .field("time", &self.time)
            .field("pending_steps", &self.steps.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyrender::{NullScenePainter, RenderContext, WindowHandle};
    use blitz_dom::{BaseDocument, DocGuard, DocGuardMut, Document, DocumentConfig, QualName, ns};
    use blitz_traits::shell::{ColorScheme, FrameTime, Presented, Viewport};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Log(Rc<RefCell<Vec<String>>>);

    impl Log {
        fn push(&self, entry: impl Into<String>) {
            self.0.borrow_mut().push(entry.into());
        }

        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.0.borrow_mut())
        }
    }

    /// A document that records the scheduler's calls into it.
    struct RecordingDoc {
        base: BaseDocument,
        log: Log,
        poll_result: bool,
        frame_request: bool,
    }

    impl RecordingDoc {
        fn new(log: &Log) -> Self {
            Self {
                base: BaseDocument::new(DocumentConfig::default()),
                log: log.clone(),
                poll_result: false,
                frame_request: false,
            }
        }

        /// A document whose `<canvas src>` keeps it animating.
        fn animating(log: &Log) -> Self {
            let mut doc = Self::new(log);
            let name = |local: &str| QualName::new(None, ns!(html), local.into());
            let root = doc.base.root_node().id;
            let mut mutator = doc.base.mutate();
            let html = mutator.create_element(name("html"), Vec::new());
            let attr = blitz_dom::Attribute {
                name: QualName::new(None, ns!(), "src".into()),
                value: "1".to_string(),
            };
            let canvas = mutator.create_element(name("canvas"), vec![attr]);
            mutator.append_children(html, &[canvas]);
            mutator.append_children(root, &[html]);
            drop(mutator);
            assert!(doc.base.is_animating());
            doc
        }
    }

    impl Document for RecordingDoc {
        fn inner(&self) -> DocGuard<'_> {
            DocGuard::Ref(&self.base)
        }

        // The scheduler borrows the inner document mutably only to resolve it.
        fn inner_mut(&mut self) -> DocGuardMut<'_> {
            self.log.push("resolve");
            DocGuardMut::Ref(&mut self.base)
        }

        fn poll(&mut self, _cx: Option<std::task::Context>) -> bool {
            self.log.push("poll");
            self.poll_result
        }

        fn begin_frame(&mut self, time: FrameTime) {
            self.log
                .push(format!("begin_frame {} {} {}", time.t, time.dt, time.frame));
            self.base.set_frame_time(time);
        }

        fn take_frame_request(&mut self) -> bool {
            std::mem::take(&mut self.frame_request)
        }

        fn flush_queued_events(&mut self) {
            self.log.push("flush");
        }
    }

    /// A renderer that paints nothing and records each render.
    struct FakeRenderer {
        log: Log,
        wants_redraw: bool,
    }

    impl RenderContext for FakeRenderer {}

    impl WindowRenderer for FakeRenderer {
        type ScenePainter<'a> = NullScenePainter;

        fn resume<F: FnOnce() + 'static>(
            &mut self,
            _window: Arc<dyn WindowHandle>,
            _width: u32,
            _height: u32,
            on_ready: F,
        ) {
            on_ready();
        }

        fn complete_resume(&mut self) -> bool {
            true
        }

        fn suspend(&mut self) {}

        fn is_active(&self) -> bool {
            true
        }

        fn set_size(&mut self, _width: u32, _height: u32) {}

        fn render<F: FnOnce(&mut Self::ScenePainter<'_>)>(&mut self, draw_fn: F) {
            self.log.push("render");
            draw_fn(&mut NullScenePainter::new());
        }

        fn wants_redraw(&self) -> bool {
            self.wants_redraw
        }
    }

    type Scheduler = FrameScheduler<RecordingDoc, FakeRenderer>;

    struct Fixture {
        scheduler: Scheduler,
        doc: RecordingDoc,
        renderer: FakeRenderer,
        log: Log,
        acks: Arc<Mutex<Vec<Presented>>>,
    }

    impl Fixture {
        fn new(source: TimeSource) -> Self {
            Self::with_doc(source, RecordingDoc::new)
        }

        fn with_doc(source: TimeSource, doc: impl FnOnce(&Log) -> RecordingDoc) -> Self {
            let log = Log::default();
            let mut scheduler = FrameScheduler::new(source);
            scheduler.set_waker(Some(Waker::noop().clone()));
            Self {
                scheduler,
                doc: doc(&log),
                renderer: FakeRenderer {
                    log: log.clone(),
                    wants_redraw: false,
                },
                log,
                acks: Arc::default(),
            }
        }

        fn step(&mut self, dt: f64) {
            let acks = Arc::clone(&self.acks);
            self.scheduler.step(
                dt,
                StepAck::new(move |presented| acks.lock().unwrap().push(presented)),
            );
        }

        fn frame(&mut self, can_paint: bool) -> FrameOutcome {
            let log = self.log.clone();
            self.scheduler.frame(
                &mut self.doc,
                &mut self.renderer,
                can_paint,
                |_, renderer| {
                    log.push("paint");
                    renderer.render(|_| {});
                },
            )
        }

        fn acks(&self) -> Vec<Presented> {
            self.acks.lock().unwrap().clone()
        }
    }

    #[test]
    fn step_runs_one_frame_in_order() {
        let mut fx = Fixture::new(TimeSource::Manual);
        fx.step(0.25);
        fx.frame(true);
        assert_eq!(
            fx.log.take(),
            [
                "begin_frame 0.25 0.25 1",
                "poll",
                "resolve",
                "flush",
                "paint",
                "render"
            ]
        );
        assert_eq!(fx.acks(), [Presented::new(1, 0.25, true)]);
        assert_eq!(fx.scheduler.frame_time(), FrameTime::new(0.25, 0.25, 1));
    }

    #[test]
    fn step_acknowledges_when_nothing_paints() {
        let mut fx = Fixture::new(TimeSource::Manual);
        fx.step(0.5);
        fx.frame(false);
        assert!(!fx.log.take().contains(&"paint".to_string()));
        assert_eq!(fx.acks(), [Presented::new(1, 0.5, false)]);
    }

    #[test]
    fn queued_steps_resolve_in_order() {
        let mut fx = Fixture::new(TimeSource::Manual);
        for dt in [0.25, 0.5, 0.125] {
            fx.step(dt);
        }
        assert!(fx.scheduler.has_pending_steps());
        for _ in 0..3 {
            fx.frame(false);
        }
        assert!(!fx.scheduler.has_pending_steps());
        assert_eq!(
            fx.acks(),
            [
                Presented::new(1, 0.25, false),
                Presented::new(2, 0.75, false),
                Presented::new(3, 0.875, false),
            ]
        );
    }

    #[test]
    fn dropped_ack_reports_unpainted() {
        let acks: Arc<Mutex<Vec<Presented>>> = Arc::default();
        let sink = Arc::clone(&acks);
        let ack = StepAck::new(move |presented| sink.lock().unwrap().push(presented));
        let copy = ack.clone();
        drop(ack);
        assert!(acks.lock().unwrap().is_empty(), "a clone still holds it");
        drop(copy);
        let acks = acks.lock().unwrap().clone();
        assert_eq!(acks.len(), 1);
        assert!(!acks[0].painted);

        let mut fx = Fixture::new(TimeSource::Manual);
        fx.step(0.25);
        fx.frame(true);
        fx.step(0.25);
        let Fixture {
            scheduler, acks, ..
        } = fx;
        drop(scheduler);
        assert_eq!(
            *acks.lock().unwrap(),
            [
                Presented::new(1, 0.25, true),
                Presented::new(1, 0.25, false)
            ]
        );
    }

    #[test]
    fn acknowledging_takes_the_closure_once() {
        let acks: Arc<Mutex<Vec<Presented>>> = Arc::default();
        let sink = Arc::clone(&acks);
        let ack = StepAck::new(move |presented| sink.lock().unwrap().push(presented));
        ack.clone().acknowledge(Presented::new(4, 1.0, true));
        ack.acknowledge(Presented::new(5, 2.0, true));
        drop(ack);
        assert_eq!(*acks.lock().unwrap(), [Presented::new(4, 1.0, true)]);
    }

    #[test]
    fn manual_mode_never_self_schedules() {
        let mut fx = Fixture::with_doc(TimeSource::Manual, RecordingDoc::animating);
        fx.renderer.wants_redraw = true;
        fx.doc.frame_request = true;
        fx.step(0.25);
        assert!(!fx.frame(true).request_redraw);
        assert!(!fx.frame(true).request_redraw);
    }

    #[test]
    fn manual_poll_never_requests_a_redraw() {
        let mut fx = Fixture::new(TimeSource::Manual);
        fx.doc.poll_result = true;
        assert!(!fx.scheduler.poll(&mut fx.doc, None));
        assert_eq!(fx.log.take(), ["poll"]);

        let mut fx = Fixture::new(TimeSource::Wall);
        fx.doc.poll_result = true;
        assert!(fx.scheduler.poll(&mut fx.doc, None));
    }

    #[test]
    fn os_redraw_in_manual_mode_keeps_the_frame() {
        let mut fx = Fixture::new(TimeSource::Manual);
        fx.frame(true);
        assert_eq!(fx.scheduler.frame_time(), FrameTime::new(0.0, 0.0, 0));
        fx.step(0.5);
        fx.frame(true);
        fx.log.take();
        fx.frame(true);
        assert_eq!(fx.log.take()[0], "begin_frame 0.5 0.5 1");
        assert_eq!(fx.scheduler.frame_time(), FrameTime::new(0.5, 0.5, 1));
        assert_eq!(fx.acks(), [Presented::new(1, 0.5, true)]);
    }

    #[test]
    fn wall_mode_redraws_for_renderer_and_frame_requests() {
        let mut fx = Fixture::new(TimeSource::Wall);
        assert!(!fx.frame(true).request_redraw);
        let first = fx.scheduler.frame_time();
        assert_eq!((first.dt, first.frame), (0.0, 0));

        fx.renderer.wants_redraw = true;
        assert!(fx.frame(true).request_redraw);
        assert_eq!(fx.scheduler.frame_time().frame, 1);

        fx.renderer.wants_redraw = false;
        fx.doc.frame_request = true;
        assert!(fx.frame(true).request_redraw);
        assert!(!fx.frame(true).request_redraw, "a request is taken once");

        let mut fx = Fixture::with_doc(TimeSource::Wall, RecordingDoc::animating);
        assert!(fx.frame(true).request_redraw);
    }

    #[test]
    fn frame_hook_runs_before_paint() {
        let mut fx = Fixture::new(TimeSource::Manual);
        fx.doc
            .base
            .set_viewport(Viewport::new(100, 100, 3.0, ColorScheme::Dark));
        let log = fx.log.clone();
        fx.scheduler
            .set_frame_hook(Some(Box::new(move |_, time, scale, scheme| {
                log.push(format!("hook {} {} {:?}", time.t, scale, scheme));
            })));
        fx.step(0.25);
        fx.frame(true);
        assert_eq!(
            fx.log.take(),
            [
                "begin_frame 0.25 0.25 1",
                "poll",
                "resolve",
                "flush",
                "hook 0.25 3 Dark",
                "paint",
                "render"
            ]
        );

        fx.frame(false);
        assert!(!fx.log.take().iter().any(|entry| entry.starts_with("hook")));
    }

    #[test]
    fn run_hook_passes_the_resume_inputs() {
        let mut fx = Fixture::new(TimeSource::Manual);
        fx.scheduler.run_hook(
            &mut fx.renderer,
            FrameTime::new(0.5, 0.25, 2),
            3.0,
            ColorScheme::Dark,
        );
        assert!(fx.log.take().is_empty(), "no hook, no call");

        let log = fx.log.clone();
        fx.scheduler
            .set_frame_hook(Some(Box::new(move |_, time, scale, scheme| {
                log.push(format!("hook {} {} {:?}", time.t, scale, scheme));
            })));
        fx.scheduler.run_hook(
            &mut fx.renderer,
            FrameTime::new(0.5, 0.25, 2),
            3.0,
            ColorScheme::Dark,
        );
        assert_eq!(fx.log.take(), ["hook 0.5 3 Dark"]);
    }

    #[test]
    fn frame_clock_is_shareable() {
        fn assert_shareable<T: Clone + Send + Sync + 'static>() {}
        assert_shareable::<FrameClock>();
        assert_shareable::<StepAck>();
    }
}
