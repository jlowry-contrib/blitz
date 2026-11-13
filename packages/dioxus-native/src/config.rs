use anyrender::CompositeAlphaMode;
use blitz_dom::FontContext;
use dioxus_core::LaunchConfig;
use peniko::Color;
use winit::window::WindowAttributes;

#[cfg(feature = "vello")]
use crate::GlassInputs;
use crate::KeyboardShortcut;
use crate::assets::AssetHandler;
use crate::{SafeArea, ScrollPhysics, TimeSource};

/// Launch-time configuration for a dioxus-native application.
pub struct Config {
    pub(crate) window_attributes: WindowAttributes,
    pub(crate) font_ctx: Option<FontContext>,
    pub(crate) alpha_mode: Option<CompositeAlphaMode>,
    pub(crate) base_color: Option<Color>,
    #[cfg_attr(not(feature = "devtools"), allow(dead_code))]
    pub(crate) devtools_shortcuts: Vec<KeyboardShortcut>,
    pub(crate) asset_handler: Option<AssetHandler>,
    pub(crate) time_source: TimeSource,
    #[cfg(feature = "vello")]
    pub(crate) glass_backend: Option<Box<dyn anyrender_vello::glass::GlassBackend + Send>>,
    #[cfg(feature = "vello")]
    pub(crate) glass_inputs: Option<GlassInputs>,
    pub(crate) safe_area: SafeArea,
    pub(crate) scroll_physics: ScrollPhysics,
}

impl LaunchConfig for Config {}

impl Default for Config {
    fn default() -> Self {
        let window_attributes = WindowAttributes::default()
            .with_title(dioxus_cli_config::app_title().unwrap_or_else(|| "Dioxus App".to_string()));

        // On WASM, append the canvas to the document body. Surface size is
        // intentionally not seeded: winit-web's with_surface_size writes inline
        // canvas.style.width/height that overrides host CSS. View::init reads
        // the canvas's CSS layout box when winit reports a 0×0 initial size.
        // To target an existing `<canvas>`, replace these attributes via
        // [`Config::with_window_attributes`].
        #[cfg(target_arch = "wasm32")]
        let window_attributes = {
            use winit::platform::web::WindowAttributesWeb;
            window_attributes.with_platform_attributes(Box::new(
                WindowAttributesWeb::default().with_append(true),
            ))
        };

        Self {
            window_attributes,
            font_ctx: None,
            alpha_mode: None,
            base_color: None,
            devtools_shortcuts: if cfg!(feature = "devtools") {
                KeyboardShortcut::chrome_devtools().to_vec()
            } else {
                Vec::new()
            },
            asset_handler: None,
            time_source: TimeSource::default(),
            #[cfg(feature = "vello")]
            glass_backend: None,
            #[cfg(feature = "vello")]
            glass_inputs: None,
            safe_area: SafeArea::default(),
            scroll_physics: ScrollPhysics::default(),
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the configuration for the window.
    pub fn with_window_attributes(mut self, attrs: WindowAttributes) -> Self {
        self.window_attributes = attrs;
        self
    }

    /// Set a custom [`FontContext`] for the document.
    ///
    /// On WASM, browsers don't expose system fonts, so a `FontContext` with
    /// bundled fonts must be provided. Use [`crate::build_single_font_ctx`] for
    /// the common one-font case.
    pub fn with_font_ctx(mut self, font_ctx: FontContext) -> Self {
        self.font_ctx = Some(font_ctx);
        self
    }

    /// Serve `dioxus://` requests from `handler` before the bundled asset files.
    ///
    /// The handler receives the URL path, such as `/symbol/heart.png`, and returns the
    /// response body, or `None` to fall back to the asset files. Requests run on a few
    /// worker threads, so a slow handler doesn't block the document.
    pub fn with_asset_handler(
        mut self,
        handler: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        self.asset_handler = Some(std::sync::Arc::new(handler));
        self
    }

    /// Set the alpha mode used when compositing the window surface.
    ///
    /// This controls how the rendered content is blended with the background,
    /// e.g. to enable transparent windows. Not all renderers support every
    /// mode; unsupported modes are ignored by the active renderer.
    pub fn with_alpha_mode(mut self, alpha_mode: CompositeAlphaMode) -> Self {
        self.alpha_mode = Some(alpha_mode);
        self
    }

    /// Set the base (background) color used to clear each frame.
    pub fn with_base_color(mut self, base_color: Color) -> Self {
        self.base_color = Some(base_color);
        self
    }

    /// Set the keyboard shortcuts that open devtools inspecting a window's
    /// document when pressed in it (replacing any set before). The key press
    /// isn't delivered to the app.
    ///
    /// With the `devtools` feature, this defaults to Chrome's devtools
    /// shortcuts ([`KeyboardShortcut::chrome_devtools`]). Pass an empty list to
    /// disable them:
    ///
    /// ```no_run
    /// # use dioxus_native::{Config, KeyboardShortcut};
    /// # use dioxus_native::winit::keyboard::{KeyCode, ModifiersState};
    /// // Only F11
    /// let config = Config::new()
    ///     .with_devtools_shortcuts([KeyboardShortcut::new(ModifiersState::empty(), KeyCode::F11)]);
    /// // None
    /// let config = Config::new().with_devtools_shortcuts([]);
    /// ```
    ///
    /// Shortcuts do nothing without the `devtools` feature. With the
    /// `embedded-devtools-frontend` feature too, devtools open in a browser;
    /// otherwise they open Chrome's own devtools frontend, which only works
    /// where Chrome can be launched directly (e.g. macOS). Elsewhere, a
    /// `devtools://` URL to paste into Chrome is printed.
    pub fn with_devtools_shortcuts(
        mut self,
        shortcuts: impl IntoIterator<Item = KeyboardShortcut>,
    ) -> Self {
        self.devtools_shortcuts = shortcuts.into_iter().collect();
        self
    }

    /// Set the clock that drives frames; on [`TimeSource::Manual`], only a `FrameClock` draws.
    pub fn with_time_source(mut self, time_source: TimeSource) -> Self {
        self.time_source = time_source;
        self
    }

    /// Install a glass backend into the Vello renderer.
    #[cfg(feature = "vello")]
    pub fn with_glass_backend(
        mut self,
        backend: Box<dyn anyrender_vello::glass::GlassBackend + Send>,
    ) -> Self {
        self.glass_backend = Some(backend);
        self
    }

    /// Set the accessibility settings the glass backend reads each frame.
    #[cfg(feature = "vello")]
    pub fn with_glass_inputs(mut self, inputs: GlassInputs) -> Self {
        self.glass_inputs = Some(inputs);
        self
    }

    /// Set whether the document is laid out inside the safe area or under the whole window.
    pub fn with_safe_area(mut self, safe_area: SafeArea) -> Self {
        self.safe_area = safe_area;
        self
    }

    /// Set the physics that touch-driven scrolling follows in the document.
    pub fn with_scroll_physics(mut self, scroll_physics: ScrollPhysics) -> Self {
        self.scroll_physics = scroll_physics;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_shortcuts_default_to_chrome_with_the_devtools_feature() {
        let expected = if cfg!(feature = "devtools") {
            KeyboardShortcut::chrome_devtools().to_vec()
        } else {
            Vec::new()
        };
        assert_eq!(Config::new().devtools_shortcuts, expected);
    }

    #[test]
    fn with_devtools_shortcuts_replaces_them() {
        let f11 = KeyboardShortcut::new(Default::default(), winit::keyboard::KeyCode::F11);
        assert_eq!(
            Config::new()
                .with_devtools_shortcuts([f11])
                .devtools_shortcuts,
            vec![f11]
        );
        assert!(
            Config::new()
                .with_devtools_shortcuts([])
                .devtools_shortcuts
                .is_empty()
        );
    }

    #[test]
    fn config_builders_store_their_values() {
        let config = Config::new()
            .with_time_source(TimeSource::Manual)
            .with_safe_area(SafeArea::Ignore)
            .with_scroll_physics(ScrollPhysics::Ios);
        assert_eq!(config.time_source, TimeSource::Manual);
        assert_eq!(config.safe_area, SafeArea::Ignore);
        assert_eq!(config.scroll_physics, ScrollPhysics::Ios);
    }
}
