//! An element with `--glass` emits one glass group, just before its background, in the
//! element's border-box CSS px.

use anyrender::glass::{
    BackdropSource, CornerStyle, GlassGroup, GlassShape, GlassVariant, ShapeKind,
};
use anyrender::recording::RenderCommand;
use anyrender::{Glyph, NormalizedCoord, PaintRef, PaintScene, RenderContext, Scene};
use blitz_dom::{DocumentConfig, NodeId, ScrollBehavior};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_tests::register_glass_properties;
use blitz_traits::shell::{ColorScheme, Viewport};
use kurbo::{Affine, Point, Rect, RoundedRectRadii, Shape, Stroke, Vec2};
use peniko::{BlendMode, Color, Fill, FontData, StyleRef};
use std::sync::Arc;

/// Every page paints at 3×, as the iPhone target does.
const SCALE: f64 = 3.0;
/// The viewport's size in CSS px.
const VIEW: u32 = 200;

fn doc_with(body: &str, registered: bool) -> HtmlDocument {
    let html = format!("<html><body style='margin:0'>{body}</body></html>");
    let size = (f64::from(VIEW) * SCALE) as u32;
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(size, size, SCALE as f32, ColorScheme::Light)),
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

fn doc(body: &str) -> HtmlDocument {
    doc_with(body, true)
}

fn record(doc: &mut HtmlDocument) -> Scene {
    let size = (f64::from(VIEW) * SCALE) as u32;
    let mut scene = Scene::new();
    paint_scene(&mut scene, doc, SCALE, size, size, 0, 0);
    scene
}

fn scene_for(body: &str) -> Scene {
    record(&mut doc(body))
}

fn groups(scene: &Scene) -> Vec<&GlassGroup> {
    scene
        .commands
        .iter()
        .filter_map(|cmd| match cmd {
            RenderCommand::Glass(group) => Some(&**group),
            _ => None,
        })
        .collect()
}

/// The one shape of the scene's one glass group.
fn only_shape(scene: &Scene) -> GlassShape {
    let groups = groups(scene);
    assert_eq!(groups.len(), 1, "glass groups: {groups:#?}");
    assert_eq!(groups[0].shapes.len(), 1);
    groups[0].shapes[0].clone()
}

fn shape_of(style: &str) -> GlassShape {
    only_shape(&scene_for(&format!("<div style='{style}'></div>")))
}

/// The index of the first solid fill of `rgb`.
fn fill_index(scene: &Scene, rgb: [f32; 3]) -> usize {
    scene
        .commands
        .iter()
        .position(|cmd| is_fill_of(cmd, rgb))
        .unwrap_or_else(|| panic!("no fill of {rgb:?} in {:#?}", scene.commands))
}

fn is_fill_of(cmd: &RenderCommand, rgb: [f32; 3]) -> bool {
    match cmd {
        RenderCommand::Fill(fill) => matches!(
            &fill.brush,
            anyrender::Paint::Solid(color)
                if color.components[..3] == rgb[..] && color.components[3] > 0.0
        ),
        _ => false,
    }
}

fn glass_index(scene: &Scene) -> usize {
    scene
        .commands
        .iter()
        .position(|cmd| matches!(cmd, RenderCommand::Glass(_)))
        .expect("no glass")
}

fn node_id(doc: &HtmlDocument, selector: &str) -> NodeId {
    doc.query_selector(selector).unwrap().unwrap()
}

fn assert_near(a: Point, b: Point) {
    assert!((a - b).hypot() < 1e-6, "{a:?} != {b:?}");
}

const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
const GREEN: [f32; 3] = [0.0, 1.0, 0.0];

#[test]
fn glass_element_emits_one_group_before_its_background() {
    let mut doc = doc("<div id='g' style='margin:10px; width:100px; height:40px; \
         --glass:regular; background:#0000ff'>\
         <div style='width:10px; height:10px; background:#00ff00'></div></div>");
    let scene = record(&mut doc);

    let groups = groups(&scene);
    assert_eq!(groups.len(), 1);
    let group = groups[0];
    assert_eq!(group.id, Some(node_id(&doc, "#g").as_u64()));
    assert_eq!(group.alpha, 1.0);
    assert_eq!(group.merge_spacing, 0.0);
    assert!(group.unions.is_empty());
    assert_eq!(group.source, BackdropSource::Behind);
    assert_eq!(group.shapes.len(), 1);

    let glass = glass_index(&scene);
    let background = fill_index(&scene, BLUE);
    let child = fill_index(&scene, GREEN);
    assert!(
        glass < background && background < child,
        "glass {glass}, background {background}, child {child}"
    );
    assert_eq!(glass + 1, background, "{:#?}", scene.commands);
}

#[test]
fn shape_is_the_border_box_in_device_px() {
    let scene = scene_for(
        "<div style='margin:7px 0 0 13px; width:100px; height:40px; --glass:regular; \
         background:#0000ff; transform:translate(5px, 3px) rotate(10deg)'></div>",
    );
    let shape = only_shape(&scene);
    let rect = Rect::new(0.0, 0.0, 100.0, 40.0);
    assert_eq!(
        shape.kind,
        ShapeKind::RoundedRect {
            rect,
            radii: RoundedRectRadii::from_single_radius(0.0),
            corner: CornerStyle::Circular,
        }
    );

    let RenderCommand::Fill(fill) = &scene.commands[fill_index(&scene, BLUE)] else {
        unreachable!()
    };
    let device = fill.shape.bounding_box();
    assert_eq!(device, Rect::new(0.0, 0.0, 300.0, 120.0));
    let to_device = Affine::scale(SCALE) * shape.transform;
    for (css, px) in [
        (Point::new(0.0, 0.0), Point::new(0.0, 0.0)),
        (Point::new(100.0, 0.0), Point::new(300.0, 0.0)),
        (Point::new(100.0, 40.0), Point::new(300.0, 120.0)),
        (Point::new(0.0, 40.0), Point::new(0.0, 120.0)),
    ] {
        assert_near(to_device * css, fill.transform * px);
    }
}

#[test]
fn capsule_and_ellipse_are_recognised() {
    let base = "width:100px; --glass:regular;";
    assert_eq!(
        shape_of(&format!("{base} height:100px; border-radius:50%")).kind,
        ShapeKind::Ellipse(Rect::new(0.0, 0.0, 100.0, 100.0))
    );
    assert_eq!(
        shape_of(&format!("{base} height:40px; border-radius:20px")).kind,
        ShapeKind::Capsule(Rect::new(0.0, 0.0, 100.0, 40.0))
    );
    assert_eq!(
        shape_of(&format!("{base} height:40px; border-radius:50%")).kind,
        ShapeKind::Ellipse(Rect::new(0.0, 0.0, 100.0, 40.0))
    );
    assert_eq!(
        shape_of(&format!("{base} height:40px; border-radius:10px")).kind,
        ShapeKind::RoundedRect {
            rect: Rect::new(0.0, 0.0, 100.0, 40.0),
            radii: RoundedRectRadii::from_single_radius(10.0),
            corner: CornerStyle::Circular,
        }
    );
}

#[test]
fn huge_radius_clamps_to_a_capsule() {
    assert_eq!(
        shape_of("width:100px; height:40px; border-radius:9999px; --glass:regular").kind,
        ShapeKind::Capsule(Rect::new(0.0, 0.0, 100.0, 40.0))
    );
    assert_eq!(
        shape_of("width:40px; height:40px; border-radius:9999px; --glass:regular").kind,
        ShapeKind::Ellipse(Rect::new(0.0, 0.0, 40.0, 40.0))
    );
}

#[test]
fn elliptical_radii_take_the_smaller_radius() {
    let rect = Rect::new(0.0, 0.0, 100.0, 60.0);
    assert_eq!(
        shape_of("width:100px; height:60px; border-radius:20px / 10px; --glass:regular").kind,
        ShapeKind::RoundedRect {
            rect,
            radii: RoundedRectRadii::from_single_radius(10.0),
            corner: CornerStyle::Circular,
        }
    );
    assert_eq!(
        shape_of(
            "width:100px; height:60px; border-top-left-radius:12px 30px; \
             border-bottom-right-radius:8px; --glass:regular"
        )
        .kind,
        ShapeKind::RoundedRect {
            rect,
            radii: RoundedRectRadii::new(12.0, 0.0, 8.0, 0.0),
            corner: CornerStyle::Circular,
        }
    );
}

#[test]
fn corner_style_reaches_the_shape() {
    let shape = shape_of(
        "width:100px; height:60px; border-radius:12px; --glass:regular; \
         --corner-style:continuous",
    );
    let ShapeKind::RoundedRect { corner, .. } = shape.kind else {
        panic!("{:?}", shape.kind)
    };
    assert_eq!(corner, CornerStyle::Continuous);
}

#[test]
fn tint_and_interactive_reach_the_material() {
    let plain = shape_of("width:100px; height:40px; --glass:regular").material;
    assert_eq!(plain.variant, GlassVariant::Regular);
    assert_eq!(plain.tint, None);
    assert!(!plain.interactive);

    let material = shape_of(
        "width:100px; height:40px; --glass:clear; --glass-tint:rgb(255 0 0 / 0.5); \
         --glass-interactive:1",
    )
    .material;
    assert_eq!(material.variant, GlassVariant::Clear);
    let tint = material.tint.expect("a tint");
    assert_eq!(tint.components, [1.0, 0.0, 0.0, 0.5]);
    assert!(material.interactive);

    let identity = shape_of("width:100px; height:40px; --glass:identity").material;
    assert_eq!(identity.variant, GlassVariant::Identity);
}

#[test]
fn glass_inside_opacity_is_inside_the_layer() {
    let scene = scene_for(
        "<div style='width:100px; height:40px; opacity:0.5; --glass:regular; \
         background:#0000ff'></div>",
    );
    let glass = glass_index(&scene);
    let push = scene
        .commands
        .iter()
        .position(|cmd| matches!(cmd, RenderCommand::PushLayer(layer) if layer.alpha == 0.5))
        .expect("an opacity layer");
    assert!(push < glass, "push {push}, glass {glass}");
    let mut depth = 0;
    let pop = scene.commands[push..]
        .iter()
        .position(|cmd| {
            match cmd {
                RenderCommand::PushLayer(_) | RenderCommand::PushClipLayer(_) => depth += 1,
                RenderCommand::PopLayer => depth -= 1,
                _ => {}
            }
            depth == 0
        })
        .map(|i| i + push)
        .expect("the opacity layer's pop");
    assert!(glass < pop, "glass {glass}, pop {pop}");
    assert_eq!(groups(&scene)[0].alpha, 1.0);
}

#[test]
fn glass_drops_its_own_backdrop_filter() {
    let style = "width:100px; height:40px; backdrop-filter:blur(4px)";
    let plain = scene_for(&format!("<div style='{style}'></div>"));
    assert!(has_backdrop_filter(&plain));

    let glass = scene_for(&format!("<div style='{style}; --glass:regular'></div>"));
    assert_eq!(groups(&glass).len(), 1);
    assert!(!has_backdrop_filter(&glass), "{:#?}", glass.commands);
}

#[test]
fn scrolled_glass_moves_with_its_scroller() {
    let mut doc = doc(
        "<div id='s' style='width:100px; height:100px; overflow:auto'>\
         <div style='height:30px'></div>\
         <div style='width:80px; height:40px; --glass:regular'></div>\
         <div style='height:500px'></div></div>",
    );
    let before = only_shape(&record(&mut doc));
    assert_eq!(before.transform.translation(), Vec2::new(0.0, 30.0));

    let scroller = node_id(&doc, "#s");
    doc.scroll_by(scroller, 0.0, 20.0, ScrollBehavior::Instant);
    doc.resolve(0.0);
    let after = only_shape(&record(&mut doc));
    let moved = after.transform.translation();
    assert!(
        (moved - Vec2::new(0.0, 10.0)).hypot() < 1e-9,
        "translation {moved:?}"
    );
}

#[test]
fn identity_transform_emits_the_same_glass() {
    let page = |transform: &str| {
        scene_for(&format!(
            "<div style='margin:5px; width:100px; height:40px; border-radius:9999px; \
             --glass:regular; --glass-interactive:1; background:#0000ff; {transform}'>\
             <div style='width:10px; height:10px; background:#00ff00'></div></div>"
        ))
    };
    let plain = page("");
    let scaled = page("transform:scale(1)");
    assert_eq!(groups(&plain).len(), 1);
    assert_eq!(plain.commands, scaled.commands);
}

#[test]
fn zero_size_glass_is_skipped() {
    for size in ["width:0; height:40px", "width:100px; height:0"] {
        let mut doc = doc(&format!(
            "<div style='{size}; --glass:regular'></div>\
             <div id='kept' style='width:100px; height:40px; --glass:regular'></div>"
        ));
        let scene = record(&mut doc);
        let groups = groups(&scene);
        assert_eq!(groups.len(), 1, "{size}");
        assert_eq!(
            groups[0].id,
            Some(node_id(&doc, "#kept").as_u64()),
            "{size}"
        );
    }
}

#[test]
fn non_invertible_glass_is_skipped() {
    let mut doc = doc("<div style='width:100px; height:40px; --glass:regular; \
         transform:scale(0)'></div>\
         <div id='kept' style='width:100px; height:40px; --glass:regular'></div>");
    let scene = record(&mut doc);
    let groups = groups(&scene);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, Some(node_id(&doc, "#kept").as_u64()));
}

#[test]
fn non_glass_pages_record_the_same_commands() {
    let page = |extra: &str| {
        format!(
            "<div style='margin:5px; width:100px; height:40px; border-radius:12px; \
             opacity:0.5; backdrop-filter:blur(4px); background:#0000ff; {extra}'>\
             <div style='width:10px; height:10px; background:#00ff00'></div></div>"
        )
    };
    let reference = record(&mut doc_with(&page(""), false));
    assert!(groups(&reference).is_empty());
    for (extra, registered) in [
        ("", true),
        ("--glass:none", true),
        ("--glass:none", false),
        ("--glass:bogus", false),
    ] {
        let scene = record(&mut doc_with(&page(extra), registered));
        assert_eq!(
            scene.commands, reference.commands,
            "{extra:?}, registered: {registered}"
        );
    }
}

fn has_backdrop_filter(scene: &Scene) -> bool {
    scene.commands.iter().any(
        |cmd| matches!(cmd, RenderCommand::PushLayer(layer) if layer.backdrop_filter.is_some()),
    )
}

#[test]
fn glass_not_drawn_keeps_its_backdrop_filter() {
    for body in [
        "<div style='width:0; height:40px; backdrop-filter:blur(4px); --glass:regular'></div>",
        "<span style='backdrop-filter:blur(4px); --glass:regular'><div>split</div></span>",
    ] {
        let scene = scene_for(body);
        assert!(groups(&scene).is_empty(), "{body}");
        assert!(has_backdrop_filter(&scene), "{body}: {:#?}", scene.commands);
    }
}

#[test]
fn shaped_corners_stay_rounded_rects() {
    let bevel = shape_of(
        "width:100px; height:100px; border-radius:50%; corner-shape:bevel; --glass:regular",
    );
    assert_eq!(
        bevel.kind,
        ShapeKind::RoundedRect {
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            radii: RoundedRectRadii::from_single_radius(50.0),
            corner: CornerStyle::Superellipse([0.0; 4]),
        }
    );
    let squircle = shape_of(
        "width:100px; height:100px; border-radius:50%; corner-shape:squircle; --glass:regular",
    );
    assert_eq!(
        squircle.kind,
        ShapeKind::RoundedRect {
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            radii: RoundedRectRadii::from_single_radius(50.0),
            corner: CornerStyle::Superellipse([2.0; 4]),
        }
    );
}

#[test]
fn continuous_capsule_stays_a_rounded_rect() {
    let shape = shape_of(
        "width:100px; height:40px; border-radius:9999px; --glass:regular; \
         --corner-style:continuous",
    );
    assert_eq!(
        shape.kind,
        ShapeKind::RoundedRect {
            rect: Rect::new(0.0, 0.0, 100.0, 40.0),
            radii: RoundedRectRadii::from_single_radius(20.0),
            corner: CornerStyle::Continuous,
        }
    );
}

/// A recording scene without glass support, which falls back to `fill_tint`.
struct TintOnly(Scene);

impl RenderContext for TintOnly {}

impl PaintScene for TintOnly {
    fn reset(&mut self) {
        self.0.reset();
    }

    fn push_layer(
        &mut self,
        fill: Fill,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
        filter: Option<Arc<anyrender::Filter>>,
        backdrop_filter: Option<Arc<anyrender::Filter>>,
    ) {
        self.0
            .push_layer(fill, blend, alpha, transform, clip, filter, backdrop_filter);
    }

    fn push_clip_layer(&mut self, fill: Fill, transform: Affine, clip: &impl Shape) {
        self.0.push_clip_layer(fill, transform, clip);
    }

    fn pop_layer(&mut self) {
        self.0.pop_layer();
    }

    fn stroke<'a>(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.0
            .stroke(style, transform, brush, brush_transform, shape);
    }

    fn fill<'a>(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.0.fill(style, transform, brush, brush_transform, shape);
    }

    fn draw_glyphs<'a, 's: 'a>(
        &'s mut self,
        font: &'a FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'a [NormalizedCoord],
        embolden: Vec2,
        style: impl Into<StyleRef<'a>>,
        brush: impl Into<PaintRef<'a>>,
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = Glyph> + Clone,
    ) {
        self.0.draw_glyphs(
            font,
            font_size,
            hint,
            normalized_coords,
            embolden,
            style,
            brush,
            brush_alpha,
            transform,
            glyph_transform,
            glyphs,
        );
    }

    fn draw_box_shadow(
        &mut self,
        transform: Affine,
        rect: Rect,
        brush: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.0
            .draw_box_shadow(transform, rect, brush, radius, std_dev);
    }
}

#[test]
fn tint_fallback_draws_in_device_px() {
    let mut doc = doc(
        "<div style='margin:7px 0 0 13px; width:100px; height:40px; --glass:regular; \
         --glass-tint:rgb(255 0 0 / 0.5); background:#0000ff'></div>",
    );
    let size = (f64::from(VIEW) * SCALE) as u32;
    let mut painter = TintOnly(Scene::new());
    assert!(!painter.supports_glass());
    paint_scene(&mut painter, &mut doc, SCALE, size, size, 0, 0);
    let scene = painter.0;
    assert!(groups(&scene).is_empty());

    let device_box = |rgb| {
        let RenderCommand::Fill(fill) = &scene.commands[fill_index(&scene, rgb)] else {
            unreachable!()
        };
        fill.transform
            .transform_rect_bbox(fill.shape.bounding_box())
    };
    let tint = device_box([1.0, 0.0, 0.0]);
    let background = device_box(BLUE);
    assert_eq!(background, Rect::new(39.0, 21.0, 339.0, 141.0));
    assert_near(tint.origin(), background.origin());
    assert_near(
        Point::new(tint.x1, tint.y1),
        Point::new(background.x1, background.y1),
    );
}
