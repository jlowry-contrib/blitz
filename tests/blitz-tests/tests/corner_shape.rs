//! `corner-shape` reshapes the border box, and box shadows follow it.

use anyrender::Scene;
use anyrender::recording::RenderCommand;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use kurbo::{BezPath, Point, Shape};
use std::sync::Arc;

/// The recorded fill paths of the red shadow and the blue background.
fn shadow_and_background(shadow: &str) -> (BezPath, BezPath) {
    let html = format!(
        "<html><body style='margin:0'>\
         <div style='margin:20px; width:100px; height:100px; background:#0000ff; \
         border-radius:20px; corner-shape:bevel; box-shadow:{shadow}'></div>\
         </body></html>"
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(200, 200, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let mut scene = Scene::new();
    paint_scene(&mut scene, &mut doc, 1.0, 200, 200, 0, 0);

    let fill_of = |rgb: [f32; 3]| {
        scene
            .commands
            .iter()
            .find_map(|cmd| match cmd {
                RenderCommand::Fill(fill) => match &fill.brush {
                    anyrender::Paint::Solid(color)
                        if color.components[..3] == rgb[..] && color.components[3] > 0.0 =>
                    {
                        Some(fill.shape.clone())
                    }
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("no fill of {rgb:?} in {:#?}", scene.commands))
    };
    (fill_of([1.0, 0.0, 0.0]), fill_of([0.0, 0.0, 1.0]))
}

/// Whether `path`, closed as a fill closes it, covers `point`.
fn covers(path: &BezPath, point: Point) -> bool {
    let mut closed = path.clone();
    closed.close_path();
    closed.contains(point)
}

#[test]
fn box_shadow_follows_the_corner_shape() {
    let (shadow, background) = shadow_and_background("#f00 6px 6px 0");
    assert_eq!(shadow, background);
    // A bevel cuts the corner along x + y = 20, where a round corner would
    // still cover (8, 8).
    assert!(
        !covers(&background, Point::new(8.0, 8.0)),
        "the border box isn't bevelled"
    );
    assert!(covers(&background, Point::new(12.0, 12.0)));

    // A spread of 6 moves the bevel out by 6, to x + y = 20 - 6 * sqrt(2);
    // a round corner of radius 26 would cover (2, 2).
    let (spread, _) = shadow_and_background("#f00 0 0 0 6px");
    assert!(
        !covers(&spread, Point::new(2.0, 2.0)),
        "the spread isn't bevelled"
    );
    assert!(covers(&spread, Point::new(8.0, 8.0)));
    assert!(covers(&spread, Point::new(-5.0, 50.0)));
}
