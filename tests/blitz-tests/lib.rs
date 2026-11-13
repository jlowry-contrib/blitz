//! Integration tests for Blitz live in the `tests/` directory of this crate.
//!
//! This library holds the helpers they share.

use blitz_dom::BaseDocument;

/// The glass custom properties, as `(name, syntax, initial value)`.
///
/// They mirror the `@property` rules glass-dioxus ships. `--glass-source-offset`
/// is `<length>+` because Stylo's syntax parser rejects the `{2}` multiplier.
const GLASS_PROPERTIES: &[(&str, &str, &str)] = &[
    ("--glass", "none | regular | clear | identity", "none"),
    ("--glass-tint", "<color>", "transparent"),
    ("--glass-interactive", "<number>", "0"),
    ("--glass-container", "none | <length>", "none"),
    ("--glass-id", "<custom-ident>", "none"),
    ("--glass-union", "<custom-ident>", "none"),
    ("--glass-press", "<number>", "0"),
    ("--glass-touch-x", "<length>", "0px"),
    ("--glass-touch-y", "<length>", "0px"),
    ("--glass-materialize", "<number>", "1"),
    ("--glass-x", "<length>", "0px"),
    ("--glass-y", "<length>", "0px"),
    ("--glass-w", "<length>", "0px"),
    ("--glass-h", "<length>", "0px"),
    ("--glass-r", "<length>", "0px"),
    ("--glass-source", "none | <custom-ident>", "none"),
    ("--glass-source-hidden", "<number>", "0"),
    ("--glass-magnify", "<number>", "1"),
    ("--glass-source-offset", "<length>+", "0px 0px"),
    ("--corner-style", "circular | continuous", "circular"),
];

/// Registers the glass custom properties on `doc`, each with `inherits: false`.
///
/// # Panics
///
/// Panics if Stylo rejects a registration.
pub fn register_glass_properties(doc: &mut BaseDocument) {
    for &(name, syntax, initial) in GLASS_PROPERTIES {
        let result = doc.register_custom_property(name, syntax, false, Some(initial));
        assert!(
            matches!(
                result,
                blitz_dom::RegisterCustomPropertyResult::SuccessfullyRegistered
            ),
            "registering {name} failed"
        );
    }
}
