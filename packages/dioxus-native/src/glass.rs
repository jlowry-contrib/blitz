//! The inputs the glass backend reads that don't come from the document.

use std::sync::{Arc, Mutex, PoisonError};

use anyrender_vello::glass::Accessibility;

/// A shared handle to the glass accessibility settings; currently stored but not yet read.
#[derive(Clone, Debug, Default)]
pub struct GlassInputs {
    accessibility: Arc<Mutex<Accessibility>>,
}

impl GlassInputs {
    /// Creates inputs with every accessibility setting off.
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
}
