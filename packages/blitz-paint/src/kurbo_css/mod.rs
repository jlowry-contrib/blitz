//! A rounded rect closer to the browser
//! Implemented in such a way that splits the border into 4 parts at the midway of each radius
//!
//! This object is meant to be updated only when the data changes - BezPaths are expensive!
//!
//! Can I just say, this is a lot of work for a border
//! HTML/css is annoyingly wild

use anyrender::glass::CornerStyle;
use kurbo::{Insets, Vec2};

mod corner_curve;
mod css_box;
mod non_uniform_radii;

pub use corner_curve::CornerCurve;
pub use css_box::CssBox;
pub use non_uniform_radii::NonUniformRoundedRectRadii;

#[derive(Debug, Clone, Copy)]
pub enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(clippy::enum_variant_names, reason = "Use CSS standard terminology")]
pub(crate) enum CssBoxKind {
    OutlineBox,
    BorderBox,
    PaddingBox,
    ContentBox,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Direction {
    Clockwise,
    Anticlockwise,
}

fn add_insets(a: Insets, b: Insets) -> Insets {
    Insets {
        x0: a.x0 + b.x0,
        y0: a.y0 + b.y0,
        x1: a.x1 + b.x1,
        y1: a.y1 + b.y1,
    }
}

#[inline(always)]
fn get_corner_insets(insets: Insets, corner: Corner) -> Vec2 {
    match corner {
        Corner::TopLeft => Vec2 {
            x: insets.x0,
            y: insets.y0,
        },
        Corner::TopRight => Vec2 {
            x: insets.x1,
            y: insets.y0,
        },
        Corner::BottomLeft => Vec2 {
            x: insets.x0,
            y: insets.y1,
        },
        Corner::BottomRight => Vec2 {
            x: insets.x1,
            y: insets.y1,
        },
    }
}

/// The corner style a glass shape drawn over `css_box` takes.
///
/// `--corner-style: continuous` comes first, as it overrides `corner-shape`. Then any corner
/// that isn't round makes it a superellipse, with K clamped to [-8, 8].
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "glass shapes read it once blitz-paint emits glass"
    )
)]
pub(crate) fn corner_style(css_box: &CssBox) -> CornerStyle {
    let curves = css_box.corner_curves;
    if curves.contains(&CornerCurve::Continuous) {
        return CornerStyle::Continuous;
    }
    if css_box.has_round_corners() {
        return CornerStyle::Circular;
    }
    CornerStyle::Superellipse(curves.map(|curve| match curve {
        CornerCurve::Superellipse(k) => k.clamp(-8.0, 8.0),
        CornerCurve::Round | CornerCurve::Continuous => 1.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Rect;

    fn css_box(curves: [CornerCurve; 4]) -> CssBox {
        let radius = Vec2::new(10.0, 10.0);
        let radii = NonUniformRoundedRectRadii {
            top_left: radius,
            top_right: radius,
            bottom_right: radius,
            bottom_left: radius,
        };
        CssBox::new(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            Insets::ZERO,
            Insets::ZERO,
            0.0,
            radii,
        )
        .with_corner_curves(curves)
    }

    #[test]
    fn corner_style_prefers_continuous() {
        use CornerCurve::*;
        assert_eq!(corner_style(&css_box([Round; 4])), CornerStyle::Circular);
        assert_eq!(
            corner_style(&css_box([Continuous; 4])),
            CornerStyle::Continuous
        );
        assert_eq!(
            corner_style(&css_box([
                Superellipse(2.0),
                Round,
                Superellipse(f32::INFINITY),
                Superellipse(f32::NEG_INFINITY),
            ])),
            CornerStyle::Superellipse([2.0, 1.0, 8.0, -8.0])
        );
    }
}
