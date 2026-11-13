//! Scrolling, scrollbars and click timing follow the frame clock and the
//! events' time stamps, never the wall clock.

use blitz_dom::ScrollBehavior;
use blitz_test_harness::{Harness, HarnessOptions, mouse_pointer_event};
use blitz_traits::events::UiEvent;

const FRAME: f64 = 1.0 / 60.0;

const SCROLLER_HTML: &str = r#"<html><body style="margin:0">
    <div id="scroller" style="width:200px; height:200px; overflow-y:scroll;">
        <div style="height:4000px"></div>
    </div>
</body></html>"#;

fn scroller_harness() -> Harness {
    Harness::from_html_with(
        SCROLLER_HTML,
        HarnessOptions {
            width: 200,
            height: 200,
            ..Default::default()
        },
    )
}

fn scroller_offset(harness: &Harness) -> f64 {
    let base = harness.base();
    let scroller = base
        .query_selector("#scroller")
        .unwrap()
        .expect("#scroller");
    base.get_node(scroller).unwrap().scroll_offset().y
}

/// Drags a finger up the scroller one frame at a time, lifts it, and
/// records the scroll offset after each of the following frames.
fn fling_offsets() -> Vec<f64> {
    let mut harness = scroller_harness();
    harness.touch_down(0, 100.0, 180.0);
    for i in 1..=6 {
        harness.tick(FRAME);
        harness.touch_move(0, 100.0, 180.0 - 20.0 * i as f32);
    }
    harness.touch_up(0, 100.0, 60.0);
    (0..90)
        .map(|_| {
            harness.tick(FRAME);
            scroller_offset(&harness)
        })
        .collect()
}

#[test]
fn fling_is_a_pure_function_of_frame_times() {
    let first = fling_offsets();
    let second = fling_offsets();
    assert_eq!(first, second);
    assert!(
        first[89] > first[0],
        "the fling should keep scrolling after the finger lifts: {first:?}"
    );
}

fn smooth_scroll_offset_at_100ms() -> f64 {
    let mut harness = scroller_harness();
    let scroller = harness
        .base()
        .query_selector("#scroller")
        .unwrap()
        .expect("#scroller");
    harness
        .base_mut()
        .scroll_to(scroller, 0.0, 200.0, ScrollBehavior::Smooth);
    // The animation starts on the first frame that resolves it, here at t 0.
    harness.pump();
    harness.tick(0.1);
    scroller_offset(&harness)
}

#[test]
fn smooth_scroll_progress_follows_the_frame_time() {
    let first = smooth_scroll_offset_at_100ms();
    assert!(
        first > 0.0 && first < 200.0,
        "a third of the way through, the scroll should be in flight: {first}"
    );
    for _ in 0..3 {
        assert_eq!(smooth_scroll_offset_at_100ms(), first);
    }
}

#[test]
fn scrollbar_fade_follows_the_frame_time() {
    let mut harness = scroller_harness();
    let scroller = harness
        .base()
        .query_selector("#scroller")
        .unwrap()
        .expect("#scroller");
    harness
        .base_mut()
        .scroll_by(scroller, 0.0, 50.0, ScrollBehavior::Instant);
    // The fade counts from the first frame that shows the scrollbar, here at t 0.
    harness.pump();

    harness.tick(0.4);
    assert_eq!(harness.base().scrollbar_opacity(scroller), 1.0);

    harness.tick(0.2);
    let fading = harness.base().scrollbar_opacity(scroller);
    assert!(
        fading > 0.0 && fading < 1.0,
        "expected a partial fade: {fading}"
    );

    harness.tick(0.11);
    assert_eq!(harness.base().scrollbar_opacity(scroller), 0.0);
}

/// Activity between frames counts from the next frame, however stale the
/// last frame's time is, as it is on the wall clock after idling.
#[test]
fn activity_after_a_gap_counts_from_the_next_frame() {
    let mut harness = scroller_harness();
    let scroller = harness
        .base()
        .query_selector("#scroller")
        .unwrap()
        .expect("#scroller");

    harness
        .base_mut()
        .scroll_by(scroller, 0.0, 50.0, ScrollBehavior::Instant);
    harness.tick(10.0);
    assert_eq!(harness.base().scrollbar_opacity(scroller), 1.0);

    harness
        .base_mut()
        .scroll_to(scroller, 0.0, 250.0, ScrollBehavior::Smooth);
    harness.tick(10.0);
    assert_eq!(
        scroller_offset(&harness),
        50.0,
        "no progress on its first frame"
    );
    harness.tick(0.1);
    let offset = scroller_offset(&harness);
    assert!(
        offset > 50.0 && offset < 250.0,
        "the scroll should be in flight 0.1 s after its first frame: {offset}"
    );
}

fn click_names(gap: f64) -> Vec<String> {
    let mut harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="target" style="width:100px; height:100px;"></div>
        </body></html>"#,
    );
    let events = [0.0, gap].into_iter().flat_map(|stamp| {
        let event = mouse_pointer_event(50.0, 50.0, stamp);
        [
            UiEvent::PointerDown(event.clone()),
            UiEvent::PointerUp(event),
        ]
    });
    harness.dispatch_recorded(events)
}

#[test]
fn double_click_uses_event_stamps() {
    let quick = click_names(0.2);
    assert!(
        quick.contains(&"dblclick".to_string()),
        "clicks 0.2 s apart should double-click: {quick:?}"
    );
    let slow = click_names(0.6);
    assert!(
        !slow.contains(&"dblclick".to_string()),
        "clicks 0.6 s apart shouldn't double-click: {slow:?}"
    );
}
