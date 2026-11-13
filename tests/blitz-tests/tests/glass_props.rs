//! The glass custom properties: `--glass` makes a stacking context, and `GlassProps` reads the
//! same values whether or not the properties are registered.

use blitz_dom::glass::{CornerStyleProp, GeometryOverride, GlassProps, GlassVariant};
use blitz_dom::util::Color;
use blitz_dom::{Atom, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_tests::register_glass_properties;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

fn doc(style: &str, child_style: &str, registered: bool) -> HtmlDocument {
    let html = format!(
        r#"<div id="g" style="{style}"><div id="child" style="{child_style}"></div></div>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 200, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    if registered {
        register_glass_properties(&mut doc);
    }
    doc.resolve(0.0);
    doc
}

fn node_props(doc: &HtmlDocument, selector: &str) -> GlassProps {
    let id = doc.query_selector(selector).unwrap().unwrap();
    doc.get_node(id).unwrap().glass_props().unwrap()
}

fn props(style: &str, registered: bool) -> GlassProps {
    node_props(&doc(style, "", registered), "#g")
}

fn is_stacking_context(doc: &HtmlDocument, selector: &str) -> bool {
    let id = doc.query_selector(selector).unwrap().unwrap();
    doc.get_node(id).unwrap().is_stacking_context_root(false)
}

#[test]
fn glass_makes_a_stacking_context() {
    for registered in [false, true] {
        for variant in ["regular", "clear", "identity"] {
            let doc = doc(&format!("--glass: {variant}"), "", registered);
            assert!(
                is_stacking_context(&doc, "#g"),
                "{variant}, registered: {registered}"
            );
        }
    }
}

#[test]
fn glass_none_does_not() {
    for registered in [false, true] {
        for style in ["--glass: none", "", "--glass: bogus"] {
            let doc = doc(style, "", registered);
            assert!(
                !is_stacking_context(&doc, "#g"),
                "{style:?}, registered: {registered}"
            );
        }
    }
}

#[test]
fn glass_is_not_inherited() {
    let style = "--glass: regular; --glass-press: 0.5; --glass-union: bar";
    for registered in [false, true] {
        let doc = doc(style, "", registered);
        assert!(
            !is_stacking_context(&doc, "#child"),
            "registered: {registered}"
        );
        let expected = GlassProps {
            corner_style: registered.then_some(CornerStyleProp::Circular),
            ..GlassProps::default()
        };
        assert_eq!(
            node_props(&doc, "#child"),
            expected,
            "registered: {registered}"
        );
    }
}

#[test]
fn child_redeclaring_glass_is_glass() {
    let cases = [
        (true, "regular", GlassVariant::Regular),
        (true, "clear", GlassVariant::Clear),
        // Unregistered, only a different value can be told apart from an inherited one:
        // Stylo keeps the parent's shared value when a declaration equals it.
        (false, "clear", GlassVariant::Clear),
    ];
    for (registered, value, variant) in cases {
        let doc = doc("--glass: regular", &format!("--glass: {value}"), registered);
        assert!(
            is_stacking_context(&doc, "#child"),
            "{value}, registered: {registered}"
        );
        assert_eq!(
            node_props(&doc, "#child").variant,
            Some(variant),
            "{value}, registered: {registered}"
        );
    }
}

#[test]
fn props_parse_registered_and_unregistered() {
    let style = "--glass: clear; --glass-tint: rgb(255 0 0 / 50%); --glass-interactive: 1; \
        --glass-container: 12px; --glass-id: tab; --glass-union: bar; --glass-press: 0.25; \
        --glass-touch-x: 5px; --glass-touch-y: 7px; --glass-materialize: 0.5; \
        --glass-x: 10px; --glass-y: 20px; --glass-w: 30px; --glass-h: 40px; --glass-r: 6px; \
        --glass-source: hero; --glass-source-hidden: 1; --glass-magnify: 2; \
        --glass-source-offset: 3px 4px; --corner-style: continuous";
    let unregistered = props(style, false);
    let registered = props(style, true);

    let expected = GlassProps {
        variant: Some(GlassVariant::Clear),
        tint: Some(Color::new([1.0, 0.0, 0.0, 0.5])),
        interactive: true,
        container: Some(12.0),
        union: Some(Atom::from("bar")),
        press: 0.25,
        touch: Some((5.0, 7.0)),
        materialize: 0.5,
        geometry: Some(GeometryOverride {
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 40.0,
            r: 6.0,
        }),
        source: Some(Atom::from("hero")),
        source_hidden: true,
        magnify: 2.0,
        source_offset: (3.0, 4.0),
        corner_style: Some(CornerStyleProp::Continuous),
        ..GlassProps::default()
    };

    assert_eq!(unregistered, expected);
    assert_eq!(registered, expected);
}

#[test]
fn props_default_when_absent() {
    assert_eq!(props("", false), GlassProps::default());

    // A registered `--corner-style` always has a value: its initial `circular`.
    let registered_default = GlassProps {
        corner_style: Some(CornerStyleProp::Circular),
        ..GlassProps::default()
    };
    assert_eq!(props("", true), registered_default);

    let defaults = GlassProps::default();
    assert_eq!(defaults.variant, None);
    assert_eq!(defaults.container, None);
    assert_eq!(defaults.press, 0.0);
    assert_eq!(defaults.materialize, 1.0);
    assert_eq!(defaults.magnify, 1.0);
    assert_eq!(defaults.source_offset, (0.0, 0.0));
    assert!(defaults.invalid.is_empty());
}

#[test]
fn bad_value_is_reported_and_defaulted() {
    let unregistered = props("--glass: regular; --glass-press: red", false);
    assert_eq!(unregistered.press, 0.0);
    assert_eq!(unregistered.variant, Some(GlassVariant::Regular));
    assert_eq!(unregistered.invalid.as_slice(), ["--glass-press"]);

    let unregistered = props("--glass-tint: 12px; --glass-magnify: big", false);
    assert_eq!(unregistered.tint, None);
    assert_eq!(unregistered.magnify, 1.0);
    assert_eq!(
        unregistered.invalid.as_slice(),
        ["--glass-tint", "--glass-magnify"]
    );

    // A registered property rejects the declaration in the cascade, so it reads its initial
    // value and nothing is reported.
    let registered = props(
        "--glass: regular; --glass-press: red; --glass-tint: 12px",
        true,
    );
    assert_eq!(registered.press, 0.0);
    assert_eq!(registered.tint, None);
    assert!(registered.invalid.is_empty());
}

#[test]
fn source_offset_takes_two_lengths() {
    for registered in [false, true] {
        let offset = |value: &str| {
            props(&format!("--glass-source-offset: {value}"), registered).source_offset
        };
        assert_eq!(offset("10px"), (10.0, 0.0), "registered: {registered}");
        assert_eq!(
            offset("10px 20px"),
            (10.0, 20.0),
            "registered: {registered}"
        );
        assert_eq!(
            offset("10px 20px 30px"),
            (10.0, 20.0),
            "registered: {registered}"
        );
    }
}

#[test]
fn overrides_need_a_positive_size() {
    for registered in [false, true] {
        let geometry = |style: &str| props(style, registered).geometry;
        assert_eq!(
            geometry("--glass-x: 1px; --glass-y: 2px; --glass-w: 0px; --glass-h: 4px"),
            None
        );
        assert_eq!(
            geometry("--glass-x: 1px; --glass-y: 2px; --glass-w: 3px; --glass-h: 0px"),
            None
        );
        assert_eq!(
            geometry("--glass-w: 3px; --glass-h: 4px"),
            Some(GeometryOverride {
                x: 0.0,
                y: 0.0,
                w: 3.0,
                h: 4.0,
                r: 0.0,
            })
        );
    }
}

#[test]
fn container_none_is_not_a_container() {
    for registered in [false, true] {
        let container =
            |value: &str| props(&format!("--glass-container: {value}"), registered).container;
        assert_eq!(container("none"), None, "registered: {registered}");
        assert_eq!(container("0px"), Some(0.0), "registered: {registered}");
        assert_eq!(container("12px"), Some(12.0), "registered: {registered}");
    }
}

#[test]
fn transparent_tint_is_no_tint() {
    for registered in [false, true] {
        for tint in ["transparent", "rgb(0 0 255 / 0)"] {
            let props = props(&format!("--glass-tint: {tint}"), registered);
            assert_eq!(props.tint, None, "{tint}, registered: {registered}");
        }
    }
}

#[test]
fn currentcolor_tint_is_the_element_color() {
    for registered in [false, true] {
        let props = props(
            "color: rgb(0 0 255); --glass-tint: currentcolor",
            registered,
        );
        assert_eq!(
            props.tint,
            Some(Color::new([0.0, 0.0, 1.0, 1.0])),
            "registered: {registered}"
        );
    }
}

#[test]
fn magnify_not_positive_reads_as_one() {
    for registered in [false, true] {
        for value in ["0", "-2"] {
            let props = props(&format!("--glass-magnify: {value}"), registered);
            assert_eq!(props.magnify, 1.0, "{value}, registered: {registered}");
        }
    }
}

#[test]
fn touch_needs_a_press() {
    for registered in [false, true] {
        let touch = |style: &str| props(style, registered).touch;
        assert_eq!(
            touch("--glass-press: 0; --glass-touch-x: 5px; --glass-touch-y: 7px"),
            None
        );
        assert_eq!(
            touch("--glass-press: 0.5; --glass-touch-x: 0px; --glass-touch-y: 0px"),
            Some((0.0, 0.0))
        );
    }
}

#[test]
fn press_and_materialize_are_clamped() {
    for registered in [false, true] {
        let props = props("--glass-press: 2; --glass-materialize: -1", registered);
        assert_eq!(props.press, 1.0, "registered: {registered}");
        assert_eq!(props.materialize, 0.0, "registered: {registered}");
        assert!(props.invalid.is_empty(), "registered: {registered}");
    }
}

#[test]
fn union_none_is_no_union() {
    for registered in [false, true] {
        let none = props("--glass: regular; --glass-union: none", registered);
        assert_eq!(none.union, None, "registered: {registered}");
        assert!(none.invalid.is_empty(), "registered: {registered}");
        let named = props("--glass: regular; --glass-union: bar", registered);
        assert_eq!(
            named.union,
            Some(Atom::from("bar")),
            "registered: {registered}"
        );
    }
}
