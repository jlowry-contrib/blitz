//! Dioxus portals render into overlay hosts appended to `<body>` after `<main>`, or into
//! outlets bound by key.

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use anyrender::Scene;
use anyrender::recording::RenderCommand;
use blitz_dom::{BaseDocument, DocumentConfig, Node, NodeData, NodeId};
use blitz_html::HtmlProvider;
use blitz_paint::paint_scene;
use blitz_test_harness::{Harness, HarnessOptions};
use blitz_tests::register_glass_properties;
use blitz_traits::shell::{ColorScheme, Viewport};
use dioxus::prelude::*;
use dioxus_core::{Runtime, ScopeId};
use dioxus_native::{Portal, RenderTargetId, use_portal_outlet, use_portal_target};
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

/// Two counters a test's components share: one for ROOT's handlers, one for the portal's.
#[derive(Clone, Default)]
struct Counts {
    root: Rc<Cell<usize>>,
    portal: Rc<Cell<usize>>,
}

impl Counts {
    fn get(&self) -> (usize, usize) {
        (self.root.get(), self.portal.get())
    }
}

fn bump(count: &Rc<Cell<usize>>) {
    count.set(count.get() + 1);
}

fn with_counts(app: fn() -> Element, counts: &Counts) -> Harness<DioxusDocument> {
    let vdom = VirtualDom::new(app).with_root_context(counts.clone());
    Harness::from_vdom(vdom, HarnessOptions::default())
}

#[test]
fn portal_clicks_never_reach_root_handlers() {
    fn app() -> Element {
        let target = use_portal_target();
        let Counts { root, portal } = consume_context::<Counts>();
        rsx! {
            button { id: "root", onclick: move |_| bump(&root), "root" }
            Portal { target,
                button { id: "portal", onclick: move |_| bump(&portal), "portal" }
            }
        }
    }

    let counts = Counts::default();
    let mut harness = with_counts(app, &counts);
    // Each target numbers its elements from its own arena, so both buttons carry the same id.
    assert_eq!(
        harness.attr("#root", "data-dioxus-id"),
        harness.attr("#portal", "data-dioxus-id")
    );

    harness.click("#portal");
    assert_eq!(counts.get(), (0, 1));
    harness.click("#root");
    assert_eq!(counts.get(), (1, 1));
}

#[test]
fn portal_mouseenter_never_reaches_root_handlers() {
    fn app() -> Element {
        let target = use_portal_target();
        let Counts { root, portal } = consume_context::<Counts>();
        rsx! {
            div {
                id: "root",
                style: "height: 50px",
                onmouseenter: move |_| bump(&root),
                "root"
            }
            Portal { target,
                div {
                    id: "portal",
                    style: "height: 50px",
                    onmouseenter: move |_| bump(&portal),
                    "portal"
                }
            }
        }
    }

    let counts = Counts::default();
    let mut harness = with_counts(app, &counts);
    // Each target numbers its elements from its own arena, so both elements carry the same id.
    assert_eq!(
        harness.attr("#root", "data-dioxus-id"),
        harness.attr("#portal", "data-dioxus-id")
    );

    // `mouseenter` doesn't bubble, so its chain holds only the entered element.
    let (x, y) = harness.center_of("#portal");
    harness.move_mouse_to(x, y);
    assert_eq!(counts.get(), (0, 1));
    let (x, y) = harness.center_of("#root");
    harness.move_mouse_to(x, y);
    assert_eq!(counts.get(), (1, 1));
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

/// The viewport's size in CSS px for the tests that record a scene.
const VIEW: u32 = 300;

/// Builds `app` at scale 1 with the glass properties registered before the first render.
fn scene_harness(app: fn() -> Element) -> Harness<DioxusDocument> {
    let config = DocumentConfig {
        viewport: Some(Viewport::new(VIEW, VIEW, 1.0, ColorScheme::Light)),
        html_parser_provider: Some(Arc::new(HtmlProvider) as _),
        ..Default::default()
    };
    let mut doc = DioxusDocument::new(VirtualDom::new(app), config);
    register_glass_properties(&mut doc.inner.borrow_mut());
    doc.initial_build();
    let mut harness = Harness::wrap(doc);
    harness.pump();
    harness
}

fn record(harness: &Harness<DioxusDocument>) -> Scene {
    let mut scene = Scene::new();
    let mut doc = harness.doc.inner.borrow_mut();
    paint_scene(&mut scene, &mut doc, 1.0, VIEW, VIEW, 0, 0);
    scene
}

/// The scene's commands, with each glass group's id, which comes from a `NodeId`, replaced by
/// the order the group first appears in.
fn commands_without_node_ids(scene: &Scene) -> Vec<RenderCommand> {
    let mut ids = Vec::new();
    scene
        .commands
        .iter()
        .cloned()
        .map(|mut command| {
            if let RenderCommand::Glass(group) = &mut command {
                let group = Arc::make_mut(group);
                group.id = group.id.map(|id| {
                    let order = ids.iter().position(|seen| *seen == id).unwrap_or_else(|| {
                        ids.push(id);
                        ids.len() - 1
                    });
                    order as u64
                });
            }
            command
        })
        .collect()
}

/// The translation of the first solid fill of `rgb`.
fn fill_translation(scene: &Scene, rgb: [f32; 3]) -> kurbo::Vec2 {
    scene
        .commands
        .iter()
        .find_map(|command| match command {
            RenderCommand::Fill(fill)
                if matches!(
                    &fill.brush,
                    anyrender::Paint::Solid(color) if color.components[..3] == rgb[..]
                ) =>
            {
                Some(fill.transform.translation())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fill of {rgb:?}"))
}

const PAGE_STYLE: &str = "position: absolute; left: 0px; top: 0px; width: 300px; \
     height: 300px; background: rgb(0, 255, 0)";
const OUTLET_STYLE: &str = "position: absolute; left: 20px; top: 30px; width: 200px; \
     height: 120px; background: rgb(0, 0, 255); --glass-container: 10px";
const LENS_STYLE: &str = "position: absolute; left: 10px; top: 10px; width: 60px; \
     height: 40px; border-radius: 20px; --glass: regular; background: rgba(255, 0, 0, 0.5)";
const BUTTON_STYLE: &str = "position: absolute; left: 80px; top: 10px; width: 60px; \
     height: 40px; --glass: clear; --glass-interactive: 1";

#[test]
fn portal_into_an_outlet_records_the_same_commands_as_inline() {
    fn inline() -> Element {
        rsx! {
            div { id: "page", style: PAGE_STYLE }
            div { id: "outlet", style: OUTLET_STYLE,
                div { id: "lens", style: LENS_STYLE }
                div { id: "button", style: BUTTON_STYLE }
            }
        }
    }

    fn through_a_portal() -> Element {
        let target = use_portal_outlet("sheet");
        rsx! {
            div { id: "page", style: PAGE_STYLE }
            div { id: "outlet", "data-portal-outlet": "sheet", style: OUTLET_STYLE }
            Portal { target,
                div { id: "lens", style: LENS_STYLE }
                div { id: "button", style: BUTTON_STYLE }
            }
        }
    }

    let inline = scene_harness(inline);
    let portal = scene_harness(through_a_portal);
    assert_eq!(
        parent(&portal, node_id(&portal, "#lens")),
        Some(node_id(&portal, "#outlet"))
    );

    let inline = commands_without_node_ids(&record(&inline));
    let portal = commands_without_node_ids(&record(&portal));
    assert!(
        inline
            .iter()
            .any(|command| matches!(command, RenderCommand::Glass(_))),
        "the scene has glass: {inline:#?}"
    );
    assert_eq!(inline, portal);
}

#[test]
fn click_in_a_portal_reaches_its_logical_handler() {
    #[derive(Clone, Default)]
    struct Clicks {
        outlet: Rc<Cell<usize>>,
        logical: Rc<Cell<usize>>,
        twin: Rc<Cell<usize>>,
        portal: Rc<Cell<usize>>,
    }

    impl Clicks {
        fn get(&self) -> [usize; 4] {
            [&self.outlet, &self.logical, &self.twin, &self.portal].map(|count| count.get())
        }
    }

    fn app() -> Element {
        let target = use_portal_outlet("sheet");
        let Clicks {
            outlet,
            logical,
            twin,
            portal,
        } = consume_context::<Clicks>();
        rsx! {
            div {
                id: "outlet",
                "data-portal-outlet": "sheet",
                style: "padding: 20px",
                onclick: move |_| bump(&outlet),
            }
            div { id: "logical", onclick: move |_| bump(&logical),
                button { id: "twin", onclick: move |_| bump(&twin), "twin" }
                Portal { target,
                    button { id: "portal", onclick: move |_| bump(&portal), "portal" }
                }
            }
        }
    }

    let clicks = Clicks::default();
    let vdom = VirtualDom::new(app).with_root_context(clicks.clone());
    let mut harness = Harness::from_vdom(vdom, HarnessOptions::default());
    assert_eq!(
        parent(&harness, node_id(&harness, "#portal")),
        Some(node_id(&harness, "#outlet"))
    );

    // The click bubbles through the portal's logical parents, not the outlet it sits in.
    harness.click("#portal");
    assert_eq!(clicks.get(), [0, 1, 0, 1]);

    // The outlet's own padding belongs to ROOT.
    let outlet = harness.layout_rect("#outlet");
    harness.click_at(outlet.x as f32 + 5.0, outlet.y as f32 + 5.0);
    assert_eq!(clicks.get(), [1, 1, 0, 1]);

    harness.click("#twin");
    assert_eq!(clicks.get(), [1, 2, 1, 1]);
}

#[test]
fn mounted_events_fire_per_target() {
    #[derive(Clone, Default)]
    struct Log(Rc<std::cell::RefCell<Vec<&'static str>>>);

    fn app() -> Element {
        let overlay = use_portal_target();
        let outlet = use_portal_outlet("sheet");
        let log = consume_context::<Log>();
        let mount = move |name: &'static str| {
            let log = log.clone();
            move |_: MountedEvent| log.0.borrow_mut().push(name)
        };
        rsx! {
            div { id: "root", onmounted: mount("root") }
            div { id: "outlet", "data-portal-outlet": "sheet" }
            Portal { target: overlay,
                div { id: "overlay", onmounted: mount("overlay") }
            }
            Portal { target: outlet,
                div { id: "sheet", onmounted: mount("outlet") }
            }
        }
    }

    let log = Log::default();
    let vdom = VirtualDom::new(app).with_root_context(log.clone());
    let harness = Harness::from_vdom(vdom, HarnessOptions::default());
    assert_eq!(
        parent(&harness, node_id(&harness, "#sheet")),
        Some(node_id(&harness, "#outlet"))
    );
    let mut fired = log.0.borrow().clone();
    fired.sort_unstable();
    assert_eq!(fired, ["outlet", "overlay", "root"]);
}

#[test]
fn fixed_child_of_an_overlay_host_does_not_scroll() {
    const PAGE: [f32; 3] = [0.0, 1.0, 0.0];
    const BAR: [f32; 3] = [0.0, 0.0, 1.0];

    fn app() -> Element {
        let target = use_portal_target();
        rsx! {
            div { id: "page", style: "height: 2000px; background: rgb(0, 255, 0)" }
            Portal { target,
                div {
                    id: "bar",
                    style: "position: fixed; left: 0px; bottom: 0px; width: 100px; \
                            height: 40px; background: rgb(0, 0, 255)",
                }
            }
        }
    }

    let mut harness = scene_harness(app);
    let bar = node_id(&harness, "#bar");
    let before = record(&harness);
    assert_eq!(harness.hit_node(50.0, VIEW as f32 - 20.0), bar);

    // A negative delta scrolls the page down, as a wheel does.
    harness.base_mut().scroll_viewport_by(0.0, -300.0);
    harness.pump();
    assert_eq!(harness.base().viewport_scroll().y, 300.0);
    let after = record(&harness);

    assert_eq!(
        fill_translation(&after, PAGE) - fill_translation(&before, PAGE),
        kurbo::Vec2::new(0.0, -300.0),
        "the page scrolls"
    );
    assert_eq!(
        fill_translation(&after, BAR),
        fill_translation(&before, BAR)
    );
    assert_eq!(harness.hit_node(50.0, VIEW as f32 - 20.0), bar);
}

#[test]
fn outlet_appearing_after_the_portal_still_renders() {
    #[component]
    fn Sheet(stage: usize) -> Element {
        let target = use_portal_outlet("sheet");
        rsx! {
            Portal { target,
                div { id: "sheet", "sheet {stage}" }
            }
        }
    }

    fn app() -> Element {
        let stage = stage();
        rsx! {
            if matches!(stage, 1 | 3 | 5) {
                div { id: "outlet", "data-portal-outlet": "sheet" }
            }
            if stage < 4 {
                Sheet { stage }
            }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    let main = node_id(&harness, "#main");
    assert_eq!(harness.query("#sheet"), None, "no outlet yet");
    assert_eq!(children(&harness, body(&harness)), vec![main]);

    // The outlet appears, then goes and comes back while the sheet keeps rendering.
    for next in [1, 2, 3] {
        stage.set(next);
        rerender(&mut harness);
        if next == 2 {
            assert_eq!(harness.query("#outlet"), None);
            assert_eq!(harness.query("#sheet"), None);
        } else {
            let outlet = node_id(&harness, "#outlet");
            assert_eq!(harness.query_all("#sheet").len(), 1);
            assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(outlet));
            assert_eq!(harness.text_content("#sheet"), format!("sheet {next}"));
        }
        assert_eq!(children(&harness, body(&harness)), vec![main]);
    }

    // The sheet's owner drops while the outlet is gone, then the outlet comes back empty.
    for next in [2, 4, 5] {
        stage.set(next);
        rerender(&mut harness);
        assert_eq!(harness.query("#sheet"), None);
    }
    assert_eq!(children(&harness, node_id(&harness, "#outlet")), vec![]);
}

#[test]
fn outlet_waits_for_its_key_and_outlives_it() {
    #[component]
    fn Sheet() -> Element {
        let target = use_portal_outlet("sheet");
        rsx! {
            Portal { target,
                div { id: "sheet", "sheet" }
            }
        }
    }

    fn app() -> Element {
        rsx! {
            div { id: "outlet", "data-portal-outlet": "sheet" }
            if stage() == 1 {
                Sheet {}
            }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    let outlet = node_id(&harness, "#outlet");
    assert_eq!(children(&harness, outlet), vec![]);

    stage.set(1);
    rerender(&mut harness);
    assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(outlet));

    // Unregistering the key leaves the outlet element in place.
    stage.set(2);
    rerender(&mut harness);
    assert_eq!(harness.query("#sheet"), None);
    assert_eq!(node_id(&harness, "#outlet"), outlet);
    assert_eq!(children(&harness, outlet), vec![]);

    stage.set(1);
    rerender(&mut harness);
    assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(outlet));
}

#[test]
fn outlet_content_follows_the_key_between_elements() {
    fn app() -> Element {
        let target = use_portal_outlet("sheet");
        let stage = stage();
        let key = |element: usize| (stage == element).then_some("sheet");
        rsx! {
            div { id: "first", "data-portal-outlet": key(1) }
            div { id: "second", "data-portal-outlet": key(2) }
            Portal { target,
                div { id: "sheet", "sheet {stage}" }
            }
        }
    }

    let stage = Rc::new(Cell::new(1));
    let mut harness = with_stage(app, &stage);
    for (next, element) in [(2, Some("#second")), (0, None), (1, Some("#first"))] {
        stage.set(next);
        rerender(&mut harness);
        match element {
            Some(element) => {
                let element = node_id(&harness, element);
                assert_eq!(parent(&harness, node_id(&harness, "#sheet")), Some(element));
                assert_eq!(harness.text_content("#sheet"), format!("sheet {next}"));
            }
            None => assert_eq!(harness.query("#sheet"), None),
        }
    }
}

#[test]
fn outlet_never_binds_inside_its_own_content() {
    fn app() -> Element {
        let target = use_portal_outlet("sheet");
        let moved = stage() == 1;
        rsx! {
            div { id: "first", "data-portal-outlet": (!moved).then_some("sheet") }
            div { id: "second", "data-portal-outlet": moved.then_some("sheet") }
            Portal { target,
                div { id: "sheet",
                    // Comes before #second in tree order while the sheet sits in #first.
                    div { id: "inner", "data-portal-outlet": moved.then_some("sheet") }
                }
            }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    assert_eq!(
        parent(&harness, node_id(&harness, "#sheet")),
        Some(node_id(&harness, "#first"))
    );
    stage.set(1);
    rerender(&mut harness);
    assert_eq!(
        parent(&harness, node_id(&harness, "#sheet")),
        Some(node_id(&harness, "#second"))
    );
}

#[test]
fn swapping_the_owner_in_one_render_hands_the_key_over() {
    fn page(name: &'static str) -> Element {
        let target = use_portal_outlet("sheet");
        rsx! {
            Portal { target,
                div { id: name, "{name}" }
            }
        }
    }

    // Two component types, so the swap unmounts one owner and mounts the other.
    #[component]
    fn PageA() -> Element {
        page("a")
    }

    #[component]
    fn PageB() -> Element {
        page("b")
    }

    fn app() -> Element {
        rsx! {
            div { id: "outlet", "data-portal-outlet": "sheet" }
            if stage().is_multiple_of(2) {
                PageA {}
            } else {
                PageB {}
            }
        }
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    let outlet = node_id(&harness, "#outlet");
    assert_eq!(children(&harness, outlet), vec![node_id(&harness, "#a")]);

    for (next, shown, gone) in [(1, "#b", "#a"), (2, "#a", "#b"), (3, "#b", "#a")] {
        stage.set(next);
        rerender(&mut harness);
        assert_eq!(harness.query(gone), None);
        assert_eq!(children(&harness, outlet), vec![node_id(&harness, shown)]);
    }
}

#[test]
fn outlet_children_of_its_own_stay_with_root() {
    #[component]
    fn Sheet() -> Element {
        let target = use_portal_outlet("sheet");
        let Counts { portal, .. } = consume_context::<Counts>();
        rsx! {
            Portal { target,
                // A numbered element first, so the button's id matches the ROOT child's.
                span { onclick: move |_| {} }
                button { id: "portal", onclick: move |_| bump(&portal), "portal" }
            }
        }
    }

    fn app() -> Element {
        let Counts { root, .. } = consume_context::<Counts>();
        let stage = consume_context::<Rc<Cell<usize>>>().get();
        rsx! {
            div { id: "outlet", "data-portal-outlet": "sheet",
                button { id: "own", onclick: move |_| bump(&root), "own" }
            }
            if stage != 1 {
                Sheet {}
            }
        }
    }

    let counts = Counts::default();
    let stage = Rc::new(Cell::new(0_usize));
    let vdom = VirtualDom::new(app)
        .with_root_context(counts.clone())
        .with_root_context(Rc::clone(&stage));
    let mut harness = Harness::from_vdom(vdom, HarnessOptions::default());
    let outlet = node_id(&harness, "#outlet");
    let own = node_id(&harness, "#own");
    let content = |harness: &Harness<DioxusDocument>| {
        vec![own, node_id(harness, "span"), node_id(harness, "#portal")]
    };
    assert_eq!(children(&harness, outlet), content(&harness));
    // Each target numbers its elements from its own arena, so both buttons carry the same id.
    assert_eq!(
        harness.attr("#own", "data-dioxus-id"),
        harness.attr("#portal", "data-dioxus-id")
    );

    // The outlet's own child is ROOT's, though it shares the host with the portal's content.
    harness.click("#own");
    assert_eq!(counts.get(), (1, 0));
    harness.click("#portal");
    assert_eq!(counts.get(), (1, 1));

    // The owner drops: its target goes although the outlet still has a child, and the key is
    // free for the next owner.
    stage.set(1);
    rerender(&mut harness);
    assert_eq!(children(&harness, outlet), vec![own]);
    stage.set(2);
    rerender(&mut harness);
    assert_eq!(children(&harness, outlet), content(&harness));
    harness.click("#portal");
    assert_eq!(counts.get(), (1, 2));
}

#[test]
fn moved_outlet_content_keeps_its_order_once() {
    /// Each stage's item order; the key sits on `#first` until stage 3.
    const ORDERS: [[usize; 3]; 5] = [[1, 2, 3], [3, 1, 2], [2, 3, 1], [2, 3, 1], [1, 2, 3]];

    fn app() -> Element {
        let target = use_portal_outlet("sheet");
        let stage = stage();
        let key = |second: bool| (second == (stage == 3)).then_some("sheet");
        rsx! {
            div { id: "first", "data-portal-outlet": key(false) }
            div { id: "second", "data-portal-outlet": key(true) }
            Portal { target,
                if stage >= 1 {
                    div { id: "extra" }
                }
                for item in ORDERS[stage] {
                    div { key: "{item}", id: "item-{item}" }
                }
            }
        }
    }

    fn expected(harness: &Harness<DioxusDocument>, order: [usize; 3]) -> Vec<NodeId> {
        let mut nodes = vec![node_id(harness, "#extra")];
        nodes.extend(order.map(|item| node_id(harness, &format!("#item-{item}"))));
        nodes
    }

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    // Reorders and an insertion before the first item, all inside `#first`.
    for next in [1, 2] {
        stage.set(next);
        rerender(&mut harness);
        let first = node_id(&harness, "#first");
        assert_eq!(children(&harness, first), expected(&harness, ORDERS[next]));
    }

    // The key moves, then moves back with another reorder: the content arrives once, in order.
    for (next, element, other) in [(3, "#second", "#first"), (4, "#first", "#second")] {
        stage.set(next);
        rerender(&mut harness);
        let element = node_id(&harness, element);
        assert_eq!(
            children(&harness, element),
            expected(&harness, ORDERS[next])
        );
        assert_eq!(children(&harness, node_id(&harness, other)), vec![]);
    }
}

/// A portal root that renders nothing while closed, as a sheet does.
#[component]
fn Late(open: bool) -> Element {
    if !open {
        return VNode::empty();
    }
    rsx! {
        div { id: "late", "late" }
    }
}

/// Opens and closes a `Late` portal root twice. The host holds exactly `before`, the content
/// while open, and `after`, so it is empty apart from them once closed.
fn open_and_close_a_placeholder_root(
    app: fn() -> Element,
    host: fn(&Harness<DioxusDocument>) -> NodeId,
    before: &[&str],
    after: &[&str],
) {
    let expected = |harness: &Harness<DioxusDocument>, open: bool| {
        let mut nodes: Vec<NodeId> = before.iter().map(|s| node_id(harness, s)).collect();
        if open {
            nodes.push(node_id(harness, "#late"));
        }
        nodes.extend(after.iter().map(|s| node_id(harness, s)));
        nodes
    };

    let stage = Rc::new(Cell::new(0));
    let mut harness = with_stage(app, &stage);
    assert_eq!(harness.query("#late"), None);
    assert_eq!(
        children(&harness, host(&harness)),
        expected(&harness, false)
    );
    for next in [1, 2, 3, 4] {
        stage.set(next);
        rerender(&mut harness);
        let open = next % 2 == 1;
        assert_eq!(children(&harness, host(&harness)), expected(&harness, open));
        if open {
            assert_eq!(harness.text_content("#late"), "late");
        } else {
            assert_eq!(harness.query("#late"), None);
        }
    }
}

fn overlay_host(harness: &Harness<DioxusDocument>) -> NodeId {
    let body = body(harness);
    *children(harness, body).last().unwrap()
}

fn outlet_host(harness: &Harness<DioxusDocument>) -> NodeId {
    node_id(harness, "#outlet")
}

#[test]
fn placeholder_root_of_an_overlay_portal_is_replaced() {
    fn app() -> Element {
        let target = use_portal_target();
        rsx! {
            div { id: "page" }
            Portal { target,
                Late { open: stage() % 2 == 1 }
            }
        }
    }

    open_and_close_a_placeholder_root(app, overlay_host, &[], &[]);
}

#[test]
fn placeholder_root_of_an_outlet_portal_is_replaced() {
    fn app() -> Element {
        let target = use_portal_outlet("sheet");
        rsx! {
            div { id: "outlet", "data-portal-outlet": "sheet" }
            Portal { target,
                Late { open: stage() % 2 == 1 }
            }
        }
    }

    open_and_close_a_placeholder_root(app, outlet_host, &[], &[]);
}

#[test]
fn placeholder_root_content_follows_an_outlets_own_children() {
    fn app() -> Element {
        let target = use_portal_outlet("sheet");
        rsx! {
            div { id: "outlet", "data-portal-outlet": "sheet",
                span { id: "own", "own" }
            }
            Portal { target,
                Late { open: stage() % 2 == 1 }
            }
        }
    }

    open_and_close_a_placeholder_root(app, outlet_host, &["#own"], &[]);
}

#[test]
fn placeholder_roots_among_others_keep_their_order() {
    fn between() -> Element {
        let target = use_portal_target();
        rsx! {
            Portal { target,
                div { id: "first" }
                Late { open: stage() % 2 == 1 }
                div { id: "last" }
            }
        }
    }

    fn leading() -> Element {
        let target = use_portal_target();
        rsx! {
            Portal { target,
                Late { open: stage() % 2 == 1 }
                div { id: "last" }
            }
        }
    }

    fn trailing() -> Element {
        let target = use_portal_target();
        rsx! {
            Portal { target,
                div { id: "first" }
                Late { open: stage() % 2 == 1 }
            }
        }
    }

    open_and_close_a_placeholder_root(between, overlay_host, &["#first"], &["#last"]);
    open_and_close_a_placeholder_root(leading, overlay_host, &[], &["#last"]);
    open_and_close_a_placeholder_root(trailing, overlay_host, &["#first"], &[]);
}
