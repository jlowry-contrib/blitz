//! Builds the glass shape and group of an element with `--glass`.

use anyrender::glass::{CornerStyle, GlassMaterial, GlassShape, GlassVariant, ShapeKind};
use blitz_dom::glass::{GlassProps, GlassVariant as PropVariant};
use kurbo::{Affine, Rect, RoundedRectRadii, Vec2};

use crate::kurbo_css::{CssBox, corner_style};

/// Slack, in CSS px, for radii that the overlap scaling of css-backgrounds-3 §5.5 leaves a
/// rounding error away from half a side.
const RADIUS_EPSILON: f64 = 1e-6;

/// The glass shape of an element whose border box is `frame`, painted under `transform`.
///
/// `frame` and `transform` are in device px, as Blitz paints. The shape is in the element's
/// border-box CSS px, origin at its top-left, and its transform is `S⁻¹ · transform · S`
/// with `S` the document scale, which the glass backend applies itself. Returns `None` for a
/// zero-size box or a non-invertible transform.
pub(crate) fn glass_shape(
    frame: &CssBox,
    props: &GlassProps,
    transform: Affine,
    scale: f64,
    id: u32,
) -> Option<GlassShape> {
    let size = frame.border_box.size() / scale;
    if !(size.width > 0.0 && size.height > 0.0) {
        return None;
    }
    let det = transform.determinant();
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let variant = props.variant?;

    let rect = Rect::from_origin_size((0.0, 0.0), size);
    let kind = shape_kind(frame, rect, scale);
    let transform = Affine::scale(scale.recip()) * transform * Affine::scale(scale);

    let mut material = GlassMaterial::new(match variant {
        PropVariant::Regular => GlassVariant::Regular,
        PropVariant::Clear => GlassVariant::Clear,
        PropVariant::Identity => GlassVariant::Identity,
    });
    material.tint = props.tint;
    material.interactive = props.interactive;

    Some(GlassShape::new(id, transform, kind, material))
}

/// Whether a rounded-rect glass shape over `frame` had to round an elliptical corner down.
#[cfg(feature = "tracing")]
pub(crate) fn has_elliptical_corner(frame: &CssBox) -> bool {
    used_radii(frame, 1.0)
        .iter()
        .any(|r| (r.x - r.y).abs() > RADIUS_EPSILON)
}

/// The used radii, after overlap scaling, in the order top-left, top-right, bottom-right,
/// bottom-left, divided by `scale`.
fn used_radii(frame: &CssBox, scale: f64) -> [Vec2; 4] {
    let r = &frame.border_radii;
    [r.top_left, r.top_right, r.bottom_right, r.bottom_left].map(|v| v / scale)
}

/// The ellipse, capsule or rounded rect that `frame`'s used radii make of `rect`.
///
/// Only circular corners make an ellipse or a capsule; other corner styles stay rounded rects,
/// so the glass keeps the painted background's corners.
fn shape_kind(frame: &CssBox, rect: Rect, scale: f64) -> ShapeKind {
    let radii = used_radii(frame, scale);
    let corner = corner_style(frame);

    if corner == CornerStyle::Circular {
        let (half_w, half_h) = (rect.width() / 2.0, rect.height() / 2.0);
        let at_least = |r: f64, half: f64| r >= half - RADIUS_EPSILON;
        if radii
            .iter()
            .all(|r| at_least(r.x, half_w) && at_least(r.y, half_h))
        {
            return ShapeKind::Ellipse(rect);
        }
        let half_short = half_w.min(half_h);
        if radii
            .iter()
            .all(|r| (r.x - r.y).abs() <= RADIUS_EPSILON && at_least(r.x, half_short))
        {
            return ShapeKind::Capsule(rect);
        }
    }

    let [tl, tr, br, bl] = radii.map(|r| r.x.min(r.y));
    ShapeKind::RoundedRect {
        rect,
        radii: RoundedRectRadii::new(tl, tr, br, bl),
        corner,
    }
}
