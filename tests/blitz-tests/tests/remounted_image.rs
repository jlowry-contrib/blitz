//! An `img` that unmounts and mounts again still paints its image.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use anyrender::Scene;
use anyrender::recording::RenderCommand;
use blitz_paint::paint_scene;
use blitz_test_harness::{Harness, HarnessOptions};
use blitz_traits::net::{NetHandler, NetProvider, Request};
use dioxus::prelude::*;
use dioxus_core::ScopeId;
use dioxus_native_dom::DioxusDocument;

/// The viewport's side in CSS px.
const VIEW: u32 = 200;

/// Serves `file://` requests at once, on the calling thread.
struct FileNetProvider;

impl NetProvider for FileNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let path = request.url.to_file_path().expect("a file URL");
        let bytes = std::fs::read(path).expect("the test image");
        handler.bytes(request.url.to_string(), bytes.into());
    }
}

fn image_url() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/assets/tall.png"
    );
    let path = std::fs::canonicalize(path).expect("the test image exists");
    format!("file://{}", path.display())
}

fn app() -> Element {
    let shown = consume_context::<Rc<Cell<bool>>>().get();
    let src = image_url();
    rsx! {
        div {
            if shown {
                img { src, style: "display: block; width: 50px; height: 50px;" }
            }
        }
    }
}

fn set_shown(harness: &mut Harness<DioxusDocument>, shown: &Rc<Cell<bool>>, value: bool) {
    shown.set(value);
    harness.doc.vdom.mark_dirty(ScopeId::APP);
    harness.tick(1.0 / 60.0);
}

fn image_draws(harness: &Harness<DioxusDocument>) -> usize {
    let mut scene = Scene::new();
    let mut doc = harness.doc.inner.borrow_mut();
    paint_scene(&mut scene, &mut doc, 1.0, VIEW, VIEW, 0, 0);
    scene
        .commands
        .iter()
        .filter(|command| {
            matches!(command, RenderCommand::Fill(fill) if matches!(fill.brush, anyrender::Paint::Image(_)))
        })
        .count()
}

#[test]
fn remounted_image_still_paints() {
    let shown = Rc::new(Cell::new(true));
    let options = HarnessOptions {
        width: VIEW,
        height: VIEW,
        net_provider: Some(Arc::new(FileNetProvider) as _),
        ..Default::default()
    };
    let vdom = VirtualDom::new(app).with_root_context(Rc::clone(&shown));
    let mut harness = Harness::from_vdom(vdom, options);
    harness.tick(1.0 / 60.0);
    assert_eq!(image_draws(&harness), 1, "the first mount paints its image");

    set_shown(&mut harness, &shown, false);
    assert_eq!(
        image_draws(&harness),
        0,
        "the unmounted image doesn't paint"
    );

    set_shown(&mut harness, &shown, true);
    assert_eq!(image_draws(&harness), 1, "the remounted image paints");
}
