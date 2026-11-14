//! Dioxus portals render into overlay hosts appended to `<body>` after `<main>`.

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;

use blitz_dom::{BaseDocument, Node, NodeData, NodeId};
use blitz_test_harness::{Harness, HarnessOptions};
use dioxus::prelude::*;
use dioxus_core::{Runtime, ScopeId};
use dioxus_native::{Portal, RenderTargetId, use_portal_target};
use dioxus_native_dom::DioxusDocument;

/// Writes `node`'s subtree, one node per line, with node ids and attributes in order.
fn write_tree(out: &mut String, doc: &BaseDocument, node: &Node, depth: usize) {
    let indent = "  ".repeat(depth);
    match &node.data {
        NodeData::Element(data) => {
            let attrs: Vec<String> = data
                .attrs
                .iter()
                .map(|attr| format!("{}={:?}", attr.name.local, attr.value))
                .collect();
            writeln!(
                out,
                "{indent}{} <{} {}>",
                node.id,
                data.name.local,
                attrs.join(" ")
            )
            .unwrap();
        }
        NodeData::Text(data) => writeln!(out, "{indent}{} {:?}", node.id, data.content).unwrap(),
        _ => writeln!(out, "{indent}{} #other", node.id).unwrap(),
    }
    for child in &node.children {
        write_tree(out, doc, doc.get_node(*child).unwrap(), depth + 1);
    }
}

fn tree(harness: &Harness<DioxusDocument>) -> String {
    let doc = harness.doc.inner.borrow();
    let mut out = String::new();
    write_tree(&mut out, &doc, doc.root_node(), 0);
    out
}

fn rerender(harness: &mut Harness<DioxusDocument>) {
    harness.doc.vdom.mark_dirty(ScopeId::APP);
    harness.pump();
}

fn with_stage(app: fn() -> Element, stage: &Rc<Cell<usize>>) -> Harness<DioxusDocument> {
    let vdom = VirtualDom::new(app).with_root_context(Rc::clone(stage));
    Harness::from_vdom(vdom, HarnessOptions::default())
}

fn stage() -> usize {
    consume_context::<Rc<Cell<usize>>>().get()
}

/// A shared cell the tests use to read a target id out of a component.
#[derive(Clone, Default)]
struct TargetSlot(Rc<Cell<Option<RenderTargetId>>>);

fn with_stage_and_slot(
    app: fn() -> Element,
    stage: &Rc<Cell<usize>>,
    slot: &TargetSlot,
) -> Harness<DioxusDocument> {
    let vdom = VirtualDom::new(app)
        .with_root_context(Rc::clone(stage))
        .with_root_context(slot.clone());
    Harness::from_vdom(vdom, HarnessOptions::default())
}

fn node_id(harness: &Harness<DioxusDocument>, selector: &str) -> NodeId {
    harness.node(selector)
}

fn parent(harness: &Harness<DioxusDocument>, node: NodeId) -> Option<NodeId> {
    harness.base().get_node(node).and_then(|node| node.parent)
}

fn children(harness: &Harness<DioxusDocument>, node: NodeId) -> Vec<NodeId> {
    harness.base().get_node(node).unwrap().children.to_vec()
}

fn body(harness: &Harness<DioxusDocument>) -> NodeId {
    node_id(harness, "body")
}

#[test]
fn portal_children_render_in_the_overlay_host() {
    fn app() -> Element {
        let target = use_portal_target();
        let mounted = consume_context::<Rc<Cell<usize>>>();
        rsx! {
            div { id: "page", "page" }
            Portal { target,
                div {
                    id: "sheet",
                    onmounted: move |_| mounted.set(mounted.get() + 1),
                    "sheet"
                }
            }
        }
    }

    let mounted = Rc::new(Cell::new(0));
    let harness = with_stage(app, &mounted);

    let main = node_id(&harness, "#main");
    let sheet = node_id(&harness, "#sheet");
    let host = parent(&harness, sheet).expect("the sheet has a host");
    assert_ne!(host, main);
    assert_eq!(parent(&harness, host), Some(body(&harness)));
    assert_eq!(harness.query("#main #sheet"), None);
    assert_eq!(parent(&harness, node_id(&harness, "#page")), Some(main));
    assert_eq!(harness.text_content("#sheet"), "sheet");
    assert_eq!(mounted.get(), 1, "the portal's mounted event fires once");
}

#[test]
fn portal_clicks_never_reach_root_handlers() {
    fn app() -> Element {
        let target = use_portal_target();
        let root_clicks = consume_context::<Rc<Cell<usize>>>();
        rsx! {
            button {
                id: "root",
                onclick: move |_| root_clicks.set(root_clicks.get() + 1),
                "root"
            }
            Portal { target,
                button { id: "portal", onclick: move |_| {}, "portal" }
            }
        }
    }

    let root_clicks = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &root_clicks);
    // Each target numbers its elements from its own arena, so both buttons carry the same id.
    assert_eq!(
        harness.attr("#root", "data-dioxus-id"),
        harness.attr("#portal", "data-dioxus-id")
    );

    harness.click("#portal");
    assert_eq!(root_clicks.get(), 0);
    harness.click("#root");
    assert_eq!(root_clicks.get(), 1);
}

#[test]
fn portal_mouseenter_never_reaches_root_handlers() {
    fn app() -> Element {
        let target = use_portal_target();
        let root_enters = consume_context::<Rc<Cell<usize>>>();
        rsx! {
            div {
                id: "root",
                style: "height: 50px",
                onmouseenter: move |_| root_enters.set(root_enters.get() + 1),
                "root"
            }
            Portal { target,
                div {
                    id: "portal",
                    style: "height: 50px",
                    onmouseenter: move |_| {},
                    "portal"
                }
            }
        }
    }

    let root_enters = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &root_enters);
    // Each target numbers its elements from its own arena, so both elements carry the same id.
    assert_eq!(
        harness.attr("#root", "data-dioxus-id"),
        harness.attr("#portal", "data-dioxus-id")
    );

    // `mouseenter` doesn't bubble, so its chain holds only the entered element.
    let (x, y) = harness.center_of("#portal");
    harness.move_mouse_to(x, y);
    assert_eq!(root_enters.get(), 0);
    let (x, y) = harness.center_of("#root");
    harness.move_mouse_to(x, y);
    assert_eq!(root_enters.get(), 1);
}

#[test]
fn overlay_host_follows_main() {
    fn app() -> Element {
        let first = use_portal_target();
        let second = use_portal_target();
        rsx! {
            div { id: "page" }
            Portal { target: first, div { id: "first" } }
            Portal { target: second, div { id: "second" } }
        }
    }

    let mut harness = Harness::from_component(app);
    let main = node_id(&harness, "#main");
    let first = parent(&harness, node_id(&harness, "#first")).unwrap();
    let second = parent(&harness, node_id(&harness, "#second")).unwrap();
    assert_eq!(
        children(&harness, body(&harness)),
        vec![main, first, second]
    );

    // A host registered by hand goes last, and registering again returns the same host.
    let target = harness.doc.vdom.runtime().create_render_target();
    let host = harness.doc.register_overlay_target(target);
    assert_eq!(harness.doc.register_overlay_target(target), host);
    assert_eq!(
        children(&harness, body(&harness)),
        vec![main, first, second, host]
    );

    harness.doc.remove_target(target);
    assert_eq!(
        children(&harness, body(&harness)),
        vec![main, first, second]
    );
}

#[test]
fn retarget_moves_the_children() {
    fn app() -> Element {
        let first = use_portal_target();
        let second = use_portal_target();
        let target = if stage() == 0 { first } else { second };
        rsx! {
            Portal { target,
                div { id: "sheet", "sheet" }
            }
            Portal { target: first, div { id: "first-anchor" } }
            Portal { target: second, div { id: "second-anchor" } }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    let first = parent(&harness, node_id(&harness, "#first-anchor")).unwrap();
    let second = parent(&harness, node_id(&harness, "#second-anchor")).unwrap();
    assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(first));

    for (next, host) in [(1, second), (0, first), (1, second)] {
        stage.set(next);
        rerender(&mut harness);
        assert_eq!(harness.query_all("#sheet").len(), 1);
        assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(host));
        assert_eq!(harness.text_content("#sheet"), "sheet");
    }
}

#[test]
fn writes_to_an_unknown_target_are_dropped() {
    fn never_registered() -> Element {
        let target = use_hook(|| Runtime::current().create_render_target());
        let stage = stage();
        rsx! {
            div { id: "page", "page {stage}" }
            Portal { target,
                div { id: "lost", onclick: move |_| {}, "lost {stage}" }
                if stage % 2 == 1 {
                    span { id: "extra" }
                }
            }
        }
    }

    fn removed() -> Element {
        let target = use_portal_target();
        consume_context::<TargetSlot>().0.set(Some(target));
        let stage = stage();
        rsx! {
            Portal { target,
                div { id: "sheet", "sheet {stage}" }
                if stage % 2 == 1 {
                    span { id: "extra" }
                }
            }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(never_registered, &stage);
    for next in [1, 2, 3] {
        stage.set(next);
        rerender(&mut harness);
        assert_eq!(harness.text_content("#page"), format!("page {next}"));
        assert_eq!(harness.query("#lost"), None);
        assert_eq!(harness.query("#extra"), None);
    }
    let main = node_id(&harness, "#main");
    assert_eq!(children(&harness, body(&harness)), vec![main]);

    let stage = Rc::new(Cell::new(0));
    let slot = TargetSlot::default();
    let mut harness = with_stage_and_slot(removed, &stage, &slot);
    assert_eq!(harness.text_content("#sheet"), "sheet 0");
    harness.doc.remove_target(slot.0.get().unwrap());
    let main = node_id(&harness, "#main");
    assert_eq!(children(&harness, body(&harness)), vec![main]);
    for next in [1, 2, 3] {
        stage.set(next);
        rerender(&mut harness);
        assert_eq!(harness.query("#sheet"), None);
        assert_eq!(harness.query("#extra"), None);
    }
    assert_eq!(children(&harness, body(&harness)), vec![main]);
}

#[test]
fn host_outlives_its_mounted_children() {
    #[component]
    fn Owner() -> Element {
        let target = use_portal_target();
        consume_context::<TargetSlot>().0.set(Some(target));
        rsx! { div { id: "owner" } }
    }

    #[component]
    fn Content() -> Element {
        let target = consume_context::<TargetSlot>().0.get().unwrap();
        rsx! {
            Portal { target,
                div { id: "sheet", "sheet" }
            }
        }
    }

    fn app() -> Element {
        let stage = stage();
        rsx! {
            if stage == 0 {
                Owner {}
            }
            if stage < 2 {
                Content {}
            }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let slot = TargetSlot::default();
    let mut harness = with_stage_and_slot(app, &stage, &slot);
    let main = node_id(&harness, "#main");
    let host = parent(&harness, node_id(&harness, "#sheet")).unwrap();
    assert_eq!(children(&harness, body(&harness)), vec![main, host]);

    // The owner goes, but the sheet is still mounted in its target: the host stays.
    stage.set(1);
    rerender(&mut harness);
    assert_eq!(harness.query("#owner"), None);
    assert_eq!(children(&harness, body(&harness)), vec![main, host]);
    assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(host));

    // Once the sheet unmounts, the target and then its host go.
    stage.set(2);
    rerender(&mut harness);
    assert_eq!(harness.query("#sheet"), None);
    assert_eq!(children(&harness, body(&harness)), vec![main]);
    let target = slot.0.get().unwrap();
    assert!(
        !harness.doc.vdom.runtime().remove_render_target(target),
        "the target was already removed"
    );
}

#[test]
fn root_only_apps_are_unchanged() {
    fn app() -> Element {
        let stage = stage();
        let items: Vec<usize> = match stage {
            0 => vec![1, 2, 3],
            1 => vec![3, 1, 4, 2],
            _ => vec![4],
        };
        rsx! {
            div { id: "list",
                for item in items {
                    p { key: "{item}", class: "item", onclick: move |_| {}, "item {item}" }
                }
            }
            if stage == 1 {
                span { id: "shown", onmounted: move |_| {}, "stage {stage}" }
            }
            input { r#type: "checkbox", checked: stage == 1 }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    let mut actual = tree(&harness);
    for next in [1, 2, 1] {
        stage.set(next);
        rerender(&mut harness);
        actual.push_str("---\n");
        actual.push_str(&tree(&harness));
    }
    assert_eq!(actual, ROOT_ONLY_BEFORE_PORTALS, "{actual}");
}

/// The tree `root_only_apps_are_unchanged` printed before Dioxus portals were rendered.
const ROOT_ONLY_BEFORE_PORTALS: &str = r#"NodeId(1v1) #other
  NodeId(2v1) <html >
    NodeId(3v1) <head >
    NodeId(4v1) <body >
      NodeId(5v1) <main id="main">
        NodeId(7v1) <div id="list">
          NodeId(11v1) <p class="item" click="<rust func>" data-dioxus-id="5">
            NodeId(12v1) "item 1"
          NodeId(13v1) <p class="item" click="<rust func>" data-dioxus-id="8">
            NodeId(14v1) "item 2"
          NodeId(15v1) <p class="item" click="<rust func>" data-dioxus-id="10">
            NodeId(16v1) "item 3"
        NodeId(9v1) <input type="checkbox">
---
NodeId(1v1) #other
  NodeId(2v1) <html >
    NodeId(3v1) <head >
    NodeId(4v1) <body >
      NodeId(5v1) <main id="main">
        NodeId(7v1) <div id="list">
          NodeId(15v1) <p class="item" click="<rust func>" data-dioxus-id="10">
            NodeId(16v1) "item 3"
          NodeId(11v1) <p class="item" click="<rust func>" data-dioxus-id="5">
            NodeId(12v1) "item 1"
          NodeId(18v1) <p class="item" click="<rust func>" data-dioxus-id="12">
            NodeId(19v1) "item 4"
          NodeId(13v1) <p class="item" click="<rust func>" data-dioxus-id="8">
            NodeId(14v1) "item 2"
        NodeId(21v1) <span id="shown">
          NodeId(22v1) "stage 1"
        NodeId(9v1) <input type="checkbox" checked="true">
---
NodeId(1v1) #other
  NodeId(2v1) <html >
    NodeId(3v1) <head >
    NodeId(4v1) <body >
      NodeId(5v1) <main id="main">
        NodeId(7v1) <div id="list">
          NodeId(18v1) <p class="item" click="<rust func>" data-dioxus-id="12">
            NodeId(19v1) "item 4"
        NodeId(9v1) <input type="checkbox">
---
NodeId(1v1) #other
  NodeId(2v1) <html >
    NodeId(3v1) <head >
    NodeId(4v1) <body >
      NodeId(5v1) <main id="main">
        NodeId(7v1) <div id="list">
          NodeId(22v3) <p class="item" click="<rust func>" data-dioxus-id="14">
            NodeId(21v3) "item 3"
          NodeId(14v3) <p class="item" click="<rust func>" data-dioxus-id="8">
            NodeId(13v3) "item 1"
          NodeId(18v1) <p class="item" click="<rust func>" data-dioxus-id="12">
            NodeId(19v1) "item 4"
          NodeId(12v3) <p class="item" click="<rust func>" data-dioxus-id="5">
            NodeId(11v3) "item 2"
        NodeId(16v3) <span id="shown">
          NodeId(15v3) "stage 1"
        NodeId(9v1) <input type="checkbox" checked="true">
"#;
