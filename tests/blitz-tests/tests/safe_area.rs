//! The safe-area insets reach CSS as `--safe-area-inset-*` custom properties.

use blitz_dom::{BaseDocument, CSSPixel, DocumentConfig, SideOffsets2D};
use blitz_html::HtmlDocument;

const HTML: &str = "<html><body style='margin: 0'>\
    <div id='bar' style='position: absolute; top: var(--safe-area-inset-top); height: 10px'></div>\
    </body></html>";

fn document() -> BaseDocument {
    let mut doc = HtmlDocument::from_html(HTML, DocumentConfig::default()).into_inner();
    doc.resolve(0.0);
    doc
}

fn insets(top: f32) -> SideOffsets2D<f32, CSSPixel> {
    SideOffsets2D::new(top, 0.0, 34.0, 0.0)
}

fn bar_y(doc: &mut BaseDocument) -> f32 {
    doc.resolve(0.0);
    let bar = doc.query_selector("#bar").unwrap().unwrap();
    doc.get_node(bar).unwrap().final_layout().location.y
}

#[test]
fn safe_area_insets_reach_css() {
    let mut doc = document();
    doc.set_safe_area_insets(insets(62.0));
    assert_eq!(bar_y(&mut doc), 62.0);
    assert_eq!(doc.safe_area_insets(), insets(62.0));
}

#[test]
fn changing_insets_keeps_one_rule() {
    let mut doc = document();
    let sheets = doc.useragent_stylesheets().count();
    doc.set_safe_area_insets(insets(62.0));
    assert_eq!(bar_y(&mut doc), 62.0);
    doc.set_safe_area_insets(insets(40.0));
    assert_eq!(bar_y(&mut doc), 40.0);
    assert_eq!(doc.useragent_stylesheets().count(), sheets + 1);
}
