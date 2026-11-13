//! The opt-in iOS scroll model: deceleration, rubber band and bounce-back.

use blitz_dom::{Document, DocumentConfig, ScrollBehavior};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_test_harness::{Harness, touch_pointer_event};
use blitz_traits::events::UiEvent;
use blitz_traits::shell::{ColorScheme, ScrollPhysics, Viewport};
use dioxus::prelude::*;
use dioxus_native_dom::DioxusDocument;
use std::cell::Cell;
use std::sync::Arc;

const FRAME: f64 = 1.0 / 60.0;

/// UIScrollView's `normal` deceleration rate, per millisecond.
const DECELERATION_RATE: f64 = 0.998;

/// The rubber band's coefficient, as commonly cited for UIScrollView.
const RUBBER_BAND: f64 = 0.55;

const SCROLLER: &str = r#"<body style="margin:0">
    <div id="scroller" style="width:200px; height:200px; overflow-y:scroll;">
        <div style="height:1000px"></div>
    </div>
</body>"#;

fn config(scroll_physics: ScrollPhysics) -> DocumentConfig {
    DocumentConfig {
        viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
        html_parser_provider: Some(Arc::new(HtmlProvider) as _),
        scroll_physics,
        ..Default::default()
    }
}

fn ios_harness() -> Harness<HtmlDocument> {
    ios_harness_from(SCROLLER)
}

fn ios_harness_from(html: &str) -> Harness<HtmlDocument> {
    let mut harness = Harness::wrap(HtmlDocument::from_html(html, config(ScrollPhysics::Ios)));
    harness.pump();
    harness
}

fn offset<D: Document>(harness: &Harness<D>) -> f64 {
    offset_of(harness, "#scroller")
}

fn offset_of<D: Document>(harness: &Harness<D>, selector: &str) -> f64 {
    let scroller = harness.node(selector);
    harness.base().get_node(scroller).unwrap().scroll_offset().y
}

fn scrollport_height<D: Document>(harness: &Harness<D>) -> f64 {
    let scroller = harness.node("#scroller");
    harness.base().get_node(scroller).unwrap().client_height() as f64
}

/// The displayed offset past an end for an overshoot `x` in a scrollport of extent `d`.
fn rubber_band(x: f64, d: f64) -> f64 {
    d * (1.0 - 1.0 / (x * RUBBER_BAND / d + 1.0))
}

/// The distance a fling released at `v0` px/s covers in `t` seconds.
fn fling_distance(v0: f64, t: f64) -> f64 {
    v0 * (DECELERATION_RATE.powf(1000.0 * t) - 1.0) / (1000.0 * DECELERATION_RATE.ln())
}

/// Pans the content up by 20 px a frame for five frames, then lifts the finger, which
/// releases at 1200 px/s from an offset of 100 px.
fn fling_up<D: Document>(harness: &mut Harness<D>) {
    harness.touch_down(0, 100.0, 150.0);
    for i in 1..=6 {
        harness.tick(FRAME);
        harness.touch_move(0, 100.0, 150.0 - 20.0 * i as f32);
    }
    harness.touch_up(0, 100.0, 30.0);
}

/// Asserts that the scroller no longer moves. (`is_animating` can't tell: the scrollbars'
/// fade keeps it true for a while.)
fn assert_settled<D: Document>(harness: &mut Harness<D>) {
    let rest = offset(harness);
    for _ in 0..10 {
        harness.tick(FRAME);
        assert_eq!(offset(harness), rest);
    }
}

/// Pulls the content down past the top by `overshoot` px in one frame, leaving the finger down.
fn pull_past_top<D: Document>(harness: &mut Harness<D>, overshoot: f32) {
    harness.touch_down(0, 100.0, 20.0);
    // The first move passes the 2 px drag threshold and starts the pan.
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 30.0);
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 30.0 + overshoot);
}

#[test]
fn ios_fling_follows_the_deceleration_law() {
    let mut harness = ios_harness();
    fling_up(&mut harness);
    let release = harness.time();
    let start = offset(&harness);
    assert_eq!(start, 100.0);

    for _ in 0..60 {
        harness.tick(FRAME);
        let t = harness.time() - release;
        let expected = start + fling_distance(1200.0, t);
        let actual = offset(&harness);
        assert!(
            (actual - expected).abs() < 0.01,
            "at t = {t:.4} s the offset is {actual}, expected {expected}"
        );
    }

    // The fling stops once it slows below 0.5 px/s, about 3.9 s after release.
    for _ in 0..240 {
        harness.tick(FRAME);
    }
    assert_settled(&mut harness);
    let rest = offset(&harness);
    assert!((rest - (start + fling_distance(1200.0, f64::INFINITY))).abs() < 0.5);
}

#[test]
fn ios_fling_stops_at_the_next_touch() {
    let mut harness = ios_harness();
    fling_up(&mut harness);
    for _ in 0..10 {
        harness.tick(FRAME);
    }
    harness.touch_down(0, 100.0, 100.0);
    let caught = offset(&harness);
    for _ in 0..10 {
        harness.tick(FRAME);
    }
    assert_eq!(offset(&harness), caught);
    assert_settled(&mut harness);
}

#[test]
fn ios_rubber_band_matches_the_formula_at_three_overshoots() {
    let mut harness = ios_harness();
    let d = scrollport_height(&harness);
    harness.touch_down(0, 100.0, 20.0);
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 30.0);
    for overshoot in [10.0, 100.0, 1000.0] {
        harness.tick(FRAME);
        harness.touch_move(0, 100.0, 30.0 + overshoot as f32);
        let expected = -rubber_band(overshoot, d);
        let actual = offset(&harness);
        assert!(
            (actual - expected).abs() < 1e-9,
            "overshoot {overshoot}: offset {actual}, expected {expected}"
        );
    }

    // The bottom end bands the same way.
    let mut harness = ios_harness();
    let scroller = harness.node("#scroller");
    harness
        .base_mut()
        .scroll_to(scroller, 0.0, 800.0, ScrollBehavior::Instant);
    harness.pump();
    harness.touch_down(0, 100.0, 150.0);
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 140.0);
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 40.0);
    let expected = 800.0 + rubber_band(100.0, d);
    assert!((offset(&harness) - expected).abs() < 1e-9);
}

#[test]
fn ios_release_past_the_end_returns_to_the_end() {
    let mut harness = ios_harness();
    pull_past_top(&mut harness, 100.0);
    assert!(offset(&harness) < -40.0);
    harness.touch_up(0, 100.0, 130.0);

    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert!(
        offset(&harness).abs() < 0.5,
        "after 1 s the offset is {}",
        offset(&harness)
    );

    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert_eq!(offset(&harness), 0.0);
    assert_settled(&mut harness);
}

#[test]
fn ios_touch_during_bounce_holds_then_returns() {
    let mut harness = ios_harness();
    pull_past_top(&mut harness, 100.0);
    harness.touch_up(0, 100.0, 130.0);
    harness.tick(FRAME);

    // A tap catches the content where it is, and lifting the finger bounces it back.
    harness.touch_down(0, 100.0, 100.0);
    let caught = offset(&harness);
    assert!(caught < 0.0);
    harness.tick(FRAME);
    assert_eq!(offset(&harness), caught);
    harness.touch_up(0, 100.0, 100.0);
    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert!(offset(&harness).abs() < 0.5);
}

#[test]
fn default_physics_are_unchanged() {
    // Recorded at the fork's tip before the iOS model landed.
    let expected = vec![
        0.0,
        20.0,
        40.0,
        60.0,
        80.0,
        100.0,
        100.0,
        137.99999693502323,
        174.0999932632951,
        208.3949890531531,
        240.97523436761801,
        271.92646676475454,
        301.3301369230093,
        329.2636229852778,
        355.8004341857628,
        381.01040429548715,
        404.9598753955257,
        427.7118724615727,
        449.3262692192772,
        469.85994570680833,
        489.3669379592893,
        507.89858020900624,
        525.5036399756043,
        542.2284464017712,
        558.1170121721334,
        573.2111493362061,
        587.5505793401923,
        601.1730375571904,
        614.1143725908893,
        626.4086406140765,
        638.0881949902189,
        649.183771413963,
        659.7245687946083,
        669.7383260954052,
        679.2513953308871,
        688.2888109143335,
        696.8743555378592,
        705.0306227584975,
        712.7790764549786,
        720.1401073116665,
        727.1330864782993,
        733.7764165467408,
        740.0875799788936,
        746.0831851132154,
        751.779009870909,
        757.1900432768014,
        762.3305249041784,
        762.3305249041784,
        732.3305249041784,
        702.3305249041784,
        672.3305249041784,
        642.3305249041784,
        612.3305249041784,
        582.3305249041784,
        552.3305249041784,
        522.3305249041784,
        492.33052490417845,
        492.33052490417845,
        423.93053117612897,
        358.95053850248195,
        297.21954676211766,
        238.5751058433923,
        182.86288814349248,
        129.93628244283246,
        79.6560080857383,
        31.88974845210472,
        0.0,
        0.0,
        800.0,
        0.0,
    ];
    let mut harness = Harness::from_html(SCROLLER);
    assert_eq!(run_sequence(&mut harness), expected);
}

/// Pans, flings, overscrolls and wheels, recording the scroller's offset after each step.
fn run_sequence(harness: &mut Harness<HtmlDocument>) -> Vec<f64> {
    let mut offsets = Vec::new();
    harness.touch_down(0, 100.0, 150.0);
    for i in 1..=6 {
        harness.tick(FRAME);
        harness.touch_move(0, 100.0, 150.0 - 20.0 * i as f32);
        offsets.push(offset(harness));
    }
    harness.touch_up(0, 100.0, 30.0);
    offsets.push(offset(harness));
    for _ in 0..40 {
        harness.tick(FRAME);
        offsets.push(offset(harness));
    }

    harness.touch_down(0, 100.0, 20.0);
    for i in 1..=10 {
        harness.tick(FRAME);
        harness.touch_move(0, 100.0, 20.0 + 30.0 * i as f32);
        offsets.push(offset(harness));
    }
    harness.touch_up(0, 100.0, 320.0);
    offsets.push(offset(harness));
    for _ in 0..10 {
        harness.tick(FRAME);
        offsets.push(offset(harness));
    }

    harness.wheel_at(100.0, 100.0, 0.0, -5000.0);
    offsets.push(offset(harness));
    harness.wheel_at(100.0, 100.0, 0.0, 5000.0);
    offsets.push(offset(harness));
    offsets
}

#[test]
fn wheel_scrolls_never_overscroll_under_ios() {
    let mut harness = ios_harness();
    harness.wheel_at(100.0, 100.0, 0.0, -5000.0);
    assert_eq!(offset(&harness), 800.0);
    harness.wheel_at(100.0, 100.0, 0.0, 5000.0);
    assert_eq!(offset(&harness), 0.0);
    for _ in 0..30 {
        harness.tick(FRAME);
        assert_eq!(offset(&harness), 0.0);
    }
    assert_settled(&mut harness);
}

thread_local! {
    static LAST_SCROLL_TOP: Cell<f64> = const { Cell::new(f64::NAN) };
}

fn scroller_app() -> Element {
    rsx! {
        div {
            id: "scroller",
            style: "width:200px; height:200px; overflow-y:scroll;",
            onscroll: move |event| {
                LAST_SCROLL_TOP.with(|top| top.set(event.data().scroll_top()));
            },
            div { style: "height:1000px" }
        }
    }
}

#[test]
fn ios_scroll_events_report_the_displayed_offset() {
    LAST_SCROLL_TOP.with(|top| top.set(f64::NAN));
    let mut doc = DioxusDocument::new(VirtualDom::new(scroller_app), config(ScrollPhysics::Ios));
    doc.initial_build();
    let mut harness = Harness::wrap(doc);
    harness.pump();

    // The body's default 8 px margin keeps these points inside the scroller.
    pull_past_top(&mut harness, 100.0);
    let last = || LAST_SCROLL_TOP.with(Cell::get);
    assert!(offset(&harness) < 0.0);
    assert_eq!(last(), offset(&harness));

    harness.touch_up(0, 100.0, 130.0);
    for _ in 0..5 {
        harness.tick(FRAME);
        assert!(offset(&harness) < 0.0);
        assert_eq!(last(), offset(&harness));
    }
}

#[test]
fn ios_viewport_bands_and_returns() {
    let mut harness = Harness::wrap(HtmlDocument::from_html(
        r#"<body style="margin:0"><div style="height:2000px"></div></body>"#,
        config(ScrollPhysics::Ios),
    ));
    harness.pump();
    harness.touch_down(0, 100.0, 100.0);
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 110.0);
    harness.tick(FRAME);
    harness.touch_move(0, 100.0, 210.0);
    let expected = -rubber_band(100.0, 600.0);
    assert!((harness.base().viewport_scroll().y - expected).abs() < 1e-9);

    harness.touch_up(0, 100.0, 210.0);
    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert!(harness.base().viewport_scroll().y.abs() < 0.5);
}

/// Pulls past the top, releases, and lets the bounce-back run for one frame.
fn start_bounce<D: Document>(harness: &mut Harness<D>) {
    pull_past_top(harness, 100.0);
    harness.touch_up(0, 100.0, 130.0);
    harness.tick(FRAME);
    assert!(offset(harness) < 0.0);
}

#[test]
fn ios_wheel_during_bounce_ends_in_range() {
    let mut harness = ios_harness();
    start_bounce(&mut harness);
    harness.wheel_at(100.0, 100.0, 0.0, -50.0);
    // The bounce ends with the scroller back at the top, and the wheel scrolls on from there.
    assert_eq!(offset(&harness), 50.0);
    assert_settled(&mut harness);
}

#[test]
fn ios_wheel_during_fling_wins() {
    let mut harness = ios_harness();
    fling_up(&mut harness);
    for _ in 0..5 {
        harness.tick(FRAME);
    }
    let flung = offset(&harness);
    harness.wheel_at(100.0, 100.0, 0.0, -50.0);
    assert_eq!(offset(&harness), flung + 50.0);
    assert_settled(&mut harness);
}

#[test]
fn ios_smooth_scroll_elsewhere_during_bounce_ends_in_range() {
    let mut harness = ios_harness_from(
        r#"<body style="margin:0">
            <div id="scroller" style="width:200px; height:200px; overflow-y:scroll;">
                <div style="height:1000px"></div>
            </div>
            <div id="other" style="width:200px; height:200px; overflow-y:scroll;">
                <div style="height:1000px"></div>
            </div>
        </body>"#,
    );
    start_bounce(&mut harness);
    let other = harness.node("#other");
    harness
        .base_mut()
        .scroll_to(other, 0.0, 100.0, ScrollBehavior::Smooth);
    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert_eq!(offset(&harness), 0.0);
    assert_eq!(offset_of(&harness, "#other"), 100.0);
}

const SCROLLER_WITH_INPUT: &str = r#"<body style="margin:0">
    <div id="scroller" style="width:200px; height:200px; overflow-y:scroll;">
        <input style="display:block; width:180px; height:180px;">
        <div style="height:1000px"></div>
    </div>
</body>"#;

#[test]
fn ios_pan_on_an_input_follows_the_ios_laws() {
    let mut harness = ios_harness_from(SCROLLER_WITH_INPUT);
    let d = scrollport_height(&harness);
    pull_past_top(&mut harness, 100.0);
    let expected = -rubber_band(100.0, d);
    assert!(
        (offset(&harness) - expected).abs() < 1e-9,
        "offset {}, expected {expected}",
        offset(&harness)
    );

    let mut harness = ios_harness_from(SCROLLER_WITH_INPUT);
    fling_up(&mut harness);
    let release = harness.time();
    let start = offset(&harness);
    for _ in 0..30 {
        harness.tick(FRAME);
        let expected = start + fling_distance(1200.0, harness.time() - release);
        assert!((offset(&harness) - expected).abs() < 0.01);
    }
}

fn cancel_touch<D: Document>(harness: &mut Harness<D>) {
    let time = harness.time();
    harness.dispatch(UiEvent::PointerCancel(touch_pointer_event(
        0, 100.0, 130.0, time,
    )));
    harness.pump();
}

#[test]
fn ios_pointer_cancel_settles_an_overscrolled_scroller() {
    // A pan cancelled past the end.
    let mut harness = ios_harness();
    pull_past_top(&mut harness, 100.0);
    cancel_touch(&mut harness);
    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert!(offset(&harness).abs() < 0.5);

    // A bounce caught by a touch that is then cancelled.
    let mut harness = ios_harness();
    start_bounce(&mut harness);
    harness.touch_down(0, 100.0, 100.0);
    cancel_touch(&mut harness);
    for _ in 0..60 {
        harness.tick(FRAME);
    }
    assert!(offset(&harness).abs() < 0.5);
}
