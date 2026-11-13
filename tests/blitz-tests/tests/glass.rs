//! An element with `--glass` emits one glass group, just before its background, in the
//! element's border-box CSS px; a `--glass-container` groups the glass in its paint subtree.

use anyrender::glass::{
    BackdropSource, CornerStyle, GlassGroup, GlassShape, GlassVariant, ShapeKind,
};
use anyrender::recording::RenderCommand;
use anyrender::{Glyph, NormalizedCoord, PaintRef, PaintScene, RenderContext, Scene};
use blitz_dom::{DocumentConfig, NodeId, QualName, ScrollBehavior, local_name, ns};
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

fn doc_from_html(html: &str) -> HtmlDocument {
    let size = (f64::from(VIEW) * SCALE) as u32;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(size, size, SCALE as f32, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    register_glass_properties(&mut doc);
    doc.resolve(0.0);
    doc
}

/// A root container whose glass reaches it through each kind of paint child: in flow, lifted
/// out of static boxes, hoisted by z-index both ways, fixed to the viewport, absolute in a
/// positioned box with its own transform, and scrolled. `GLASS` marks where `--glass` goes.
const PAINT_ORDER_PAGE: &str = "<html style='--glass-container:6px'>\
    <body style='margin:0; height:1000px'>\
    <div style='height:50px'></div>\
    <div style='width:20px; height:20px; GLASS background:#ff0000'></div>\
    <div><div><div style='position:relative; left:7px; top:3px; width:20px; height:20px; \
        GLASS background:#00ff00'></div></div></div>\
    <div style='position:relative; z-index:-1; width:20px; height:20px; GLASS \
        background:#ff00ff'></div>\
    <div style='position:relative; width:60px; height:20px'>\
        <div style='position:absolute; left:30px; top:0; width:20px; height:20px; \
        transform:rotate(10deg); GLASS background:#000000'></div></div>\
    <div id='s' style='width:60px; height:40px; overflow:auto'>\
        <div style='height:30px'></div>\
        <div style='width:20px; height:20px; GLASS background:#ffff00'></div>\
        <div style='height:200px'></div></div>\
    <div style='position:fixed; top:5px; left:150px; width:20px; height:20px; GLASS \
        background:#0000ff'></div>\
    <div style='position:absolute; z-index:1; top:100px; left:100px; width:20px; \
        height:20px; GLASS background:#00ffff'></div>\
    </body></html>";

/// `PAINT_ORDER_PAGE` with `glass` for each `GLASS`, scrolled 40px down and its scroller 20px.
fn paint_order_doc(glass: &str) -> HtmlDocument {
    let mut doc = doc_from_html(&PAINT_ORDER_PAGE.replace("GLASS", glass));
    doc.scroll_viewport_by(0.0, -40.0);
    let scroller = node_id(&doc, "#s");
    doc.scroll_by(scroller, 0.0, 20.0, ScrollBehavior::Instant);
    doc.resolve(0.0);
    doc
}

/// Each command's kind, with the colour and device-px translation and size of solid fills.
fn summary(scene: &Scene) -> Vec<String> {
    scene
        .commands
        .iter()
        .map(|cmd| match cmd {
            RenderCommand::PushLayer(_) => "push".to_owned(),
            RenderCommand::PushClipLayer(_) => "clip".to_owned(),
            RenderCommand::PushVibrantLayer(_) => "vibrant".to_owned(),
            RenderCommand::PopLayer => "pop".to_owned(),
            RenderCommand::Stroke(_) => "stroke".to_owned(),
            RenderCommand::GlyphRun(_) => "glyphs".to_owned(),
            RenderCommand::BoxShadow(_) => "shadow".to_owned(),
            RenderCommand::Glass(_) => "glass".to_owned(),
            RenderCommand::Fill(fill) => {
                let rgb = match &fill.brush {
                    anyrender::Paint::Solid(color) => format!(
                        "{:.0},{:.0},{:.0}",
                        color.components[0] * 255.0,
                        color.components[1] * 255.0,
                        color.components[2] * 255.0
                    ),
                    _ => "paint".to_owned(),
                };
                let t = fill.transform.translation();
                let b = fill.shape.bounding_box();
                format!(
                    "fill {rgb} at {:.1},{:.1} to {:.1},{:.1}",
                    t.x, t.y, b.x1, b.y1
                )
            }
        })
        .collect()
}

#[test]
fn container_paint_order_is_unchanged() {
    let scene = record(&mut paint_order_doc(""));
    // Recorded before `draw_children` shared its child order with the container walk.
    let expected = [
        "fill 0,0,0 at 0.0,0.0 to 600.0,1000.0",
        "fill 255,0,255 at 0.0,150.0 to 60.0,60.0",
        "fill 255,0,0 at 0.0,30.0 to 60.0,60.0",
        "clip",
        "fill 255,255,0 at 0.0,300.0 to 60.0,60.0",
        "pop",
        "fill 128,128,128 at 0.0,270.0 to 174.0,99.0",
        "stroke",
        "fill 0,255,0 at 21.0,99.0 to 60.0,60.0",
        "fill 0,0,0 at 95.7,205.2 to 60.0,60.0",
        "fill 0,0,255 at 450.0,15.0 to 60.0,60.0",
        "fill 0,255,255 at 300.0,180.0 to 60.0,60.0",
    ];
    assert_eq!(summary(&scene), expected);
}

/// A glass box of `size` CSS px with `style` and the background `#rgb`.
fn glass_box(size: u32, style: &str, rgb: &str) -> String {
    format!(
        "<div style='width:{size}px; height:{size}px; --glass:regular; {style} \
         background:#{rgb}'></div>"
    )
}

fn sizes(group: &GlassGroup) -> Vec<(u32, f64)> {
    group
        .shapes
        .iter()
        .map(|shape| {
            let width = match &shape.kind {
                ShapeKind::RoundedRect { rect, .. }
                | ShapeKind::Capsule(rect)
                | ShapeKind::Ellipse(rect) => rect.width(),
                other => panic!("unexpected shape {other:?}"),
            };
            (shape.id, width)
        })
        .collect()
}

#[test]
fn container_collects_its_glass_descendants() {
    let mut doc = doc(&format!(
        "<div id='c' style='--glass-container:0px; display:flex; gap:10px'>{}\
         <div><div style='width:30px; height:30px; --glass:clear; background:#00ff00'></div>\
         </div></div>",
        glass_box(40, "", "0000ff"),
    ));
    let scene = record(&mut doc);

    let groups = groups(&scene);
    assert_eq!(groups.len(), 1, "{groups:#?}");
    let group = groups[0];
    assert_eq!(sizes(group), [(0, 40.0), (1, 30.0)]);
    assert_eq!(group.shapes[0].material.variant, GlassVariant::Regular);
    assert_eq!(group.shapes[1].material.variant, GlassVariant::Clear);
    assert_eq!(
        group.shapes[1].transform.translation(),
        Vec2::new(50.0, 0.0)
    );
    assert!(glass_index(&scene) < fill_index(&scene, BLUE));
}

#[test]
fn merge_spacing_is_in_css_px() {
    for (spacing, expected) in [("12px", 12.0), ("0px", 0.0), ("1em", 16.0)] {
        let scene = scene_for(&format!(
            "<div style='--glass-container:{spacing}'>{}{}</div>",
            glass_box(20, "", "0000ff"),
            glass_box(20, "", "00ff00"),
        ));
        let groups = groups(&scene);
        assert_eq!(groups.len(), 1, "{spacing}");
        assert_eq!(groups[0].shapes.len(), 2, "{spacing}");
        assert_eq!(groups[0].merge_spacing, expected, "{spacing}");
    }
}

#[test]
fn union_ids_pair_up() {
    let scene = scene_for(&format!(
        "<div style='--glass-container:4px'>{}{}{}{}{}{}</div>",
        glass_box(10, "--glass-union:u;", "0000ff"),
        glass_box(11, "--glass-union:v;", "0000ff"),
        glass_box(12, "--glass-union:u;", "0000ff"),
        glass_box(13, "--glass-union:none;", "0000ff"),
        glass_box(14, "--glass-union:u;", "0000ff"),
        glass_box(15, "--glass-union:none;", "0000ff"),
    ));
    let groups = groups(&scene);
    assert_eq!(groups.len(), 1);
    let mut unions = groups[0].unions.clone();
    unions.sort_unstable();
    assert_eq!(unions, [(0, 2), (0, 4), (2, 4)]);

    // Shapes in different containers, or in none, never join.
    let scene = scene_for(&format!(
        "<div style='--glass-container:4px'>{}</div>\
         <div style='--glass-container:4px'>{}</div>{}{}",
        glass_box(10, "--glass-union:u;", "0000ff"),
        glass_box(11, "--glass-union:u;", "0000ff"),
        glass_box(12, "--glass-union:u;", "0000ff"),
        glass_box(13, "--glass-union:u;", "0000ff"),
    ));
    let groups = self::groups(&scene);
    assert_eq!(groups.len(), 4);
    assert!(groups.iter().all(|group| group.unions.is_empty()));
}

#[test]
fn nested_container_starts_its_own_group() {
    let mut doc = doc(&format!(
        "<div id='outer' style='--glass-container:8px'>{}\
         <div id='inner' style='--glass-container:2px; width:50px; height:50px; \
         --glass:regular'>{}{}</div>{}</div>",
        glass_box(10, "--glass-union:u;", "0000ff"),
        glass_box(11, "--glass-union:u;", "0000ff"),
        glass_box(12, "", "0000ff"),
        glass_box(13, "", "0000ff"),
    ));
    let scene = record(&mut doc);

    let groups = groups(&scene);
    assert_eq!(groups.len(), 2, "{groups:#?}");
    let (outer, inner) = (groups[0], groups[1]);
    assert_eq!(outer.id, Some(node_id(&doc, "#outer").as_u64()));
    assert_eq!(sizes(outer), [(0, 10.0), (1, 13.0)]);
    assert_eq!(outer.merge_spacing, 8.0);
    assert_eq!(inner.id, Some(node_id(&doc, "#inner").as_u64()));
    assert_eq!(sizes(inner), [(0, 50.0), (1, 11.0), (2, 12.0)]);
    assert_eq!(inner.merge_spacing, 2.0);
    assert!(outer.unions.is_empty() && inner.unions.is_empty());
}

#[test]
fn collected_glass_paints_only_its_content() {
    let mut doc = doc("<div id='c' style='--glass-container:4px'>\
         <div style='width:40px; height:40px; --glass:regular; background:#0000ff; \
         backdrop-filter:blur(4px)'>\
         <div style='width:10px; height:10px; background:#00ff00'></div></div>\
         <div style='width:20px; height:20px; --glass:regular; background:#ff0000; \
         backdrop-filter:blur(4px)'></div></div>");
    let scene = record(&mut doc);
    let groups = groups(&scene);
    assert_eq!(groups.len(), 1, "{:#?}", scene.commands);
    assert_eq!(groups[0].id, Some(node_id(&doc, "#c").as_u64()));
    assert_eq!(sizes(groups[0]), [(0, 40.0), (1, 20.0)]);
    assert!(fill_index(&scene, GREEN) < fill_index(&scene, [1.0, 0.0, 0.0]));
    assert!(!has_backdrop_filter(&scene), "{:#?}", scene.commands);
    let glass = glass_index(&scene);
    assert!(glass < fill_index(&scene, BLUE));
    assert!(fill_index(&scene, BLUE) < fill_index(&scene, GREEN));
}

#[test]
fn hidden_descendants_are_left_out() {
    let scene = scene_for(&format!(
        "<div style='--glass-container:4px'>{}{}{}<div style='display:none'>{}</div>\
         <div style='visibility:hidden'>{}</div>{}</div>",
        glass_box(10, "", "0000ff"),
        glass_box(11, "display:none;", "0000ff"),
        glass_box(12, "visibility:hidden;", "0000ff"),
        glass_box(13, "", "0000ff"),
        glass_box(14, "", "0000ff"),
        glass_box(15, "", "0000ff"),
    ));
    let groups = groups(&scene);
    assert_eq!(groups.len(), 1);
    assert_eq!(sizes(groups[0]), [(0, 10.0), (1, 15.0)]);
}

#[test]
fn container_with_no_drawn_glass_emits_nothing() {
    for members in [
        String::new(),
        "<div style='width:20px; height:20px; background:#0000ff'></div>".to_owned(),
        glass_box(10, "display:none;", "0000ff"),
        glass_box(10, "visibility:hidden;", "0000ff"),
        glass_box(0, "", "0000ff"),
        glass_box(10, "transform:scale(0);", "0000ff"),
    ] {
        let scene = scene_for(&format!(
            "<div style='--glass-container:4px; width:50px; height:50px; \
             backdrop-filter:blur(2px)'>{members}</div>"
        ));
        assert!(
            groups(&scene).is_empty(),
            "{members}: {:#?}",
            scene.commands
        );
        assert!(has_backdrop_filter(&scene), "{members}");
    }
}

#[test]
fn group_id_is_the_container_node_id() {
    let mut doc = doc(&format!(
        "<div id='c' style='--glass-container:4px'><div id='m' style='width:10px; \
         height:10px; --glass:regular'></div></div>\
         <div id='g' style='--glass-container:4px; width:30px; height:30px; \
         --glass:regular'>{}</div>",
        glass_box(10, "", "0000ff"),
    ));
    let scene = record(&mut doc);
    let groups = groups(&scene);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].id, Some(node_id(&doc, "#c").as_u64()));
    assert_ne!(groups[0].id, Some(node_id(&doc, "#m").as_u64()));
    assert_eq!(groups[1].id, Some(node_id(&doc, "#g").as_u64()));
    assert_eq!(sizes(groups[1]), [(0, 30.0), (1, 10.0)]);
}

#[test]
fn fixed_and_absolute_glass_land_where_they_paint() {
    let mut doc = paint_order_doc("--glass:regular;");
    let scene = record(&mut doc);
    let groups = groups(&scene);
    // The scroller's clip holds its glass, which draws alone inside it.
    assert_eq!(groups.len(), 2, "{groups:#?}");
    let group = groups[0];
    assert_eq!(group.id, Some(node_id(&doc, "html").as_u64()));
    assert_eq!(group.merge_spacing, 6.0);

    let colours: [[f32; 3]; 6] = [
        [1.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0],
    ];
    assert_eq!(group.shapes.len(), colours.len());
    for (shape, rgb) in group.shapes.iter().zip(colours) {
        assert_lands_on_fill(&scene, shape, rgb);
    }
    assert_eq!(groups[1].shapes.len(), 1);
    assert_lands_on_fill(&scene, &groups[1].shapes[0], [1.0, 1.0, 0.0]);
    // The fixed box stays at its viewport position, whatever the scroll.
    assert_eq!(
        group.shapes[4].transform.translation(),
        Vec2::new(150.0, 5.0)
    );
}

/// Asserts that `shape` sits where the first solid `rgb` fill paints, in CSS px.
fn assert_lands_on_fill(scene: &Scene, shape: &GlassShape, rgb: [f32; 3]) {
    let RenderCommand::Fill(fill) = &scene.commands[fill_index(scene, rgb)] else {
        unreachable!()
    };
    let s = Affine::scale(SCALE);
    let painted = s.inverse() * fill.transform * s;
    let (a, b) = (shape.transform.as_coeffs(), painted.as_coeffs());
    assert!(
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9),
        "{rgb:?}: shape {a:?}, fill {b:?}"
    );
}

/// The index of the glass command whose group has the id `id`.
fn group_index(scene: &Scene, id: u64) -> usize {
    scene
        .commands
        .iter()
        .position(|cmd| matches!(cmd, RenderCommand::Glass(group) if group.id == Some(id)))
        .unwrap_or_else(|| panic!("no group {id}: {:#?}", scene.commands))
}

/// How many layers are open just before the command at `index`.
fn depth_at(scene: &Scene, index: usize) -> usize {
    scene.commands[..index]
        .iter()
        .fold(0, |depth, cmd| match cmd {
            RenderCommand::PushLayer(_)
            | RenderCommand::PushClipLayer(_)
            | RenderCommand::PushVibrantLayer(_) => depth + 1,
            RenderCommand::PopLayer => depth - 1,
            _ => depth,
        })
}

#[test]
fn glass_scrolled_out_of_a_scroller_stays_out_of_the_group() {
    let mut doc = doc(&format!(
        "<div id='c' style='--glass-container:4px'>{}\
         <div id='s' style='width:60px; height:40px; overflow:hidden'>\
         <div id='out' style='width:20px; height:20px; --glass:regular'></div>\
         <div style='height:30px'></div>\
         <div id='in' style='width:21px; height:21px; --glass:regular'></div>\
         <div style='height:200px'></div></div></div>",
        glass_box(10, "", "0000ff"),
    ));
    let scroller = node_id(&doc, "#s");
    doc.scroll_by(scroller, 0.0, 30.0, ScrollBehavior::Instant);
    doc.resolve(0.0);
    let scene = record(&mut doc);

    let groups = groups(&scene);
    assert_eq!(groups.len(), 2, "{groups:#?}");
    assert_eq!(groups[0].id, Some(node_id(&doc, "#c").as_u64()));
    assert_eq!(sizes(groups[0]), [(0, 10.0)]);
    let inside = node_id(&doc, "#in").as_u64();
    assert_eq!(groups[1].id, Some(inside));
    assert_eq!(sizes(groups[1]), [(0, 21.0)]);
    assert_eq!(
        groups[1].shapes[0].transform.translation(),
        Vec2::new(0.0, 30.0)
    );

    let container = group_index(&scene, node_id(&doc, "#c").as_u64());
    assert_eq!(
        depth_at(&scene, group_index(&scene, inside)),
        depth_at(&scene, container) + 1
    );
}

#[test]
fn glass_under_opacity_is_not_collected() {
    let mut doc = doc(&format!(
        "<div id='c' style='--glass-container:4px'>{}\
         <div style='opacity:0.5'><div id='under' style='width:11px; height:11px; \
         --glass:regular'></div></div>\
         <div id='own' style='width:12px; height:12px; opacity:0.5; --glass:regular'></div>\
         <div id='mask' style='width:13px; height:13px; --glass:regular; \
         mask-image:linear-gradient(black, transparent)'></div></div>",
        glass_box(10, "", "0000ff"),
    ));
    let scene = record(&mut doc);

    let groups = groups(&scene);
    assert_eq!(groups.len(), 4, "{groups:#?}");
    let container = node_id(&doc, "#c").as_u64();
    assert_eq!(groups[0].id, Some(container));
    assert_eq!(sizes(groups[0]), [(0, 10.0)]);
    let depth = depth_at(&scene, group_index(&scene, container));
    for (selector, size) in [("#under", 11.0), ("#own", 12.0), ("#mask", 13.0)] {
        let id = node_id(&doc, selector).as_u64();
        let index = group_index(&scene, id);
        let RenderCommand::Glass(group) = &scene.commands[index] else {
            unreachable!()
        };
        assert_eq!(sizes(group), [(0, size)], "{selector}");
        assert!(depth_at(&scene, index) > depth, "{selector}");
    }
}

#[test]
fn clipping_container_draws_its_members_inside_its_clip() {
    let mut doc = doc(&format!(
        "<div id='c' style='--glass-container:4px; --glass:regular; width:60px; \
         height:40px; overflow:auto; background:#00ff00'>{}{}\
         <div style='height:200px'></div></div>",
        glass_box(10, "--glass-union:u;", "0000ff"),
        glass_box(11, "--glass-union:u;", "ff0000"),
    ));
    let scene = record(&mut doc);

    let container = node_id(&doc, "#c").as_u64();
    let members = container | 1 << 63;
    let groups = groups(&scene);
    assert_eq!(groups.len(), 2, "{groups:#?}");
    assert_eq!(groups[0].id, Some(container));
    assert_eq!(sizes(groups[0]), [(0, 60.0)]);
    assert_eq!(groups[0].merge_spacing, 0.0);
    assert_eq!(groups[1].id, Some(members));
    assert_eq!(sizes(groups[1]), [(0, 10.0), (1, 11.0)]);
    assert_eq!(groups[1].merge_spacing, 4.0);
    assert_eq!(groups[1].unions, [(0, 1)]);

    let own = group_index(&scene, container);
    let inner = group_index(&scene, members);
    assert!(own < fill_index(&scene, GREEN) && fill_index(&scene, GREEN) < inner);
    assert_eq!(depth_at(&scene, inner), depth_at(&scene, own) + 1);
    assert!(inner < fill_index(&scene, BLUE));

    // Scrolled, the members move with the content and the container's own glass stays.
    let scroller = node_id(&doc, "#c");
    doc.scroll_by(scroller, 0.0, 5.0, ScrollBehavior::Instant);
    doc.resolve(0.0);
    let scene = record(&mut doc);
    let groups = self::groups(&scene);
    assert_eq!(groups.len(), 2, "{groups:#?}");
    assert_eq!(groups[0].shapes[0].transform.translation(), Vec2::ZERO);
    let shapes = &groups[1].shapes;
    assert_eq!(shapes[0].transform.translation(), Vec2::new(0.0, -5.0));
    assert_eq!(shapes[1].transform.translation(), Vec2::new(0.0, 5.0));
    assert_lands_on_fill(&scene, &shapes[0], BLUE);
    assert_lands_on_fill(&scene, &shapes[1], [1.0, 0.0, 0.0]);
}

fn set_style(doc: &mut HtmlDocument, selector: &str, style: &str) {
    let id = node_id(doc, selector);
    let name = QualName::new(None, ns!(), local_name!("style"));
    doc.mutate().set_attribute(id, name, style);
    doc.resolve(0.0);
}

#[test]
fn toggling_glass_roles_rebuilds_stacking_contexts() {
    let mut doc = doc(&format!(
        "<div style='height:10px'></div>\
         <div id='c' style='--glass-container:none'>{}\
         <div id='w'>{}</div></div>",
        glass_box(10, "", "0000ff"),
        glass_box(11, "position:relative; z-index:1;", "ff0000"),
    ));
    let container = node_id(&doc, "#c");
    let is_root = |doc: &HtmlDocument, selector| {
        doc.get_node(node_id(doc, selector))
            .unwrap()
            .stacking_context
            .is_some()
    };
    let groups_of = |doc: &mut HtmlDocument| {
        let scene = record(doc);
        groups(&scene)
            .into_iter()
            .map(|group| (group.id, sizes(group)))
            .collect::<Vec<_>>()
    };

    // Not a container: two lone groups.
    assert!(!is_root(&doc, "#c"));
    assert_eq!(groups_of(&mut doc).len(), 2);

    // A container now: the z-indexed member paints in its subtree and joins its group.
    set_style(&mut doc, "#c", "--glass-container:4px");
    assert!(is_root(&doc, "#c"));
    assert_eq!(
        groups_of(&mut doc),
        [(Some(container.as_u64()), vec![(0, 10.0), (1, 11.0)])]
    );

    // Back to none: the member leaves again.
    set_style(&mut doc, "#c", "--glass-container:none");
    assert!(!is_root(&doc, "#c"));
    assert_eq!(groups_of(&mut doc).len(), 2);

    // `--glass` on the wrapper makes it a stacking context, and taking it off undoes that.
    set_style(&mut doc, "#w", "--glass:regular");
    assert!(is_root(&doc, "#w"));
    set_style(&mut doc, "#w", "--glass:none");
    assert!(!is_root(&doc, "#w"));
}

#[test]
fn positioned_glass_joins_a_static_container() {
    let mut doc = doc(&format!(
        "<div style='height:10px'></div>\
         <div id='c' style='--glass-container:4px'>{}{}\
         <div style='position:relative; height:20px'>{}</div>{}{}</div>",
        glass_box(10, "position:relative; left:5px;", "0000ff"),
        glass_box(11, "position:relative; z-index:1;", "00ff00"),
        glass_box(12, "position:absolute; top:2px; left:30px;", "ff0000"),
        glass_box(13, "position:relative; z-index:-1;", "ff00ff"),
        glass_box(14, "position:absolute; top:150px; left:150px;", "00ffff"),
    ));
    let scene = record(&mut doc);

    let groups = groups(&scene);
    assert_eq!(groups.len(), 2, "{groups:#?}");
    assert_eq!(groups[0].id, Some(node_id(&doc, "#c").as_u64()));
    assert_eq!(
        sizes(groups[0]),
        [(0, 13.0), (1, 10.0), (2, 12.0), (3, 11.0)]
    );
    for (shape, rgb) in groups[0].shapes.iter().zip([
        [1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]) {
        assert_lands_on_fill(&scene, shape, rgb);
    }
    // Its containing block, the root, lies above the container and paints it.
    assert_eq!(sizes(groups[1]), [(0, 14.0)]);
    assert_lands_on_fill(&scene, &groups[1].shapes[0], [0.0, 1.0, 1.0]);
}
