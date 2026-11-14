//! Builds the glass shape and group of an element with `--glass`.

use std::sync::Arc;

use anyrender::glass::{
    BackdropSource, CornerStyle, GlassMaterial, GlassShape, GlassVariant, ShapeKind,
};
use blitz_dom::glass::{GlassProps, GlassVariant as PropVariant};
use kurbo::{
    Affine, BezPath, Ellipse, Insets, Point, Rect, RoundedRect, RoundedRectRadii, Shape, Vec2,
};

use super::clip_path::GlassClip;
use crate::kurbo_css::{CssBox, NonUniformRoundedRectRadii, corner_style};

/// Slack, in CSS px, for radii that the overlap scaling of css-backgrounds-3 §5.5 leaves a
/// rounding error away from half a side.
const RADIUS_EPSILON: f64 = 1e-6;

/// A glass shape, and what building it lost, for the painter's logs.
pub(crate) struct BuiltShape {
    /// The shape.
    pub(crate) shape: GlassShape,
    /// Whether a rounded rect took the smaller radius of an elliptical corner.
    #[cfg_attr(
        not(feature = "tracing"),
        expect(dead_code, reason = "only the glass logs read it")
    )]
    pub(crate) rounded_elliptical: bool,
    /// Whether an evenodd clip path was emitted with the nonzero rule.
    #[cfg_attr(
        not(feature = "tracing"),
        expect(dead_code, reason = "only the glass logs read it")
    )]
    pub(crate) evenodd: bool,
}

/// The glass shape of an element whose border box is `frame`, painted under `transform`.
///
/// `frame` and `transform` are in device px, as Blitz paints. The shape is in the element's
/// border-box CSS px, origin at its top-left, and its transform is `S⁻¹ · transform · S`
/// with `S` the document scale, which the glass backend applies itself.
///
/// The morph overrides in `props`, when set, replace the box and its radii. Otherwise `clip`,
/// the element's clip path, gives the shape, and without one the border box and its radii do.
/// The shape carries the press, touch and materialize in `props`; the touch is already in
/// the shape's own coordinates.
/// Returns `None` for an empty box or shape, or a non-invertible transform.
pub(crate) fn glass_shape(
    frame: &CssBox,
    clip: Option<GlassClip>,
    props: &GlassProps,
    transform: Affine,
    scale: f64,
    id: u32,
) -> Option<BuiltShape> {
    let size = frame.border_box.size() / scale;
    if !(size.width > 0.0 && size.height > 0.0) {
        return None;
    }
    let det = transform.determinant();
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let variant = props.variant?;

    let corner = corner_style(frame);
    let mut evenodd = false;
    let (kind, rounded_elliptical) = if let Some(geometry) = &props.geometry {
        let rect = Rect::from_center_size(
            (f64::from(geometry.x), f64::from(geometry.y)),
            (f64::from(geometry.w), f64::from(geometry.h)),
        );
        let r = f64::from(geometry.r).max(0.0);
        let radii = overlap_scaled(rect, [Vec2::new(r, r); 4]);
        classify(rect, radii, corner)
    } else {
        match clip {
            None => {
                let rect = Rect::from_origin_size((0.0, 0.0), size);
                classify(rect, used_radii(frame, scale), corner)
            }
            Some(GlassClip::Inset { rect, radii }) => {
                rounded_rect(rect, overlap_scaled(rect, radii), corner)
            }
            Some(GlassClip::Ellipse(rect)) => (ShapeKind::Ellipse(rect), false),
            Some(GlassClip::Path { path, evenodd: e }) => {
                evenodd = e;
                let bounds = path.bounding_box();
                if !(bounds.width() > 0.0 && bounds.height() > 0.0) {
                    return None;
                }
                (ShapeKind::Path(Arc::new(path)), false)
            }
        }
    };
    if let ShapeKind::Ellipse(rect) = &kind
        && !(rect.width() > 0.0 && rect.height() > 0.0)
    {
        return None;
    }
    let transform = Affine::scale(scale.recip()) * transform * Affine::scale(scale);

    let mut material = GlassMaterial::new(match variant {
        PropVariant::Regular => GlassVariant::Regular,
        PropVariant::Clear => GlassVariant::Clear,
        PropVariant::Identity => GlassVariant::Identity,
    });
    material.tint = props.tint;
    material.interactive = props.interactive;

    // Each builder runs only off its default, so an unset shape stays equal to `new`'s. The
    // builders panic on non-finite input, which the guards also keep out.
    let mut shape = GlassShape::new(id, transform, kind, material);
    if props.press.is_finite() && props.press != 0.0 {
        shape = shape.with_press(props.press);
    }
    if let Some((x, y)) = props.touch {
        // Already in the border box's CSS px, the shape's own space, whatever the overrides do.
        let touch = Point::new(f64::from(x), f64::from(y));
        if touch.is_finite() {
            shape = shape.with_touch(touch);
        }
    }
    if props.materialize.is_finite() && props.materialize != 1.0 {
        shape = shape.with_materialize(props.materialize);
    }

    Some(BuiltShape {
        shape,
        rounded_elliptical,
        evenodd,
    })
}

/// Tolerance, in CSS px, for flattening a shape's curves into its portal clip.
const OUTLINE_TOLERANCE: f64 = 0.1;

/// The outline of `kind`, in the shape's own coordinates, with continuous and superellipse
/// corners drawn circular.
pub(crate) fn shape_outline(kind: &ShapeKind) -> Option<BezPath> {
    Some(match kind {
        ShapeKind::RoundedRect { rect, radii, .. } => {
            RoundedRect::from_rect(*rect, *radii).to_path(OUTLINE_TOLERANCE)
        }
        ShapeKind::Capsule(rect) => {
            RoundedRect::from_rect(*rect, rect.width().min(rect.height()) / 2.0)
                .to_path(OUTLINE_TOLERANCE)
        }
        ShapeKind::Ellipse(rect) => Ellipse::from_rect(*rect).to_path(OUTLINE_TOLERANCE),
        ShapeKind::Path(path) => (**path).clone(),
        _ => return None,
    })
}

/// Whether `props` ask for a `Region` backdrop: a magnify other than 1 or a nonzero offset.
pub(crate) fn has_region(props: &GlassProps) -> bool {
    props.magnify != 1.0 || props.source_offset != (0.0, 0.0)
}

/// The `Region` backdrop of `shape`: its box, moved by `offset` in CSS px and scaled by
/// `1 / magnify` about its centre, under the shape's transform; `None` if that box isn't finite.
pub(crate) fn region_source(
    shape: &GlassShape,
    magnify: f32,
    (dx, dy): (f32, f32),
) -> Option<BackdropSource> {
    let bounds = match &shape.kind {
        ShapeKind::RoundedRect { rect, .. }
        | ShapeKind::Capsule(rect)
        | ShapeKind::Ellipse(rect) => *rect,
        ShapeKind::Path(path) => path.bounding_box(),
        _ => return None,
    };
    let moved = bounds + Vec2::new(f64::from(dx), f64::from(dy));
    let rect = Rect::from_center_size(moved.center(), moved.size() / f64::from(magnify));
    rect.is_finite().then_some(BackdropSource::Region {
        rect,
        transform: shape.transform,
    })
}

/// The used radii, after overlap scaling, in the order top-left, top-right, bottom-right,
/// bottom-left, divided by `scale`.
fn used_radii(frame: &CssBox, scale: f64) -> [Vec2; 4] {
    let r = &frame.border_radii;
    [r.top_left, r.top_right, r.bottom_right, r.bottom_left].map(|v| v / scale)
}

/// `radii` on `rect`, scaled down together where they overlap, as css-backgrounds-3 §5.5 says.
fn overlap_scaled(
    rect: Rect,
    [top_left, top_right, bottom_right, bottom_left]: [Vec2; 4],
) -> [Vec2; 4] {
    let radii = NonUniformRoundedRectRadii {
        top_left,
        top_right,
        bottom_right,
        bottom_left,
    };
    let r = CssBox::new(rect, Insets::ZERO, Insets::ZERO, 0.0, radii).border_radii;
    [r.top_left, r.top_right, r.bottom_right, r.bottom_left]
}

/// The ellipse, capsule or rounded rect that the used `radii` make of `rect`, and whether it
/// took the smaller radius of an elliptical corner.
///
/// Only circular corners make an ellipse or a capsule; other corner styles stay rounded rects,
/// so the glass keeps the painted background's corners.
fn classify(rect: Rect, radii: [Vec2; 4], corner: CornerStyle) -> (ShapeKind, bool) {
    if corner == CornerStyle::Circular {
        let (half_w, half_h) = (rect.width() / 2.0, rect.height() / 2.0);
        let at_least = |r: f64, half: f64| r >= half - RADIUS_EPSILON;
        if radii
            .iter()
            .all(|r| at_least(r.x, half_w) && at_least(r.y, half_h))
        {
            return (ShapeKind::Ellipse(rect), false);
        }
        let half_short = half_w.min(half_h);
        if radii
            .iter()
            .all(|r| (r.x - r.y).abs() <= RADIUS_EPSILON && at_least(r.x, half_short))
        {
            return (ShapeKind::Capsule(rect), false);
        }
    }
    rounded_rect(rect, radii, corner)
}

/// A rounded rect over `rect` with the smaller radius of each corner, and whether a corner
/// was elliptical.
fn rounded_rect(rect: Rect, radii: [Vec2; 4], corner: CornerStyle) -> (ShapeKind, bool) {
    let elliptical = radii.iter().any(|r| (r.x - r.y).abs() > RADIUS_EPSILON);
    let [tl, tr, br, bl] = radii.map(|r| r.x.min(r.y));
    let kind = ShapeKind::RoundedRect {
        rect,
        radii: RoundedRectRadii::new(tl, tr, br, bl),
        corner,
    };
    (kind, elliptical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: ShapeKind, transform: Affine) -> GlassShape {
        GlassShape::new(
            0,
            transform,
            kind,
            GlassMaterial::new(GlassVariant::Regular),
        )
    }

    fn assert_rect_near(a: Rect, b: Rect) {
        let d = [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1];
        assert!(d.iter().all(|d| d.abs() < 1e-9), "{a:?} != {b:?}");
    }

    const RECT: Rect = Rect::new(0.0, 0.0, 100.0, 40.0);

    #[test]
    fn region_is_the_offset_box_scaled_about_its_centre() {
        let transform = Affine::rotate(0.3).then_translate(Vec2::new(5.0, 7.0));
        let rounded = shape(
            ShapeKind::RoundedRect {
                rect: RECT,
                radii: RoundedRectRadii::from_single_radius(4.0),
                corner: CornerStyle::Circular,
            },
            transform,
        );
        let Some(BackdropSource::Region { rect, transform: t }) =
            region_source(&rounded, 2.0, (10.0, -10.0))
        else {
            panic!("no region");
        };
        assert_rect_near(rect, Rect::from_center_size((60.0, 10.0), (50.0, 20.0)));
        assert_eq!(t, transform);

        let path = shape(
            ShapeKind::Path(Arc::new(Rect::new(10.0, 20.0, 30.0, 60.0).to_path(0.1))),
            Affine::IDENTITY,
        );
        let Some(BackdropSource::Region { rect, .. }) = region_source(&path, 1.0, (5.0, 0.0))
        else {
            panic!("no region");
        };
        assert_rect_near(rect, Rect::new(15.0, 20.0, 35.0, 60.0));
    }

    #[test]
    fn region_with_a_non_finite_offset_is_none() {
        let capsule = shape(ShapeKind::Capsule(RECT), Affine::IDENTITY);
        assert!(region_source(&capsule, 1.0, (f32::INFINITY, 0.0)).is_none());
        assert!(region_source(&capsule, 1.0, (0.0, f32::NAN)).is_none());
    }

    #[test]
    fn outlines_follow_each_kind() {
        let capsule = shape_outline(&ShapeKind::Capsule(RECT)).unwrap();
        assert_rect_near(capsule.bounding_box(), RECT);
        assert!(!capsule.contains((1.0, 1.0).into()));
        assert!(capsule.contains((20.0, 20.0).into()));
        assert!(capsule.contains((50.0, 1.0).into()));

        let ellipse = shape_outline(&ShapeKind::Ellipse(RECT)).unwrap();
        assert_rect_near(ellipse.bounding_box(), RECT);
        assert!(!ellipse.contains((10.0, 3.0).into()));
        assert!(ellipse.contains((50.0, 20.0).into()));

        let rounded = shape_outline(&ShapeKind::RoundedRect {
            rect: RECT,
            radii: RoundedRectRadii::from_single_radius(4.0),
            corner: CornerStyle::Continuous,
        })
        .unwrap();
        assert_rect_near(rounded.bounding_box(), RECT);
        assert!(!rounded.contains((0.2, 0.2).into()));
        assert!(rounded.contains((5.0, 1.0).into()));

        let path = Rect::new(10.0, 20.0, 30.0, 60.0).to_path(0.1);
        let outline = shape_outline(&ShapeKind::Path(Arc::new(path.clone()))).unwrap();
        assert_eq!(outline, path);
    }

    #[test]
    fn region_needs_a_magnify_or_an_offset() {
        let mut props = GlassProps::default();
        assert!(!has_region(&props));
        props.magnify = 2.0;
        assert!(has_region(&props));
        props.magnify = 1.0;
        props.source_offset = (0.0, 1.0);
        assert!(has_region(&props));
    }
}
