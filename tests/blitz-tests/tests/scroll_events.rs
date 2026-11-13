//! Animated scrolling (touch flings and smooth scrolls) dispatches `scroll`
//! events once per frame for each scroller whose offset moved.

use blitz_dom::ScrollBehavior;
use blitz_test_harness::Harness;
use blitz_traits::events::DomEventData;
use dioxus::prelude::*;
use dioxus_native_dom::DioxusDocument;
use std::cell::Cell;

const FRAME: f64 = 1.0 / 60.0;

thread_local! {
    static SCROLL_EVENTS: Cell<usize> = const { Cell::new(0) };
    static LAST_SCROLL_TOP: Cell<f64> = const { Cell::new(f64::NAN) };
}

fn reset_scroll_events() {
    SCROLL_EVENTS.with(|count| count.set(0));
    LAST_SCROLL_TOP.with(|top| top.set(f64::NAN));
}

fn scroll_events() -> usize {
    SCROLL_EVENTS.with(Cell::get)
}

fn last_scroll_top() -> f64 {
    LAST_SCROLL_TOP.with(Cell::get)
}

fn scroller_app() -> Element {
    rsx! {
        div {
            id: "scroller",
            style: "width:200px; height:200px; overflow-y:scroll;",
            onscroll: move |event| {
                SCROLL_EVENTS.with(|count| count.set(count.get() + 1));
                LAST_SCROLL_TOP.with(|top| top.set(event.data().scroll_top()));
            },
            div { style: "height:4000px" }
        }
    }
}

fn scroller_harness() -> Harness<DioxusDocument> {
    reset_scroll_events();
    Harness::from_component(scroller_app)
}

fn scroller_offset(harness: &Harness<DioxusDocument>) -> f64 {
    let base = harness.base();
    let scroller = base
        .query_selector("#scroller")
        .unwrap()
        .expect("#scroller");
    base.get_node(scroller).unwrap().scroll_offset().y
}

/// Ticks `frames` frames, returning how many of them moved the scroller.
fn tick_counting_moves(harness: &mut Harness<DioxusDocument>, frames: usize) -> usize {
    let mut moved = 0;
    let mut offset = scroller_offset(harness);
    for _ in 0..frames {
        harness.tick(FRAME);
        let next = scroller_offset(harness);
        if next != offset {
            moved += 1;
        }
        offset = next;
    }
    moved
}

fn start_smooth_scroll(harness: &mut Harness<DioxusDocument>) {
    let scroller = harness.node("#scroller");
    harness
        .base_mut()
        .scroll_to(scroller, 0.0, 200.0, ScrollBehavior::Smooth);
}

#[test]
fn fling_dispatches_scroll_events_each_frame() {
    let mut harness = scroller_harness();
    // The body's default 8 px margin keeps these points inside the scroller.
    harness.touch_down(0, 100.0, 180.0);
    for i in 1..=6 {
        harness.tick(FRAME);
        harness.touch_move(0, 100.0, 180.0 - 20.0 * i as f32);
    }
    harness.touch_up(0, 100.0, 60.0);
    reset_scroll_events();

    let moved = tick_counting_moves(&mut harness, 10);

    assert!(moved > 0, "the fling should move the scroller");
    assert_eq!(scroll_events(), moved);
    assert_eq!(last_scroll_top(), scroller_offset(&harness));
}

#[test]
fn smooth_scroll_dispatches_scroll_events() {
    let mut harness = scroller_harness();
    start_smooth_scroll(&mut harness);

    // The smooth scroll lasts 300 ms, so 30 frames take it to its end.
    let moved = tick_counting_moves(&mut harness, 30);

    assert!(moved > 0, "the smooth scroll should move the scroller");
    assert_eq!(scroller_offset(&harness), 200.0);
    assert_eq!(scroll_events(), moved);
    assert_eq!(last_scroll_top(), 200.0);
}

#[test]
fn settled_scroller_dispatches_nothing() {
    let mut harness = scroller_harness();
    tick_counting_moves(&mut harness, 10);
    assert_eq!(scroll_events(), 0, "a scroller at rest dispatches nothing");

    start_smooth_scroll(&mut harness);
    let moved = tick_counting_moves(&mut harness, 30);
    assert_eq!(scroll_events(), moved);

    tick_counting_moves(&mut harness, 10);
    assert_eq!(
        scroll_events(),
        moved,
        "a scroller whose animation ended dispatches nothing"
    );
}

#[test]
fn scroll_events_coalesce_per_scroller_until_the_flush() {
    let mut harness = Harness::from_html(
        r#"<div id="scroller" style="width:200px; height:200px; overflow-y:scroll;">
            <div style="height:4000px"></div>
        </div>"#,
    );
    let scroller = harness.node("#scroller");
    let mut base = harness.base_mut();
    base.scroll_to(scroller, 0.0, 50.0, ScrollBehavior::Instant);
    base.scroll_to(scroller, 0.0, 120.0, ScrollBehavior::Instant);

    let events = base.take_queued_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].target, scroller);
    let DomEventData::Scroll(scroll) = &events[0].data else {
        panic!("expected a scroll event, got {:?}", events[0].data);
    };
    assert_eq!(scroll.scroll_top, 120.0);
    assert!(base.take_queued_events().is_empty());
}
