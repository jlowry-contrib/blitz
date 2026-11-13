//! The curves a box corner can follow, as `corner-shape` sets them.

use kurbo::{
    BezPath, CubicBez, CurveFitSample, Line, ParamCurve, ParamCurveFit, PathSeg, Point, Size, Vec2,
    fit_to_bezpath,
};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
use std::ops::Range;
use std::sync::LazyLock;

/// The largest |K| drawn as a curve; a larger finite K draws as this one.
///
/// At 2^8 the curve is within a few thousandths of the radius of a square corner.
const MAX_K: f32 = 8.0;

/// How far a continuous corner reaches along each edge, per unit radius.
///
/// From PaintCode's construction of the iOS 7 rounded rect,
/// <https://www.paintcodeapp.com/news/code-for-ios-7-rounded-rectangles>.
const CONTINUOUS_EXTENT: f64 = 1.52866483;

/// How far a fitted cubic may stray from the true curve, in device px.
///
/// Half of the 0.1 px tolerance that Vello flattens paths with.
const FIT_ACCURACY: f64 = 0.05;

/// The curve one corner of a CSS box follows between its two edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CornerCurve {
    /// A quarter ellipse, as `border-radius` draws on its own.
    Round,
    /// `corner-shape: superellipse(K)`, with K in [-8, 8] or infinite.
    Superellipse(f32),
    /// A continuous-curvature corner, as `--corner-style: continuous` asks for.
    Continuous,
}

impl CornerCurve {
    /// The curve for a computed `superellipse(K)`.
    ///
    /// K = 1 is round. A finite K is clamped to [-8, 8]; the infinities, `square`
    /// and `notch`, are kept.
    pub fn from_superellipse(k: f32) -> Self {
        if k == 1.0 || k.is_nan() {
            Self::Round
        } else if k.is_infinite() {
            Self::Superellipse(k)
        } else {
            Self::Superellipse(k.clamp(-MAX_K, MAX_K))
        }
    }
}

/// A corner of a contour, in the terms of css-borders-4's border-aligned corner
/// clip-out path.
///
/// The contour runs clockwise: from the edge along `v3` to the edge along `v2`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CornerFrame {
    /// The border radius along `v2`.
    pub start_radius: f64,
    /// The border radius along `v3`.
    pub end_radius: f64,
    /// How far the contour's first edge lies inside the border box's.
    pub start_inset: f64,
    /// How far the contour's second edge lies inside the border box's.
    pub end_inset: f64,
    /// The corner of the contour's rect.
    pub target_outer: Point,
    /// The unit vector from the corner along its first edge.
    pub v3: Vec2,
    /// The unit vector from the corner along its second edge.
    pub v2: Vec2,
}

/// A shaped corner of a contour, split at its diagonal: the half from the first
/// edge, then the half to the second edge. Each starts with a `MoveTo`.
pub(crate) type CornerHalves = (BezPath, BezPath);

/// The contour of a `superellipse(k)` corner, or `None` where the contour keeps
/// the bare corner of its rect.
///
/// This follows css-borders-4 §3.9.4.2, "border-aligned corner clip-out path".
/// The contour is the rect with each corner's clip-out removed; here the curve is
/// returned instead, trimmed to the corner's quadrant of the rect.
/// <https://drafts.csswg.org/css-borders-4/#corner-shape-rendering>
pub(crate) fn superellipse_corner(k: f32, f: &CornerFrame) -> Option<CornerHalves> {
    let (sr, er) = (f.start_radius, f.end_radius);
    let (si, ei) = (f.start_inset, f.end_inset);
    let (v2, v3) = (f.v2, f.v3);
    if sr <= 0.0 || er <= 0.0 || k == f32::INFINITY {
        return None;
    }

    let original_outer = f.target_outer - v3 * ei - v2 * si;
    let original_start = original_outer + v3 * er;
    let original_end = original_outer + v2 * sr;

    let control = ((1.0 / (2f64.sqrt() - 1.0)) * normalized_half_corner(k) - 1.0 / 2f64.sqrt())
        .clamp(0.0, 1.0);
    let inset_diff = (ei - si).clamp(-sr, er);
    if k <= 0.0 && (inset_diff == -sr || inset_diff == er) {
        return None;
    }
    let (mut start_control, mut end_control) = (control, control);
    if inset_diff != 0.0 {
        let root = (sr * sr + er * er - inset_diff * inset_diff).sqrt();
        let bevel_x = er * inset_diff + sr * root;
        let bevel_y = -sr * inset_diff + er * root;
        let bevel_control = (sr * bevel_y) / (sr * bevel_y + er * bevel_x);
        start_control = if k < 0.0 {
            bevel_control * (2.0 * control)
        } else {
            1.0 - (1.0 - bevel_control) * (2.0 * (1.0 - control))
        };
        end_control = 2.0 * control - start_control;
    }
    let unmapped_start = normalize(Vec2::new((1.0 - start_control) * sr, start_control * er));
    let unmapped_end = normalize(Vec2::new(end_control * sr, (1.0 - end_control) * er));
    let start_normal = v3 * unmapped_start.x + v2 * unmapped_start.y;
    let end_normal = v3 * unmapped_end.x + v2 * unmapped_end.y;
    let mut start = original_start + start_normal * si;
    let mut end = original_end + end_normal * ei;
    let start_tangent = -perpendicular(start_normal);
    let end_tangent = perpendicular(end_normal);

    let mut miter_start = start;
    let mut miter_end = end;
    if si < 0.0 {
        let clip_start = f.target_outer + v3;
        miter_start = line_intersection(start, start + start_tangent, clip_start, f.target_outer)
            .unwrap_or(start);
        if k >= 0.0 {
            start = miter_start;
        }
    }
    if ei < 0.0 {
        let clip_end = f.target_outer + v2;
        miter_end =
            line_intersection(end, end + end_tangent, clip_end, f.target_outer).unwrap_or(end);
        if k >= 0.0 {
            end = miter_end;
        }
    }

    let height = (end - start).dot(v2);
    let outer = end - v2 * height;
    let center = start + v2 * height;

    let mut self_intersection = None;
    if k < 0.0 && si < 0.0 && ei < 0.0 {
        if -ei >= sr {
            self_intersection = segment_line_intersection(miter_start, start, miter_end, end);
        }
        if -si >= er && self_intersection.is_none() {
            self_intersection = segment_line_intersection(miter_end, end, miter_start, start);
        }
    }

    let (mut first, mut second) = if let Some(point) = self_intersection {
        (line_path(miter_start, point), line_path(point, miter_end))
    } else if k == f32::NEG_INFINITY {
        (line_path(miter_start, center), line_path(center, miter_end))
    } else {
        let (first, second) = if k > 0.0 || k <= -1.0 {
            halves(k, outer, start - outer, end - outer)
        } else {
            // css-borders-4 draws -1 < K < 0 as the quarter ellipse inside the
            // tangent lines at the two ends.
            let tangents = line_intersection(start, start + start_tangent, end, end + end_tangent)
                .unwrap_or(start);
            halves(1.0, tangents, start - tangents, end - tangents)
        };
        (
            prepend_line(miter_start, first),
            append_line(second, miter_end),
        )
    };

    trim_to_quadrant(&mut first, &mut second, f.target_outer, v3, v2).then_some((first, second))
}

/// The two halves of a corner in the parallelogram `corner`, `corner + along`,
/// `corner + across`, from `corner + along` to `corner + across`.
pub(crate) fn halves(k: f32, corner: Point, along: Vec2, across: Vec2) -> CornerHalves {
    let first = half_corner(k, corner, along, across);
    let second = half_corner(k, corner, across, along).reverse_subpaths();
    (first, second)
}

/// How far each continuous corner of a box blends towards a circular one, from 0
/// (fully continuous) to 1 (circular), in the order top-left, top-right,
/// bottom-right, bottom-left.
///
/// A side shorter than its corners' extents would need blends both its corners
/// towards circular, which they reach once the side is no longer than their radii.
pub(crate) fn continuous_blends(size: Size, radii: [Vec2; 4]) -> [f64; 4] {
    let [tl, tr, br, bl] = radii;
    let blend = |side: f64, a: f64, b: f64| {
        let ratio = side / (a + b);
        if ratio.is_nan() {
            return 0.0;
        }
        ((CONTINUOUS_EXTENT - ratio) / (CONTINUOUS_EXTENT - 1.0)).clamp(0.0, 1.0)
    };
    let top = blend(size.width, tl.x, tr.x);
    let bottom = blend(size.width, bl.x, br.x);
    let left = blend(size.height, tl.y, bl.y);
    let right = blend(size.height, tr.y, br.y);
    [
        top.max(left),
        top.max(right),
        bottom.max(right),
        bottom.max(left),
    ]
}

/// A continuous corner at the box corner `corner`, from `corner + along` to
/// `corner + across` once each is scaled by the corner's extent.
///
/// `along` and `across` are one radius long. `blend` moves it from fully
/// continuous (0) to the round corner (1), as [`continuous_blends`] sets it.
pub(crate) fn continuous_corner(
    blend: f64,
    corner: Point,
    along: Vec2,
    across: Vec2,
) -> CornerHalves {
    let (continuous, circular) = &*CONTINUOUS_CUBICS;
    let map = |i: usize, j: usize| {
        let p = continuous[i][j].lerp(circular[i][j], blend);
        corner + along * p.x + across * p.y
    };
    let cubic = |i: usize| CubicBez::new(map(i, 0), map(i, 1), map(i, 2), map(i, 3));
    let (first, middle, last) = (cubic(0), cubic(1), cubic(2));

    let mut a = BezPath::new();
    a.move_to(first.p0);
    a.curve_to(first.p1, first.p2, first.p3);
    let middle_start = middle.subsegment(0.0..0.5);
    a.curve_to(middle_start.p1, middle_start.p2, middle_start.p3);
    let mut b = BezPath::new();
    let middle_end = middle.subsegment(0.5..1.0);
    b.move_to(middle_end.p0);
    b.curve_to(middle_end.p1, middle_end.p2, middle_end.p3);
    b.curve_to(last.p1, last.p2, last.p3);
    (a, b)
}

/// The control points of three cubics in a row.
type ThreeCubics = [[Point; 4]; 3];

/// The three cubics of a continuous corner and of a round one, per unit radius,
/// as (along, across) offsets from the box corner.
///
/// The round corner is split where its handles keep the continuous corner's
/// ratio, so the blend of the two stays tangent continuous.
static CONTINUOUS_CUBICS: LazyLock<(ThreeCubics, ThreeCubics)> = LazyLock::new(|| {
    let swap = |p: Point| Point::new(p.y, p.x);
    let mirror = |c: [Point; 4]| [swap(c[3]), swap(c[2]), swap(c[1]), swap(c[0])];

    // PaintCode's construction of the iOS 7 rounded rect, for the corner's first
    // third and the start of its middle, per unit radius:
    // <https://www.paintcodeapp.com/news/code-for-ios-7-rounded-rectangles>
    let edge_handle = Point::new(1.08849323, 0.0);
    let inner_handle = Point::new(0.86840689, 0.0);
    let first_end = Point::new(0.66993427, 0.06549600);
    let middle_start = Point::new(0.63149399, 0.07491100);
    let middle_handle = Point::new(0.37282392, 0.16905899);
    // PaintCode joins its first and middle cubics with a short line, turning
    // the tangent by a few degrees at each end. Both cubics meet instead at
    // the line's midpoint, along the line between their handles, which keep
    // their lengths; the curve moves by under 0.007 radius.
    let joint = first_end.midpoint(middle_start);
    let direction = (middle_handle - inner_handle).normalize();
    let handle_in = (first_end - inner_handle).hypot();
    let handle_out = (middle_handle - middle_start).hypot();
    let out = joint + direction * handle_out;
    let first = [
        Point::new(CONTINUOUS_EXTENT, 0.0),
        edge_handle,
        joint - direction * handle_in,
        joint,
    ];
    let continuous = [first, [joint, out, swap(out), swap(joint)], mirror(first)];

    // The quarter circle about (1, 1), cut at `split` from each end, where
    // tan(split / 4) / tan((π/2 - 2·split) / 4) equals handle_in / handle_out.
    let ratio = handle_in / handle_out;
    let (mut low, mut high) = (0.0, FRAC_PI_4);
    for _ in 0..64 {
        let split = 0.5 * (low + high);
        if (split / 4.0).tan() / ((FRAC_PI_2 - 2.0 * split) / 4.0).tan() < ratio {
            low = split;
        } else {
            high = split;
        }
    }
    let split = 0.5 * (low + high);
    let at = |theta: f64| Point::new(1.0 + theta.cos(), 1.0 + theta.sin());
    let tangent = |theta: f64| Vec2::new(-theta.sin(), theta.cos());
    let arc = |from: f64, to: f64| {
        let handle = 4.0 / 3.0 * ((to - from) / 4.0).tan();
        [
            at(from),
            at(from) + tangent(from) * handle,
            at(to) - tangent(to) * handle,
            at(to),
        ]
    };
    let (start, end) = (-FRAC_PI_2, -PI);
    let circular = [
        arc(start, start - split),
        arc(start - split, end + split),
        arc(end + split, end),
    ];
    (continuous, circular)
});

/// css-borders-4's normalized superellipse half corner.
fn normalized_half_corner(k: f32) -> f64 {
    if k == f32::NEG_INFINITY {
        return 0.0;
    }
    if k == f32::INFINITY {
        return 1.0;
    }
    let n = 2f64.powf(f64::from(k).abs());
    let convex = 0.5f64.powf(1.0 / n);
    if k < 0.0 { 1.0 - convex } else { convex }
}

/// Half of a superellipse corner, from `corner + along` to the diagonal.
///
/// The parallelogram's corner is the curve's centre for K < 0, and the opposite
/// corner is for K > 0. Every curve here is symmetric about the parallelogram's
/// diagonal, so swapping `along` and `across` gives the other half.
fn half_corner(k: f32, corner: Point, along: Vec2, across: Vec2) -> BezPath {
    if k == f32::NEG_INFINITY {
        return line_path(corner + along, corner + along + across);
    }
    if k == f32::INFINITY {
        return line_path(corner + along, corner);
    }
    fit_to_bezpath(
        &HalfSuperellipse::new(k, corner, along, across),
        FIT_ACCURACY,
    )
}

/// Half of a `superellipse(K)` corner, mapped as css-borders-4 maps it.
///
/// With n = 2^|K| the curve is x^n + y^n = 1 about its centre.
struct HalfSuperellipse {
    n: f64,
    convex: bool,
    /// The diagonal's coordinate, 0.5^(1/n).
    half: f64,
    corner: Point,
    along: Vec2,
    across: Vec2,
}

impl HalfSuperellipse {
    fn new(k: f32, corner: Point, along: Vec2, across: Vec2) -> Self {
        let n = 2f64.powf(f64::from(k).abs());
        Self {
            n,
            convex: k > 0.0,
            half: 0.5f64.powf(1.0 / n),
            corner,
            along,
            across,
        }
    }
}

impl ParamCurveFit for HalfSuperellipse {
    fn sample_pt_tangent(&self, t: f64, _sign: f64) -> CurveFitSample {
        let (p, tangent) = self.sample_pt_deriv(t);
        CurveFitSample { p, tangent }
    }

    fn sample_pt_deriv(&self, t: f64) -> (Point, Vec2) {
        // The parameter is the coordinate b off the starting edge, from 0 to the
        // diagonal. Since n >= 1, the other coordinate a = (1 - b^n)^(1/n) has a
        // bounded derivative over the whole half, so the fit sees no cusp.
        let n = self.n;
        let b = t * self.half;
        let bn = b.powf(n);
        let a = (1.0 - bn).max(0.0).powf(1.0 / n);
        let da_db = if b == 0.0 {
            if n == 1.0 { -1.0 } else { 0.0 }
        } else {
            -(bn / b) * (1.0 - bn).max(f64::MIN_POSITIVE).powf(1.0 / n - 1.0)
        };
        let (u, v, du, dv) = if self.convex {
            (1.0 - b, 1.0 - a, -1.0, -da_db)
        } else {
            (a, b, da_db, 1.0)
        };
        let p = self.corner + self.along * u + self.across * v;
        let d = (self.along * du + self.across * dv) * self.half;
        (p, d)
    }

    fn break_cusp(&self, _: Range<f64>) -> Option<f64> {
        None
    }
}

/// Keeps the part of a corner inside the quadrant at `outer` spanned by `v3` and
/// `v2`, which the rest of the contour's rect lies in. Returns `false` when no
/// part is inside.
fn trim_to_quadrant(
    first: &mut BezPath,
    second: &mut BezPath,
    outer: Point,
    v3: Vec2,
    v2: Vec2,
) -> bool {
    // Rounding puts points on an edge a hair outside it.
    const SLACK: f64 = 1e-6;
    let inside = |p: Point| (p - outer).dot(v2) >= -SLACK && (p - outer).dot(v3) >= -SLACK;

    let segs: Vec<PathSeg> = first.segments().chain(second.segments()).collect();
    let split = first.segments().count() as f64;
    if segs.is_empty() {
        let point = first
            .elements()
            .first()
            .and_then(|el| el.end_point())
            .unwrap_or(outer);
        return inside(point);
    }
    let eval = |t: f64| {
        let i = (t.floor() as usize).min(segs.len() - 1);
        segs[i].eval(t - i as f64)
    };

    // Every parameter where the corner crosses an edge line of the quadrant.
    let reach = segs
        .iter()
        .map(|seg| (seg.start() - outer).hypot() + (seg.end() - outer).hypot())
        .sum::<f64>()
        + 1.0;
    let mut cuts = vec![0.0, segs.len() as f64];
    for dir in [v3, v2] {
        let line = Line::new(outer - dir * reach, outer + dir * reach);
        for (i, seg) in segs.iter().enumerate() {
            cuts.extend(
                seg.intersect_line(line)
                    .iter()
                    .map(|hit| i as f64 + hit.segment_t),
            );
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();

    let spans: Vec<(f64, f64)> = cuts
        .windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|&(a, b)| b > a && inside(eval(0.5 * (a + b))))
        .collect();
    let (Some(&(t0, _)), Some(&(_, t1))) = (spans.first(), spans.last()) else {
        return false;
    };
    let mid = split.clamp(t0, t1);
    *first = sub_path(&segs, t0, mid);
    *second = sub_path(&segs, mid, t1);
    true
}

/// The part of a chain of segments between two chain parameters.
fn sub_path(segs: &[PathSeg], t0: f64, t1: f64) -> BezPath {
    let at = |t: f64| {
        let i = (t.floor() as usize).min(segs.len() - 1);
        (i, t - i as f64)
    };
    let (i0, s0) = at(t0);
    let mut path = BezPath::new();
    path.move_to(segs[i0].eval(s0));
    if t1 <= t0 {
        return path;
    }
    let (i1, s1) = at(t1);
    for (i, seg) in segs.iter().enumerate().take(i1 + 1).skip(i0) {
        let a = if i == i0 { s0 } else { 0.0 };
        let b = if i == i1 { s1 } else { 1.0 };
        if b > a {
            path.push(seg.subsegment(a..b).as_path_el());
        }
    }
    path
}

fn line_path(from: Point, to: Point) -> BezPath {
    let mut path = BezPath::new();
    path.move_to(from);
    path.line_to(to);
    path
}

fn prepend_line(from: Point, path: BezPath) -> BezPath {
    let mut out = BezPath::new();
    out.move_to(from);
    let mut els = path.into_iter();
    if let Some(start) = els.next().and_then(|el| el.end_point()) {
        if start != from {
            out.line_to(start);
        }
    }
    out.extend(els);
    out
}

fn append_line(mut path: BezPath, to: Point) -> BezPath {
    if path.elements().last().and_then(|el| el.end_point()) != Some(to) {
        path.line_to(to);
    }
    path
}

fn normalize(v: Vec2) -> Vec2 {
    let length = v.hypot();
    if length == 0.0 { v } else { v / length }
}

fn perpendicular(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

/// Where the line through `a0` and `a1` meets the line through `b0` and `b1`.
fn line_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Option<Point> {
    let (da, db) = (a1 - a0, b1 - b0);
    let denom = da.cross(db);
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = (b0 - a0).cross(db) / denom;
    Some(a0 + da * t)
}

/// Where the segment from `a0` to `a1` meets the line through `b0` and `b1`.
fn segment_line_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Option<Point> {
    let (da, db) = (a1 - a0, b1 - b0);
    let denom = da.cross(db);
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = (b0 - a0).cross(db) / denom;
    (0.0..=1.0).contains(&t).then(|| a0 + da * t)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kurbo_css::{CssBox, Edge, NonUniformRoundedRectRadii};
    use kurbo::{
        Arc, Insets, ParamCurve, ParamCurveArclen, PathEl, PathSeg, Point, Rect, Shape, Vec2,
    };
    use std::f64::consts::{FRAC_PI_2, PI};

    const R: f64 = 50.0;

    fn radii(r: f64) -> NonUniformRoundedRectRadii {
        let v = Vec2::new(r, r);
        NonUniformRoundedRectRadii {
            top_left: v,
            top_right: v,
            bottom_right: v,
            bottom_left: v,
        }
    }

    fn square_box(curve: CornerCurve) -> CssBox {
        CssBox::new(
            Rect::new(0.0, 0.0, 200.0, 200.0),
            Insets::uniform(4.0),
            Insets::uniform(3.0),
            2.0,
            radii(R),
        )
        .with_corner_curves([curve; 4])
    }

    /// Points along every segment of `path`.
    fn samples(path: &BezPath) -> Vec<Point> {
        path.segments()
            .flat_map(|seg| (0..=32).map(move |i| seg.eval(f64::from(i) / 32.0)))
            .collect()
    }

    fn all_points(path: &BezPath) -> Vec<Point> {
        path.elements()
            .iter()
            .flat_map(|el| match *el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => vec![p],
                PathEl::QuadTo(a, b) => vec![a, b],
                PathEl::CurveTo(a, b, c) => vec![a, b, c],
                PathEl::ClosePath => vec![],
            })
            .collect()
    }

    #[test]
    fn superellipse_corner_lies_on_its_curve() {
        for k in [-1.0_f32, 0.0, 1.0, 2.0, 4.0] {
            let path = square_box(CornerCurve::Superellipse(k)).border_box_path();
            let n = 2f64.powf(f64::from(k).abs());
            // Convex corners centre on the radius point, concave ones on the box corner.
            let centre = if k < 0.0 {
                Point::ZERO
            } else {
                Point::new(R, R)
            };
            let mut checked = 0;
            for p in samples(&path) {
                // Only the top-left corner's curve, away from the straight edges.
                if p.x >= R - 1e-9 || p.y >= R - 1e-9 {
                    continue;
                }
                let q = p - centre;
                let rho = ((q.x.abs() / R).powf(n) + (q.y.abs() / R).powf(n)).powf(1.0 / n);
                // Distance to the curve along the ray from its centre.
                let error = q.hypot() * (1.0 - 1.0 / rho).abs();
                assert!(error <= 0.1, "K={k}: {p:?} is {error} px off the curve");
                checked += 1;
            }
            assert!(checked > 10, "K={k}: no corner points sampled");
        }
    }

    #[test]
    fn round_is_unchanged() {
        let path = square_box(CornerCurve::Round).border_box_path();

        let mut expected = BezPath::new();
        let corners = [
            (Point::new(R, R), -FRAC_PI_2),
            (Point::new(200.0 - R, R), 0.0),
            (Point::new(200.0 - R, 200.0 - R), FRAC_PI_2),
            (Point::new(R, 200.0 - R), PI),
        ];
        for (centre, offset) in corners {
            let arc = Arc::new(centre, (R, R), offset + PI + FRAC_PI_2, FRAC_PI_2, 0.0);
            let mut els = arc.path_elements(0.1);
            match els.next() {
                Some(PathEl::MoveTo(p)) if !expected.elements().is_empty() => {
                    expected.push(PathEl::LineTo(p));
                }
                Some(el) => expected.push(el),
                None => {}
            }
            expected.extend(els);
        }
        assert_eq!(path, expected);
        assert!(
            square_box(CornerCurve::from_superellipse(1.0)).border_box_path() == expected,
            "superellipse(1) is round"
        );
    }

    #[test]
    fn notch_and_square_are_finite() {
        for k in [f32::NEG_INFINITY, f32::INFINITY] {
            let b = square_box(CornerCurve::from_superellipse(k));
            let mut paths = vec![
                b.border_box_path(),
                b.padding_box_path(),
                b.content_box_path(),
                b.outline(),
                b.shadow_clip(Rect::new(-50.0, -50.0, 250.0, 250.0)),
            ];
            for edge in [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left] {
                paths.push(b.border_edge_shape(edge));
            }
            for path in &paths {
                for p in all_points(path) {
                    assert!(p.is_finite(), "K={k}: non-finite point {p:?}");
                }
            }
            let mut border = b.border_box_path();
            border.close_path();
            if k > 0.0 {
                // square: the corner itself
                assert!(all_points(&border).contains(&Point::ZERO));
            } else {
                // notch: the corner is cut back to the radius point
                assert!(all_points(&border).contains(&Point::new(R, R)));
                assert!(!border.contains(Point::new(R / 2.0, R / 2.0)));
                assert!(border.contains(Point::new(R + 1.0, R + 1.0)));
            }
        }
    }

    fn continuous_box(width: f64) -> CssBox {
        CssBox::new(
            Rect::new(0.0, 0.0, width, 400.0),
            Insets::uniform(4.0),
            Insets::ZERO,
            0.0,
            radii(R),
        )
        .with_corner_curves([CornerCurve::Continuous; 4])
    }

    #[test]
    fn continuous_corner_starts_at_its_extent() {
        let path = continuous_box(400.0).border_box_path();
        let extent = 1.52866483 * R;
        let ends = path.elements().iter().filter_map(|el| el.end_point());
        let top: Vec<f64> = ends.filter(|p| p.y.abs() < 1e-9).map(|p| p.x).collect();
        let first = top.iter().copied().fold(f64::INFINITY, f64::min);
        let last = top.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!((first - extent).abs() < 1e-6, "top edge starts at {first}");
        assert!(
            (last - (400.0 - extent)).abs() < 1e-6,
            "top edge ends at {last}"
        );
        // and nothing but the edge reaches it
        for p in samples(&path) {
            if p.y < 1e-9 {
                assert!(
                    p.x >= extent - 1e-6 && p.x <= 400.0 - extent + 1e-6,
                    "{p:?}"
                );
            }
        }
    }

    /// The largest turn of the tangent, in degrees, where one segment meets the next.
    fn largest_turn(path: &BezPath) -> f64 {
        // Joining lines of no length don't turn anything.
        let segs: Vec<PathSeg> = path
            .segments()
            .filter(|seg| seg.arclen(1e-9) > 1e-9)
            .collect();
        segs.windows(2)
            .map(|pair| {
                // The first control point that differs from the end, as a renderer
                // takes the tangent.
                let tangent = |points: &[Point]| {
                    points[1..]
                        .iter()
                        .map(|p| *p - points[0])
                        .find(|v| v.hypot() > 1e-9)
                        .unwrap_or(Vec2::ZERO)
                };
                let points = |seg: PathSeg| match seg {
                    PathSeg::Line(l) => vec![l.p0, l.p1],
                    PathSeg::Quad(q) => vec![q.p0, q.p1, q.p2],
                    PathSeg::Cubic(c) => vec![c.p0, c.p1, c.p2, c.p3],
                };
                let mut before = points(pair[0]);
                before.reverse();
                let out = -tangent(&before);
                let into = tangent(&points(pair[1]));
                (out.cross(into)).atan2(out.dot(into)).abs().to_degrees()
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn continuous_corner_is_tangent_continuous() {
        // A long side draws the full corner, a shorter one a blend towards the circle.
        for width in [400.0, 2.5 * R, 2.0 * R] {
            let path = continuous_box(width).border_box_path();
            let turn = largest_turn(&path);
            assert!(
                turn < 0.5,
                "width {width}: the tangent turns {turn} degrees"
            );
        }
    }

    #[test]
    fn continuous_corner_is_circular_on_short_sides() {
        // At a side of 2r the corner is the quarter circle of the round corner.
        let path = continuous_box(2.0 * R).border_box_path();
        for p in samples(&path) {
            if p.x < R && p.y < R {
                let error = ((p - Point::new(R, R)).hypot() - R).abs();
                assert!(error < 0.01, "{p:?} is {error} px off the circle");
            }
        }
    }
}
