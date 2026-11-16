//! The inputs the glass backend reads that don't come from the document.

use std::sync::{Arc, Mutex, PoisonError};

use anyrender_vello::glass::{Accessibility, AdaptStart, Appearance, FrameInputs};
use blitz_traits::shell::ColorScheme;

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
