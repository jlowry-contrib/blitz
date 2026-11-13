//! A glass element with `--glass-source` re-paints the named element under itself, clipped to
//! its shape, just before its glass; with only `--glass-magnify` or `--glass-source-offset`
//! set, its backdrop is a `Region`.

use anyrender::Scene;
use anyrender::glass::{BackdropSource, GlassGroup};
use anyrender::recording::RenderCommand;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_tests::register_glass_properties;
use blitz_traits::shell::{ColorScheme, Viewport};
use kurbo::{Affine, Rect, Shape, Vec2};
use std::sync::Arc;

/// Every page paints at 3×, as the iPhone target does.
const SCALE: f64 = 3.0;
/// The viewport's size in CSS px.
const VIEW: u32 = 200;

const RED: [f32; 3] = [1.0, 0.0, 0.0];
const GREEN: [f32; 3] = [0.0, 1.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];

fn scene_for(body: &str) -> Scene {
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
    register_glass_properties(&mut doc);
    doc.resolve(0.0);
    let mut scene = Scene::new();
    paint_scene(&mut scene, &mut doc, SCALE, size, size, 0, 0);
    scene
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

/// The indices of the solid fills of `rgb`.
fn fills_of(scene: &Scene, rgb: [f32; 3]) -> Vec<usize> {
    (0..scene.commands.len())
        .filter(|&i| is_fill_of(&scene.commands[i], rgb))
        .collect()
}

fn fill_translation(scene: &Scene, index: usize) -> Vec2 {
    match &scene.commands[index] {
        RenderCommand::Fill(fill) => fill.transform.translation(),
        cmd => panic!("not a fill: {cmd:?}"),
    }
}

fn glass_indices(scene: &Scene) -> Vec<usize> {
    (0..scene.commands.len())
        .filter(|&i| matches!(scene.commands[i], RenderCommand::Glass(_)))
        .collect()
}

fn assert_vec_near(a: Vec2, b: Vec2) {
    assert!((a - b).hypot() < 1e-6, "{a:?} != {b:?}");
}

fn assert_rect_near(a: Rect, b: Rect) {
    let d = [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1];
    assert!(d.iter().all(|d| d.abs() < 1e-6), "{a:?} != {b:?}");
}

/// A green source at (50, 120) and a glass element at (10, 20) that names it.
fn portal_page(glass_extra: &str) -> String {
    format!(
        "<div id='src' style='position:absolute; left:50px; top:120px; width:60px; \
         height:30px; background:#00ff00'></div>\
         <div id='g' style='position:absolute; left:10px; top:20px; width:100px; \
         height:40px; --glass:regular; --glass-source:src; {glass_extra}'></div>"
    )
}

#[test]
fn portal_repaints_its_source_before_the_glass() {
    let scene = scene_for(&portal_page(""));

    let glass = glass_indices(&scene);
    assert_eq!(glass.len(), 1, "{:#?}", scene.commands);
    let glass = glass[0];
    assert_eq!(groups(&scene)[0].source, BackdropSource::Behind);

    let greens = fills_of(&scene, GREEN);
    assert_eq!(greens.len(), 2, "the source at home and in the portal");
    // The source paints at home first, at its own origin.
    assert_vec_near(
        fill_translation(&scene, greens[0]),
        Vec2::new(50.0, 120.0) * SCALE,
    );
    // The copy lands on the glass element's origin.
    let copy = greens[1];
    assert_vec_near(
        fill_translation(&scene, copy),
        Vec2::new(10.0, 20.0) * SCALE,
    );

    // It sits inside a clip to the glass shape, which is popped before the glass.
    let push = (0..copy)
        .rev()
        .find(|&i| matches!(scene.commands[i], RenderCommand::PushClipLayer(_)))
        .expect("no clip before the copy");
    assert!(greens[0] < push, "home fill {}, clip {push}", greens[0]);
    let RenderCommand::PushClipLayer(clip) = &scene.commands[push] else {
        unreachable!()
    };
    let clip_box = (clip.transform * clip.clip.clone()).bounding_box();
    assert_rect_near(clip_box, Rect::new(30.0, 60.0, 330.0, 180.0));
    assert!(copy < glass);
    assert!(
        matches!(scene.commands[glass - 1], RenderCommand::PopLayer),
        "the clip is popped just before the glass"
    );
}

#[test]
fn hidden_source_paints_only_through_the_portal() {
    let scene = scene_for(&portal_page("--glass-source-hidden: 1"));

    let greens = fills_of(&scene, GREEN);
    assert_eq!(greens.len(), 1, "{:#?}", scene.commands);
    assert_vec_near(
        fill_translation(&scene, greens[0]),
        Vec2::new(10.0, 20.0) * SCALE,
    );
    assert!(greens[0] < glass_indices(&scene)[0]);
}

#[test]
fn glass_that_never_paints_hides_nothing() {
    for extra in ["display: none", "width: 0px", "transform: scale(0)"] {
        let scene = scene_for(&portal_page(&format!("--glass-source-hidden: 1; {extra}")));
        let greens = fills_of(&scene, GREEN);
        assert_eq!(greens.len(), 1, "{extra}: {:#?}", scene.commands);
        assert_vec_near(
            fill_translation(&scene, greens[0]),
            Vec2::new(50.0, 120.0) * SCALE,
        );
    }
}

#[test]
fn portal_cycle_paints_once() {
    // A portal's copy is content only, so every cycle stops after one copy, with one glass
    // per glass element.

    // A glass element that names itself paints its content once more, under its own glass.
    let scene = scene_for(
        "<div id='g' style='width:100px; height:40px; background:#ff0000; \
         --glass:regular; --glass-source:g'></div>",
    );
    assert_eq!(fills_of(&scene, RED).len(), 2, "{:#?}", scene.commands);
    assert_eq!(glass_indices(&scene).len(), 1);

    // One that names its ancestor repaints the ancestor once.
    let scene = scene_for(
        "<div id='p' style='width:150px; height:60px; background:#00ff00'>\
         <div style='width:100px; height:40px; --glass:regular; --glass-source:p'></div>\
         </div>",
    );
    assert_eq!(fills_of(&scene, GREEN).len(), 2, "{:#?}", scene.commands);
    assert_eq!(glass_indices(&scene).len(), 1);

    // Two that name each other each paint the other once.
    let scene = scene_for(
        "<div id='a' style='width:100px; height:40px; background:#ff0000; \
         --glass:regular; --glass-source:b'></div>\
         <div id='b' style='width:100px; height:40px; background:#0000ff; \
         --glass:regular; --glass-source:a'></div>",
    );
    assert_eq!(fills_of(&scene, RED).len(), 2, "{:#?}", scene.commands);
    assert_eq!(fills_of(&scene, BLUE).len(), 2);
    assert_eq!(glass_indices(&scene).len(), 2);
}

#[test]
fn portal_copy_draws_no_glass() {
    let scene = scene_for(
        "<div id='src' style='width:100px; height:40px; background:#00ff00'>\
         <div style='width:20px; height:20px; background:#0000ff; --glass:regular'></div>\
         </div>\
         <div style='width:100px; height:40px; --glass:regular; --glass-source:src'></div>",
    );
    // The source's glass child appears in the copy, without its glass.
    let blues = fills_of(&scene, BLUE);
    assert_eq!(blues.len(), 2, "{:#?}", scene.commands);
    let glass = glass_indices(&scene);
    assert_eq!(glass.len(), 2, "the child's glass at home and the portal's");
    assert!(glass[0] < blues[0] && blues[1] < glass[1]);

    let ids: Vec<_> = groups(&scene).iter().map(|g| g.id).collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "group ids: {ids:?}");
}

#[test]
fn hidden_source_hides_its_hoisted_descendants() {
    let page = |source_style: &str| {
        format!(
            "<div id='src' style='width:100px; height:40px; {source_style}'>\
             <div style='position:relative; z-index:1; width:10px; height:10px; \
             background:#ff0000'></div>\
             <div style='position:absolute; left:50px; top:5px; width:10px; height:10px; \
             background:#0000ff'></div></div>\
             <div style='position:absolute; left:10px; top:100px; width:100px; height:40px; \
             --glass:regular; --glass-source:src; --glass-source-hidden:1'></div>"
        )
    };

    // A source that is neither a stacking context nor a containing block: its badge and its
    // absolute child paint with an outer root, so they're hidden and not in the copy.
    let scene = scene_for(&page(""));
    assert_eq!(
        fills_of(&scene, RED),
        Vec::<usize>::new(),
        "{:#?}",
        scene.commands
    );
    assert_eq!(fills_of(&scene, BLUE), Vec::<usize>::new());

    // As advised, a source that is both paints them only in the copy.
    let scene = scene_for(&page("position:relative; isolation:isolate"));
    let glass = glass_indices(&scene)[0];
    for rgb in [RED, BLUE] {
        let fills = fills_of(&scene, rgb);
        assert_eq!(fills.len(), 1, "{rgb:?}: {:#?}", scene.commands);
        assert!(fills[0] < glass);
        assert!(fill_translation(&scene, fills[0]).y >= 100.0 * SCALE);
    }
}

#[test]
fn unknown_source_paints_nothing() {
    let page = |source: &str| {
        format!(
            "<div style='width:60px; height:30px; background:#00ff00'></div>\
             <div style='width:100px; height:40px; --glass:regular; {source}'></div>"
        )
    };
    let named = scene_for(&page("--glass-source: nope"));
    let plain = scene_for(&page(""));
    assert_eq!(named.commands, plain.commands);
    assert_eq!(groups(&named)[0].source, BackdropSource::Behind);
}

#[test]
fn region_source_is_the_offset_box_scaled_about_its_centre() {
    let page = |extra: &str| {
        format!(
            "<div style='margin:10px; width:100px; height:40px; transform:rotate(10deg); \
             --glass:regular; {extra}'></div>"
        )
    };
    let scene = scene_for(&page(
        "--glass-magnify: 1.15; --glass-source-offset: 0px -10px",
    ));
    let groups = groups(&scene);
    assert_eq!(groups.len(), 1);
    let shape = &groups[0].shapes[0];
    let BackdropSource::Region { rect, transform } = &groups[0].source else {
        panic!("not a region: {:?}", groups[0].source);
    };
    // The 100×40 box moved up 10px, centred on (50, 10), and shrunk by 1/1.15.
    assert_rect_near(
        *rect,
        Rect::from_center_size((50.0, 10.0), (100.0 / 1.15, 40.0 / 1.15)),
    );
    let (a, b) = (transform.as_coeffs(), shape.transform.as_coeffs());
    assert!(
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9),
        "{a:?} != {b:?}"
    );
    assert_ne!(shape.transform, Affine::IDENTITY);

    // Each alone makes a region too.
    for extra in ["--glass-magnify: 2", "--glass-source-offset: 5px"] {
        let scene = scene_for(&page(extra));
        assert!(
            matches!(groups_source(&scene), BackdropSource::Region { .. }),
            "{extra}"
        );
    }
    // A magnify of 1, or of 0 or less, which reads as 1, and a zero offset leave it behind.
    for extra in [
        "",
        "--glass-magnify: 1; --glass-source-offset: 0px 0px",
        "--glass-magnify: 0",
        "--glass-magnify: -2",
    ] {
        let scene = scene_for(&page(extra));
        assert_eq!(groups_source(&scene), BackdropSource::Behind, "{extra}");
    }
}

fn groups_source(scene: &Scene) -> BackdropSource {
    groups(scene)[0].source.clone()
}

#[test]
fn container_member_ignores_its_source() {
    // Only a lone glass element re-paints a source; a member of a container's group doesn't,
    // and doesn't hide it either.
    let scene = scene_for(
        "<div style='--glass-container: 4px'>\
         <div style='width:100px; height:40px; --glass:regular; --glass-source:src; \
         --glass-source-hidden: 1; --glass-magnify: 2'></div>\
         <div style='width:100px; height:40px; --glass:regular'></div></div>\
         <div id='src' style='width:60px; height:30px; background:#00ff00'></div>",
    );
    let groups = groups(&scene);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].shapes.len(), 2);
    assert_eq!(groups[0].source, BackdropSource::Behind);
    assert_eq!(fills_of(&scene, GREEN).len(), 1, "{:#?}", scene.commands);
    assert!(
        !scene
            .commands
            .iter()
            .any(|cmd| matches!(cmd, RenderCommand::PushClipLayer(_)))
    );
}
