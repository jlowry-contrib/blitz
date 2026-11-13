//! Pointer capture retargets a pointer's events to one element. A finger is captured implicitly
//! by the element it pressed, until it lifts or is cancelled; any pointer can be captured
//! explicitly with `BaseDocument::set_pointer_capture`. Hover and `:active` keep following the
//! hit test. Capture ends when the captured element is removed or stops being rendered.

use blitz_dom::{EventDriver, EventHandler, NodeId};
use blitz_test_harness::{Harness, HarnessOptions, mouse_pointer_event, touch_pointer_event};
use blitz_traits::events::{BlitzPointerEvent, BlitzPointerId, DomEvent, EventState, UiEvent};
use std::cell::RefCell;
use std::rc::Rc;

/// `#press` spans y 100..150 and `#other` y 150..350, both from x 0.
const PAGE: &str = r#"<html><body style="margin:0">
    <div id="pad" style="width:200px; height:100px;"></div>
    <div id="press" style="width:100px; height:50px;"></div>
    <div id="other" style="width:200px; height:200px;"></div>
</body></html>"#;

const IN_PRESS: (f32, f32) = (50.0, 125.0);
const IN_OTHER: (f32, f32) = (50.0, 250.0);

fn harness(html: &str) -> Harness {
    Harness::from_html_with(
        html,
        HarnessOptions {
            width: 200,
            height: 400,
            ..Default::default()
        },
    )
}

/// Dispatch `events` through the document's event driver, recording each DOM event's name and
/// target, then pump.
fn record(
    harness: &mut Harness,
    events: impl IntoIterator<Item = UiEvent>,
) -> Vec<(String, NodeId)> {
    #[derive(Default)]
    struct Recorder {
        events: Rc<RefCell<Vec<(String, NodeId)>>>,
    }

    impl EventHandler for Recorder {
        fn handle_event(
            &mut self,
            _chain: &[NodeId],
            event: &mut DomEvent,
            _doc: &mut dyn blitz_dom::Document,
            _event_state: &mut EventState,
        ) {
            self.events
                .borrow_mut()
                .push((event.name().to_string(), event.target));
        }
    }

    let recorder = Recorder::default();
    let recorded = recorder.events.clone();
    let mut doc = harness.base_mut();
    let mut driver = EventDriver::new(&mut *doc, recorder);
    for event in events {
        driver.handle_ui_event(event);
    }
    drop(doc);
    harness.pump();
    recorded.take()
}

fn finger(at: (f32, f32), time_stamp: f64) -> BlitzPointerEvent {
    touch_pointer_event(0, at.0, at.1, time_stamp)
}

fn mouse(at: (f32, f32)) -> BlitzPointerEvent {
    mouse_pointer_event(at.0, at.1, 0.0)
}

/// The targets of the recorded events called `name`.
fn targets(recorded: &[(String, NodeId)], name: &str) -> Vec<NodeId> {
    recorded
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, target)| *target)
        .collect()
}

fn hide(harness: &mut Harness, node: NodeId) {
    harness.base_mut().mutate().set_attribute(
        node,
        blitz_dom::QualName::new(None, blitz_dom::ns!(), blitz_dom::local_name!("style")),
        "display:none",
    );
    harness.pump();
}

#[test]
fn touch_drag_keeps_its_target_outside_the_element() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerDown(finger(IN_PRESS, 0.0)),
            UiEvent::PointerMove(finger((50.0, 200.0), 0.016)),
            UiEvent::PointerMove(finger(IN_OTHER, 0.032)),
        ],
    );
    assert_eq!(targets(&recorded, "pointerdown"), [press]);
    assert_eq!(targets(&recorded, "pointermove"), [press, press]);
    assert_eq!(targets(&recorded, "touchmove"), [press, press]);
    assert_eq!(
        harness.hovered(),
        Some(other),
        "hover follows the hit test, not the capture"
    );

    let recorded = record(&mut harness, [UiEvent::PointerUp(finger(IN_OTHER, 0.048))]);
    assert_eq!(targets(&recorded, "pointerup"), [press]);
    assert_eq!(targets(&recorded, "touchend"), [press]);
}

#[test]
fn mouse_is_not_captured_implicitly() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerDown(mouse(IN_PRESS)),
            UiEvent::PointerMove(mouse(IN_OTHER)),
            UiEvent::PointerUp(mouse(IN_OTHER)),
        ],
    );
    assert_eq!(targets(&recorded, "pointerdown"), [press]);
    assert_eq!(targets(&recorded, "pointermove"), [other]);
    assert_eq!(targets(&recorded, "pointerup"), [other]);
}

#[test]
fn explicit_capture_routes_mouse_moves() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    record(&mut harness, [UiEvent::PointerDown(mouse(IN_PRESS))]);
    harness
        .base_mut()
        .set_pointer_capture(BlitzPointerId::Mouse, press);

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerMove(mouse(IN_OTHER)),
            UiEvent::PointerUp(mouse(IN_OTHER)),
        ],
    );
    assert_eq!(targets(&recorded, "pointermove"), [press]);
    assert_eq!(targets(&recorded, "mousemove"), [press]);
    assert_eq!(targets(&recorded, "pointerup"), [press]);
    assert_eq!(harness.hovered(), Some(other));
}

#[test]
fn explicit_release_ends_the_capture() {
    let mut harness = harness(PAGE);
    let other = harness.node("#other");

    record(&mut harness, [UiEvent::PointerDown(finger(IN_PRESS, 0.0))]);
    harness
        .base_mut()
        .release_pointer_capture(BlitzPointerId::Finger(0));

    let recorded = record(
        &mut harness,
        [UiEvent::PointerMove(finger(IN_OTHER, 0.016))],
    );
    assert_eq!(targets(&recorded, "pointermove"), [other]);
}

#[test]
fn capture_is_released_on_pointerup() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerDown(finger(IN_PRESS, 0.0)),
            UiEvent::PointerUp(finger(IN_PRESS, 0.016)),
            // A later move by the same finger id is hit-tested again.
            UiEvent::PointerMove(finger(IN_OTHER, 0.032)),
        ],
    );
    assert_eq!(targets(&recorded, "pointerup"), [press]);
    assert_eq!(targets(&recorded, "pointermove"), [other]);

    // An explicit mouse capture ends at the mouse's pointerup too.
    record(&mut harness, [UiEvent::PointerDown(mouse(IN_PRESS))]);
    harness
        .base_mut()
        .set_pointer_capture(BlitzPointerId::Mouse, press);
    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerUp(mouse(IN_OTHER)),
            UiEvent::PointerMove(mouse(IN_OTHER)),
        ],
    );
    assert_eq!(targets(&recorded, "pointerup"), [press]);
    assert_eq!(targets(&recorded, "pointermove"), [other]);
}

#[test]
fn capture_is_released_on_pointercancel() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerDown(finger(IN_PRESS, 0.0)),
            UiEvent::PointerCancel(finger(IN_OTHER, 0.016)),
            UiEvent::PointerMove(finger(IN_OTHER, 0.032)),
        ],
    );
    assert_eq!(targets(&recorded, "pointercancel"), [press]);
    assert_eq!(targets(&recorded, "pointermove"), [other]);
}

#[test]
fn capture_is_released_when_the_node_is_removed() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    record(&mut harness, [UiEvent::PointerDown(finger(IN_PRESS, 0.0))]);
    harness.base_mut().mutate().remove_node(press);
    harness.pump();

    // `#other` moved up into `#press`'s place: y 100..300.
    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerMove(finger(IN_OTHER, 0.016)),
            UiEvent::PointerUp(finger(IN_OTHER, 0.032)),
        ],
    );
    assert_eq!(targets(&recorded, "pointermove"), [other]);
    assert_eq!(targets(&recorded, "pointerup"), [other]);
}

#[test]
fn capture_is_released_when_the_node_is_hidden() {
    let mut harness = harness(PAGE);
    let press = harness.node("#press");
    let other = harness.node("#other");

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerDown(finger(IN_PRESS, 0.0)),
            UiEvent::PointerMove(finger((60.0, 125.0), 0.016)),
        ],
    );
    assert_eq!(targets(&recorded, "pointermove"), [press]);

    hide(&mut harness, press);

    let recorded = record(
        &mut harness,
        [
            UiEvent::PointerMove(finger(IN_OTHER, 0.032)),
            UiEvent::PointerUp(finger(IN_OTHER, 0.048)),
        ],
    );
    assert_eq!(targets(&recorded, "pointermove"), [other]);
    assert_eq!(targets(&recorded, "pointerup"), [other]);
}

/// A control with `touch-action: none` and an identity `transform: scale(1)`. A press-and-drag on
/// such an element inside a vertical scroller never scrolls it, and the pressed element keeps
/// every move and the up after the finger leaves it.
#[test]
fn touch_action_none_glass_in_a_scroller_does_not_scroll() {
    let mut harness = harness(
        r#"<html><body style="margin:0">
            <div id="scroller" style="overflow-y:scroll; width:200px; height:300px;">
                <div style="height:100px;"></div>
                <div id="control" style="width:100px; height:40px; touch-action:none; transform:scale(1);"></div>
                <div style="height:600px;"></div>
            </div>
        </body></html>"#,
    );
    let control = harness.node("#control");
    let scroller = harness.node("#scroller");

    // Press inside the control (y 100..140) and drag 100px up, well beyond its top edge, in
    // 10px steps 16ms apart.
    let start = (50.0, 120.0);
    let mut events = vec![UiEvent::PointerDown(finger(start, 0.0))];
    for step in 1..=10 {
        let at = (start.0, start.1 - 10.0 * step as f32);
        events.push(UiEvent::PointerMove(finger(at, 0.016 * f64::from(step))));
    }
    events.push(UiEvent::PointerUp(finger((50.0, 20.0), 0.176)));
    let recorded = record(&mut harness, events);

    let offset = *harness.base().get_node(scroller).unwrap().scroll_offset();
    assert_eq!(
        (offset.x, offset.y),
        (0.0, 0.0),
        "the scroller must not scroll"
    );
    assert_eq!(targets(&recorded, "pointermove"), vec![control; 10]);
    assert_eq!(targets(&recorded, "pointerup"), [control]);
}
