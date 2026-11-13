//! Typed reading of the glass custom properties (`--glass` and its companions).

use std::fmt;
use std::sync::LazyLock;

use cssparser::{BasicParseError, ParseError, Parser, Token, match_ignore_ascii_case};
use selectors::matching::QuirksMode;
use smallvec::SmallVec;
use style::Atom;
use style::parser::{Parse as _, ParserContext};
use style::properties::ComputedValues;
use style::servo_arc::Arc as ServoArc;
use style::stylesheets::{CssRuleType, Origin, UrlExtraData};
use style::values::specified::Color as SpecifiedColor;
use style_traits::{CssWriter, ParsingMode, ToCss as _};
use stylo_taffy::CopyStyleGetters as _;
use url::Url;

use crate::util::{Color, ToColorColor as _};

/// The glass material an element asks for with `--glass`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlassVariant {
    /// `--glass: regular`.
    Regular,
    /// `--glass: clear`.
    Clear,
    /// `--glass: identity`.
    Identity,
}

/// The corner curve asked for with `--corner-style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CornerStyleProp {
    /// `--corner-style: circular`.
    Circular,
    /// `--corner-style: continuous`.
    Continuous,
}

/// The morph geometry from `--glass-x`, `-y`, `-w`, `-h` and `-r`, in border-box CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryOverride {
    /// The shape's centre x.
    pub x: f32,
    /// The shape's centre y.
    pub y: f32,
    /// The shape's full width.
    pub w: f32,
    /// The shape's full height.
    pub h: f32,
    /// The radius of every corner.
    pub r: f32,
}

/// The glass custom properties of one element, parsed into typed fields.
#[derive(Clone, Debug, PartialEq)]
pub struct GlassProps {
    /// `--glass`; `None` for `none`, which leaves the element without glass.
    pub variant: Option<GlassVariant>,
    /// `--glass-tint`; `None` when absent or fully transparent.
    pub tint: Option<Color>,
    /// `--glass-interactive` other than 0.
    pub interactive: bool,
    /// `--glass-container`, the merge spacing in CSS px; `None` for `none`.
    pub container: Option<f32>,
    /// `--glass-union`; `None` for `none`.
    pub union: Option<Atom>,
    /// `--glass-press`, clamped to [0, 1].
    pub press: f32,
    /// `--glass-touch-x` and `--glass-touch-y` in CSS px; `None` unless `press` is positive.
    pub touch: Option<(f32, f32)>,
    /// `--glass-materialize`, clamped to [0, 1].
    pub materialize: f32,
    /// The morph overrides; `Some` only when `--glass-w` and `--glass-h` are both positive.
    pub geometry: Option<GeometryOverride>,
    /// `--glass-source`, the `id` of the portal source; `None` for `none`.
    pub source: Option<Atom>,
    /// `--glass-source-hidden` other than 0.
    pub source_hidden: bool,
    /// `--glass-magnify`; 1 unless positive.
    pub magnify: f32,
    /// The first two lengths of `--glass-source-offset` in CSS px; a missing second is 0.
    pub source_offset: (f32, f32),
    /// `--corner-style`.
    pub corner_style: Option<CornerStyleProp>,
    /// The names of the properties that didn't parse, which read as their defaults.
    pub invalid: SmallVec<[&'static str; 2]>,
}

impl Default for GlassProps {
    fn default() -> Self {
        Self {
            variant: None,
            tint: None,
            interactive: false,
            container: None,
            union: None,
            press: 0.0,
            touch: None,
            materialize: 1.0,
            geometry: None,
            source: None,
            source_hidden: false,
            magnify: 1.0,
            source_offset: (0.0, 0.0),
            corner_style: None,
            invalid: SmallVec::new(),
        }
    }
}

impl GlassProps {
    /// Reads the glass properties from `style`, registered or not.
    ///
    /// `parent` is the parent element's style. An unregistered value that `style` inherited
    /// from it reads as unset, since registered glass properties don't inherit.
    ///
    /// Unregistered, an element that redeclares exactly its parent's value may read as unset
    /// too, because Stylo can keep the parent's shared value for an equal declaration.
    pub fn read(style: &ComputedValues, parent: Option<&ComputedValues>) -> Self {
        let mut r = Reader {
            style,
            parent,
            invalid: SmallVec::new(),
        };
        let defaults = Self::default();

        let variant = r.read(Prop::Glass, parse_variant).flatten();
        let tint = r
            .read(Prop::Tint, |p| parse_color(style, p))
            .filter(|tint| tint.components[3] > 0.0);
        let interactive = r.read(Prop::Interactive, parse_number).unwrap_or(0.0) != 0.0;
        let container = r.read(Prop::Container, parse_spacing).flatten();
        let union = r.read(Prop::Union, parse_ident_or_none).flatten();
        let press = r
            .read(Prop::Press, parse_number)
            .map_or(defaults.press, |v| v.clamp(0.0, 1.0));
        let touch_x = r.read(Prop::TouchX, parse_length).unwrap_or(0.0);
        let touch_y = r.read(Prop::TouchY, parse_length).unwrap_or(0.0);
        let touch = (press > 0.0).then_some((touch_x, touch_y));
        let materialize = r
            .read(Prop::Materialize, parse_number)
            .map_or(defaults.materialize, |v| v.clamp(0.0, 1.0));

        let x = r.read(Prop::X, parse_length).unwrap_or(0.0);
        let y = r.read(Prop::Y, parse_length).unwrap_or(0.0);
        let w = r.read(Prop::W, parse_length).unwrap_or(0.0);
        let h = r.read(Prop::H, parse_length).unwrap_or(0.0);
        let radius = r.read(Prop::R, parse_length).unwrap_or(0.0);
        let geometry = (w > 0.0 && h > 0.0).then_some(GeometryOverride {
            x,
            y,
            w,
            h,
            r: radius.max(0.0),
        });

        let source = r.read(Prop::Source, parse_ident_or_none).flatten();
        let source_hidden = r.read(Prop::SourceHidden, parse_number).unwrap_or(0.0) != 0.0;
        let magnify = r
            .read(Prop::Magnify, parse_number)
            .filter(|m| *m > 0.0)
            .unwrap_or(defaults.magnify);
        let source_offset = r
            .read(Prop::SourceOffset, parse_offset)
            .unwrap_or(defaults.source_offset);
        let corner_style = r.read(Prop::CornerStyle, parse_corner_style);

        Self {
            variant,
            tint,
            interactive,
            container,
            union,
            press,
            touch,
            materialize,
            geometry,
            source,
            source_hidden,
            magnify,
            source_offset,
            corner_style,
            invalid: r.invalid,
        }
    }
}

/// Reads only `--corner-style`, as [`GlassProps::read`] reads it.
pub fn read_corner_style(
    style: &ComputedValues,
    parent: Option<&ComputedValues>,
) -> Option<CornerStyleProp> {
    let mut r = Reader {
        style,
        parent,
        invalid: SmallVec::new(),
    };
    r.read(Prop::CornerStyle, parse_corner_style)
}

/// Whether `style` has a `--glass` other than `none` of its own, not inherited from `parent`.
pub(crate) fn has_glass(style: &ComputedValues, parent: Option<&ComputedValues>) -> bool {
    let mut r = Reader {
        style,
        parent,
        invalid: SmallVec::new(),
    };
    matches!(r.read(Prop::Glass, parse_variant), Some(Some(_)))
}

/// Whether `--glass` or `--glass-container` makes a stacking context of only one of `old` and
/// `new`, read against `parent` and, to catch a value that newly matches an inherited one, alone.
pub(crate) fn glass_role_changed(
    old: &ComputedValues,
    new: &ComputedValues,
    parent: Option<&ComputedValues>,
) -> bool {
    let role = |style, parent| has_glass(style, parent) || glass_container(style, parent).is_some();
    role(old, parent) != role(new, parent) || role(old, None) != role(new, None)
}

/// `style`'s own `--glass-container` spacing in CSS px, as [`GlassProps::read`] reads it.
pub(crate) fn glass_container(
    style: &ComputedValues,
    parent: Option<&ComputedValues>,
) -> Option<f32> {
    let mut r = Reader {
        style,
        parent,
        invalid: SmallVec::new(),
    };
    r.read(Prop::Container, parse_spacing).flatten()
}

/// The glass properties `GlassProps` reads, indexing `NAMES` and `ATOMS`.
#[derive(Clone, Copy)]
enum Prop {
    Glass,
    Tint,
    Interactive,
    Container,
    Union,
    Press,
    TouchX,
    TouchY,
    Materialize,
    X,
    Y,
    W,
    H,
    R,
    Source,
    SourceHidden,
    Magnify,
    SourceOffset,
    CornerStyle,
}

const NAMES: [&str; 19] = [
    "--glass",
    "--glass-tint",
    "--glass-interactive",
    "--glass-container",
    "--glass-union",
    "--glass-press",
    "--glass-touch-x",
    "--glass-touch-y",
    "--glass-materialize",
    "--glass-x",
    "--glass-y",
    "--glass-w",
    "--glass-h",
    "--glass-r",
    "--glass-source",
    "--glass-source-hidden",
    "--glass-magnify",
    "--glass-source-offset",
    "--corner-style",
];

/// The names as Stylo keys them, without the leading `--`, interned once.
static ATOMS: LazyLock<[Atom; 19]> =
    LazyLock::new(|| NAMES.map(|name| Atom::from(name.trim_start_matches('-'))));

struct Reader<'a> {
    style: &'a ComputedValues,
    parent: Option<&'a ComputedValues>,
    invalid: SmallVec<[&'static str; 2]>,
}

impl Reader<'_> {
    /// Parses the value of `prop`, or records it as invalid and returns `None`.
    ///
    /// Returns `None` too when the property is absent, empty or inherited.
    fn read<T>(
        &mut self,
        prop: Prop,
        parse: impl FnOnce(&mut Parser<'_>) -> Parsed<T>,
    ) -> Option<T> {
        let atom = &ATOMS[prop as usize];
        let value = self.style.custom_properties().get_for_cssom(atom)?;
        let mut buffer = CssText::default();
        let text = match value.as_universal() {
            Some(tokens) => {
                let inherited = self
                    .parent
                    .and_then(|parent| parent.custom_properties().get_for_cssom(atom))
                    .and_then(|parent| parent.as_universal())
                    .is_some_and(|parent| ServoArc::ptr_eq(parent, tokens));
                if inherited {
                    return None;
                }
                tokens.css_text()
            }
            None => {
                value.to_css(&mut CssWriter::new(&mut buffer)).ok()?;
                buffer.as_str()
            }
        };
        if text.trim().is_empty() {
            return None;
        }
        let parsed = Parser::new(text).parse_entirely(parse).ok();
        if parsed.is_none() {
            self.invalid.push(NAMES[prop as usize]);
        }
        parsed
    }
}

/// A serialised computed value, on the stack while it fits in 64 bytes.
#[derive(Default)]
struct CssText {
    inline: SmallVec<[u8; 64]>,
}

impl CssText {
    fn as_str(&self) -> &str {
        // Only `write_str` fills the buffer, so it holds whole UTF-8 strings.
        std::str::from_utf8(&self.inline).unwrap_or_default()
    }
}

impl fmt::Write for CssText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.inline.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

type Parsed<T> = Result<T, ParseError<()>>;

fn unexpected() -> ParseError<()> {
    BasicParseError::unexpected_token().into()
}

fn parse_variant(p: &mut Parser<'_>) -> Parsed<Option<GlassVariant>> {
    let ident = p.expect_ident()?;
    Ok(match_ignore_ascii_case! { ident,
        "none" => None,
        "regular" => Some(GlassVariant::Regular),
        "clear" => Some(GlassVariant::Clear),
        "identity" => Some(GlassVariant::Identity),
        _ => return Err(unexpected()),
    })
}

fn parse_corner_style(p: &mut Parser<'_>) -> Parsed<CornerStyleProp> {
    let ident = p.expect_ident()?;
    Ok(match_ignore_ascii_case! { ident,
        "circular" => CornerStyleProp::Circular,
        "continuous" => CornerStyleProp::Continuous,
        _ => return Err(unexpected()),
    })
}

fn parse_ident_or_none(p: &mut Parser<'_>) -> Parsed<Option<Atom>> {
    let ident = p.expect_ident()?;
    Ok((!ident.eq_ignore_ascii_case("none")).then(|| Atom::from(&**ident)))
}

fn parse_number(p: &mut Parser<'_>) -> Parsed<f32> {
    Ok(p.expect_number()?)
}

/// A length in px, or a unitless 0. Other units only work through a registration.
fn parse_length(p: &mut Parser<'_>) -> Parsed<f32> {
    match p.next()? {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("px") => Ok(*value),
        Token::Number { value, .. } if *value == 0.0 => Ok(0.0),
        _ => Err(unexpected()),
    }
}

/// `none`, or a merge spacing of at least 0.
fn parse_spacing(p: &mut Parser<'_>) -> Parsed<Option<f32>> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(None);
    }
    let spacing = parse_length(p)?;
    if spacing < 0.0 {
        return Err(unexpected());
    }
    Ok(Some(spacing))
}

/// One or more lengths, of which the first two are kept.
fn parse_offset(p: &mut Parser<'_>) -> Parsed<(f32, f32)> {
    let x = parse_length(p)?;
    let y = if p.is_exhausted() {
        0.0
    } else {
        parse_length(p)?
    };
    while !p.is_exhausted() {
        parse_length(p)?;
    }
    Ok((x, y))
}

/// Base URL for colour parsing; colours hold no URLs, so any URL will do.
static URL_DATA: LazyLock<Option<UrlExtraData>> = LazyLock::new(|| {
    Url::parse("about:blank")
        .ok()
        .map(|url| UrlExtraData(ServoArc::new(url)))
});

fn parse_color(style: &ComputedValues, p: &mut Parser<'_>) -> Parsed<Color> {
    let url_data = URL_DATA.as_ref().ok_or_else(unexpected)?;
    let context = ParserContext::new(
        Origin::Author,
        url_data,
        Some(CssRuleType::Style),
        ParsingMode::DEFAULT,
        QuirksMode::NoQuirks,
        Default::default(),
        None,
        None,
        Default::default(),
    );
    let specified = SpecifiedColor::parse(&context, p).map_err(|_| unexpected())?;
    let computed = specified
        .to_computed_color(None)
        .map_err(|()| unexpected())?;
    Ok(computed
        .resolve_to_absolute(&style.clone_color())
        .as_color_color())
}
