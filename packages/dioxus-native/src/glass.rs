//! The inputs the glass backend reads that don't come from the document.

use std::sync::{Arc, Mutex, PoisonError};

use anyrender_vello::glass::{Accessibility, AdaptStart, Appearance, FrameInputs};
use blitz_shell::FrameHook;
use blitz_traits::shell::{ColorScheme, FrameTime};

/// A shared handle to the accessibility settings and adapt-start mode the glass backend reads.
#[derive(Clone, Debug)]
pub struct GlassInputs {
    accessibility: Arc<Mutex<Accessibility>>,
    adapt_start: Arc<Mutex<AdaptStart>>,
}

impl Default for GlassInputs {
    /// Creates inputs with every accessibility setting off, adapting live.
    fn default() -> Self {
        Self {
            accessibility: Arc::default(),
            adapt_start: Arc::new(Mutex::new(AdaptStart::Live)),
        }
    }
}

impl GlassInputs {
    /// Creates inputs with every accessibility setting off, adapting live.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the accessibility settings, as seen by every clone of this handle.
    pub fn set_accessibility(&self, accessibility: Accessibility) {
        *self
            .accessibility
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = accessibility;
    }

    /// Returns the current accessibility settings.
    pub fn accessibility(&self) -> Accessibility {
        *self
            .accessibility
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets when adaptive glass decides its look, as seen by every clone of this handle.
    pub fn set_adapt_start(&self, adapt_start: AdaptStart) {
        *self
            .adapt_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = adapt_start;
    }

    /// Returns when adaptive glass decides its look.
    pub fn adapt_start(&self) -> AdaptStart {
        *self
            .adapt_start
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Builds the glass backend's inputs for one frame; a dark scheme gives the dark appearance.
pub(crate) fn frame_inputs(
    time: f64,
    scale: f64,
    scheme: ColorScheme,
    inputs: &GlassInputs,
) -> FrameInputs {
    let appearance = match scheme {
        ColorScheme::Dark => Appearance::Dark,
        _ => Appearance::Light,
    };
    FrameInputs::new(time, scale)
        .with_appearance(appearance)
        .with_accessibility(inputs.accessibility())
        .with_adapt_start(inputs.adapt_start())
}

/// A renderer the glass frame hook hands each frame's inputs to.
pub(crate) trait GlassFrameTarget {
    /// Returns the settings the app shares with the glass backend.
    fn glass_inputs(&self) -> &GlassInputs;

    /// Takes the glass backend's inputs for the frame about to be painted.
    fn set_glass_frame_inputs(&mut self, inputs: FrameInputs);
}

/// Returns the frame hook a dioxus-native window runs before each paint: it builds the
/// glass inputs from the frame's time, scale and the document's colour scheme.
pub(crate) fn glass_frame_hook<R: GlassFrameTarget>() -> FrameHook<R> {
    Box::new(|renderer: &mut R, time: FrameTime, scale, scheme| {
        let inputs = frame_inputs(time.t, scale, scheme, renderer.glass_inputs());
        renderer.set_glass_frame_inputs(inputs);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glass_inputs_share_their_accessibility() {
        let inputs = GlassInputs::new();
        let clone = inputs.clone();
        let mut accessibility = Accessibility::new();
        accessibility.reduce_transparency = true;
        clone.set_accessibility(accessibility);
        assert_eq!(inputs.accessibility(), accessibility);
    }

    #[test]
    fn frame_inputs_follow_scheme_and_accessibility() {
        let inputs = GlassInputs::new();
        let mut accessibility = Accessibility::new();
        accessibility.reduce_transparency = true;
        inputs.set_accessibility(accessibility);
        let frame = frame_inputs(1.5, 3.0, ColorScheme::Dark, &inputs);
        assert_eq!(frame.time, 1.5);
        assert_eq!(frame.scale, 3.0);
        assert_eq!(frame.appearance, Appearance::Dark);
        assert!(frame.accessibility.reduce_transparency);
        assert!(!frame.accessibility.increase_contrast);
    }

    #[test]
    fn glass_inputs_carry_adapt_start_into_the_frame() {
        let inputs = GlassInputs::new();
        assert_eq!(inputs.adapt_start(), AdaptStart::Live);
        let clone = inputs.clone();
        for mode in [
            AdaptStart::AppearanceOnly,
            AdaptStart::SettledAtMount,
            AdaptStart::Live,
        ] {
            clone.set_adapt_start(mode);
            assert_eq!(inputs.adapt_start(), mode);
            let frame = frame_inputs(1.0, 3.0, ColorScheme::Light, &inputs);
            assert_eq!(frame.adapt_start, mode);
        }
    }

    #[test]
    fn default_inputs_are_light_with_no_accessibility() {
        let frame = frame_inputs(0.0, 1.0, ColorScheme::default(), &GlassInputs::default());
        assert_eq!(frame, FrameInputs::new(0.0, 1.0));
        assert_eq!(frame.appearance, Appearance::Light);
        assert_eq!(frame.accessibility, Accessibility::new());
        assert_eq!(frame.adapt_start, AdaptStart::Live);
    }
}

#[cfg(test)]
mod frame_hook_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::task::Waker;

    use anyrender::{NullScenePainter, RenderContext, WindowHandle, WindowRenderer};
    use blitz_shell::{FrameScheduler, TimeSource};
    use blitz_traits::shell::Viewport;
    use dioxus_core::{Element, VNode, VirtualDom, consume_context, use_hook};

    use super::*;
    use crate::{ColorSchemeOverride, DioxusDocument, DocumentConfig};

    /// A renderer that paints nothing and records the glass inputs each frame hands it.
    struct Recorder {
        glass_inputs: GlassInputs,
        frames: Vec<FrameInputs>,
    }

    impl RenderContext for Recorder {}

    impl WindowRenderer for Recorder {
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
            draw_fn(&mut NullScenePainter::new());
        }
    }

    impl GlassFrameTarget for Recorder {
        fn glass_inputs(&self) -> &GlassInputs {
            &self.glass_inputs
        }

        fn set_glass_frame_inputs(&mut self, inputs: FrameInputs) {
            self.frames.push(inputs);
        }
    }

    /// Where the app leaves the colour scheme handle it took from its context.
    #[derive(Clone, Default)]
    struct Slot(Rc<RefCell<Option<ColorSchemeOverride>>>);

    fn app() -> Element {
        use_hook(|| {
            let slot = consume_context::<Slot>();
            *slot.0.borrow_mut() = Some(consume_context::<ColorSchemeOverride>());
        });
        VNode::empty()
    }

    /// Runs one frame and returns the appearance the frame hook handed the renderer.
    fn frame(
        scheduler: &mut FrameScheduler<DioxusDocument, Recorder>,
        doc: &mut DioxusDocument,
        renderer: &mut Recorder,
    ) -> Appearance {
        let before = renderer.frames.len();
        scheduler.frame(doc, renderer, true, |_, _| {});
        assert_eq!(renderer.frames.len(), before + 1, "the frame hook ran");
        renderer.frames[before].appearance
    }

    #[test]
    fn the_frame_hook_draws_the_scheme_app_code_sets() {
        let slot = Slot::default();
        let vdom = VirtualDom::new(app).with_root_context(slot.clone());
        let mut doc = DioxusDocument::new(
            vdom,
            DocumentConfig {
                viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        );
        doc.initial_build();
        let scheme = slot.0.borrow().clone().expect("the app mounted");
        let mut scheduler = FrameScheduler::new(TimeSource::Manual);
        scheduler.set_frame_hook(Some(glass_frame_hook()));
        scheduler.set_waker(Some(Waker::noop().clone()));
        let mut renderer = Recorder {
            glass_inputs: GlassInputs::new(),
            frames: Vec::new(),
        };

        assert_eq!(
            frame(&mut scheduler, &mut doc, &mut renderer),
            Appearance::Light
        );
        scheme.set(Some(ColorScheme::Dark));
        assert_eq!(
            frame(&mut scheduler, &mut doc, &mut renderer),
            Appearance::Dark
        );
        scheme.set(None);
        assert_eq!(
            frame(&mut scheduler, &mut doc, &mut renderer),
            Appearance::Light
        );
        scheme.set(Some(ColorScheme::Light));
        assert_eq!(
            frame(&mut scheduler, &mut doc, &mut renderer),
            Appearance::Light
        );

        // The shell's theme changes under a dark override, as `ThemeChanged` writes it.
        scheme.set(Some(ColorScheme::Dark));
        assert_eq!(
            frame(&mut scheduler, &mut doc, &mut renderer),
            Appearance::Dark
        );
        doc.inner.borrow_mut().viewport_mut().color_scheme = ColorScheme::Light;
        assert_eq!(
            frame(&mut scheduler, &mut doc, &mut renderer),
            Appearance::Dark
        );
    }
}
