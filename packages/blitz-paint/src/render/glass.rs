//! Builds the glass shape and group of an element with `--glass`.

use std::sync::Arc;

use anyrender::glass::{CornerStyle, GlassMaterial, GlassShape, GlassVariant, ShapeKind};
use blitz_dom::glass::{GlassProps, GlassVariant as PropVariant};
use kurbo::{Affine, Insets, Rect, RoundedRectRadii, Shape, Vec2};

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

    Some(BuiltShape {
        shape: GlassShape::new(id, transform, kind, material),
        rounded_elliptical,
        evenodd,
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
