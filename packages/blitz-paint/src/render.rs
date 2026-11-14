mod background;
mod border;
mod box_shadow;
mod clip_path;
mod form_controls;
mod glass;
mod mask;

use blitz_dom::CopyStyleGetters;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::kurbo_css::{CornerCurve, CssBox};
use crate::blend::convert_blend_mode;
use crate::color::{Color, ToColorColor};
use crate::debug_overlay::render_debug_overlay;
use crate::filters::convert_filters;
use crate::kurbo_css::NonUniformRoundedRectRadii;
use crate::layers::LayerManager;
use crate::sizing::compute_object_fit;
use crate::text::{DrawTextContext, TextPaintOptions};
use crate::{CustomWidgetSceneMap, SELECTION_COLOR};
use anyrender::glass::{BackdropSource, GlassGroup, GlassShape};
use anyrender::{PaintScene, Scene};
use blitz_dom::glass::{CornerStyleProp, GlassProps};
use blitz_dom::node::{
    ListItemLayout, ListItemLayoutPosition, Marker, NodeData, RasterImageData, TextInputData,
    TextNodeData,
};
use blitz_dom::{BaseDocument, ElementData, Node, NodeId, local_name};
use blitz_traits::devtools::DevtoolSettings;

use style::values::computed::{BorderCornerRadius, ColorOrAuto};
use style::{
    dom::TElement,
    properties::{
        ComputedValues, generated::longhands::visibility::computed_value::T as StyloVisibility,
        style_structs::Font,
    },
    values::{
        computed::{CSSPixelLength, Contain, Overflow},
        specified::box_::{DisplayInside, DisplayOutside},
        specified::image::ImageRendering,
    },
};

use kurbo::{self, Affine, BezPath, Insets, Point, Rect, Shape, Size, Stroke, Vec2};
use peniko::{self, BlendMode, Fill, ImageData, ImageSampler};
use style::computed_values::isolation::T as Isolation;
use style::values::generics::color::GenericColor;
use taffy::Layout;

/// An opacity just below 1.0, used to force the renderer to allocate an isolated
/// buffer for mask and text-clip isolation layers. Renderers (e.g. vello_cpu) may optimize
/// layers with `Mix::Normal`/`Compose::SrcOver` and an opacity of exactly 1.0 into
/// plain non-isolated clip layers, in which case the `Compose::DestIn` mask
/// compositing would erase the backdrop behind the element rather than just the
/// element's own content.
pub(super) const ALMOST_OPAQUE: f32 = 1.0 - f32::EPSILON;

/// A view of the track positions reported by taffy in physical (left-to-right /
/// top-to-bottom) order. Taffy reports tracks in logical order, which for RTL grid
/// containers is physically reversed.
#[derive(Clone, Copy)]
pub(crate) struct PhysicalTracks<'a> {
    positions: &'a [taffy::Line<f32>],
    reversed: bool,
}

impl<'a> PhysicalTracks<'a> {
    pub(crate) fn from_tracks<S: taffy::CheapCloneStr>(
        tracks: &'a taffy::DetailedGridTracksInfo<S>,
    ) -> Self {
        let positions = tracks.positions.as_slice();
        let reversed = positions
            .first()
            .zip(positions.last())
            .is_some_and(|(first, last)| first.start > last.start);
        Self {
            positions,
            reversed,
        }
    }

    fn get(self, index: usize) -> taffy::Line<f32> {
        if self.reversed {
            self.positions[self.positions.len() - 1 - index]
        } else {
            self.positions[index]
        }
    }

    /// Iterate track positions in physical order
    pub(crate) fn iter(self) -> impl ExactSizeIterator<Item = taffy::Line<f32>> + 'a {
        (0..self.positions.len()).map(move |index| self.get(index))
    }

    /// The physical start position of the first track
    pub(crate) fn origin(self) -> f32 {
        if self.positions.is_empty() {
            0.0
        } else {
            self.get(0).start
        }
    }

    /// Total distance from the start of the first track to the end of the last track
    pub(crate) fn span(self) -> f32 {
        if self.positions.is_empty() {
            0.0
        } else {
            self.get(self.positions.len() - 1).end - self.get(0).start
        }
    }
}

/// A short-lived struct which holds a bunch of parameters for rendering a scene so
/// that we don't have to pass them down as parameters
pub struct BlitzDomPainter<'dom, 'a> {
    /// Input parameters (read only) for generating the Scene
    pub(crate) dom: &'dom BaseDocument,
    pub(crate) scale: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) initial_x: f64,
    pub(crate) initial_y: f64,
    /// The id of the document's root element (cached to avoid re-resolving it for every element)
    pub(crate) root_element_id: Option<NodeId>,
    /// Scrollbar hover/drag state, resolved once per scene like the root element
    #[cfg(feature = "scrollbars")]
    pub(crate) hovered_scrollbar: Option<blitz_dom::node::ScrollbarRef>,
    #[cfg(feature = "scrollbars")]
    pub(crate) scrollbar_drag_target: Option<blitz_dom::node::ScrollbarRef>,
    pub(crate) layer_manager: LayerManager,
    /// Reusable scratch allocations shared by all text layouts painted for this document.
    pub(crate) draw_text_context: RefCell<DrawTextContext>,
    /// Cached selection ranges for O(1) lookup: node_id -> (start_offset, end_offset)
    pub(crate) selection_ranges: HashMap<NodeId, (usize, usize)>,

    // Pre-computed `Scene`s for each CustomWidget
    pub(crate) custom_widget_scenes: &'a CustomWidgetSceneMap,

    /// Glass nodes whose shape a glass container's group already draws this paint.
    pub(crate) glass_collected: RefCell<HashSet<NodeId>>,
    /// Portal sources being re-painted, innermost last; a source on it paints nothing more.
    pub(crate) glass_active_sources: RefCell<SmallVec<[NodeId; 4]>>,
    /// Portal sources that `--glass-source-hidden` skips at their home position.
    pub(crate) glass_hidden_sources: RefCell<HashSet<NodeId>>,
    /// Glass nodes whose unparsable glass properties have been logged.
    #[cfg(feature = "tracing")]
    pub(crate) glass_invalid_logged: RefCell<HashSet<NodeId>>,
    /// Glass nodes whose elliptical corners have been logged as rounded down.
    #[cfg(feature = "tracing")]
    pub(crate) glass_radii_logged: RefCell<HashSet<NodeId>>,
    /// Glass nodes whose evenodd clip path has been logged as filled nonzero.
    #[cfg(feature = "tracing")]
    pub(crate) glass_fill_rule_logged: RefCell<HashSet<NodeId>>,
    /// Glass nodes whose missing portal source has been logged.
    #[cfg(feature = "tracing")]
    pub(crate) glass_source_missing_logged: RefCell<HashSet<NodeId>>,
    /// Glass nodes whose ignored source, magnify or offset has been logged.
    #[cfg(feature = "tracing")]
    pub(crate) glass_source_ignored_logged: RefCell<HashSet<NodeId>>,
}

impl<'dom, 'a> BlitzDomPainter<'dom, 'a> {
    /// Create a new BlitzDomPainter for the given document
    pub fn new(
        dom: &'dom BaseDocument,
        scale: f64,
        width: u32,
        height: u32,
        initial_x: f64,
        initial_y: f64,
        custom_widget_scenes: &'a CustomWidgetSceneMap,
    ) -> Self {
        let selection_ranges: HashMap<NodeId, (usize, usize)> = dom
            .get_text_selection_ranges()
            .into_iter()
            .map(|(node_id, start, end)| (node_id, (start, end)))
            .collect();

        let layer_manager = LayerManager::default();
        let root_element_id = dom.try_root_element().map(|el| el.id);

        Self {
            dom,
            scale,
            width,
            height,
            initial_x,
            initial_y,
            root_element_id,
            #[cfg(feature = "scrollbars")]
            hovered_scrollbar: dom.hovered_scrollbar(),
            #[cfg(feature = "scrollbars")]
            scrollbar_drag_target: dom.scrollbar_drag_target(),
            layer_manager,
            draw_text_context: RefCell::new(DrawTextContext::default()),
            selection_ranges,
            custom_widget_scenes,
            glass_collected: RefCell::default(),
            glass_active_sources: RefCell::default(),
            glass_hidden_sources: RefCell::default(),
            #[cfg(feature = "tracing")]
            glass_invalid_logged: RefCell::default(),
            #[cfg(feature = "tracing")]
            glass_radii_logged: RefCell::default(),
            #[cfg(feature = "tracing")]
            glass_fill_rule_logged: RefCell::default(),
            #[cfg(feature = "tracing")]
            glass_source_missing_logged: RefCell::default(),
            #[cfg(feature = "tracing")]
            glass_source_ignored_logged: RefCell::default(),
        }
    }

    /// The glass shape `id` of `node`, whose border box is `frame`, if it draws one.
    ///
    /// Its `clip-path`, if it is a basic shape, gives the shape, unless the morph overrides
    /// replace it.
    fn glass_shape(
        &self,
        node: &Node,
        frame: &CssBox,
        transform: Affine,
        props: &GlassProps,
        id: u32,
    ) -> Option<GlassShape> {
        #[cfg(feature = "tracing")]
        let node_id = node.id;
        #[cfg(feature = "tracing")]
        if !props.invalid.is_empty() && self.glass_invalid_logged.borrow_mut().insert(node_id) {
            tracing::warn!(
                name: "blitz.glass.unregistered",
                node = %node_id,
                properties = ?props.invalid,
                "unregistered glass properties didn't parse and read as their defaults"
            );
        }
        let clip = node
            .primary_styles()
            .and_then(|style| clip_path::glass_clip(style.get_clip_path(), frame, self.scale));
        let built = glass::glass_shape(frame, clip, props, transform, self.scale, id)?;
        #[cfg(feature = "tracing")]
        if built.rounded_elliptical && self.glass_radii_logged.borrow_mut().insert(node_id) {
            tracing::warn!(
                name: "blitz.glass.elliptical_radii",
                node = %node_id,
                "glass takes the smaller radius of each elliptical corner"
            );
        }
        #[cfg(feature = "tracing")]
        if built.evenodd && self.glass_fill_rule_logged.borrow_mut().insert(node_id) {
            tracing::warn!(
                name: "blitz.glass.fill_rule",
                node = %node_id,
                "glass fills an evenodd clip path with the nonzero rule"
            );
        }
        Some(built.shape)
    }

    /// The glass groups `node` emits.
    ///
    /// A lone glass element groups only itself. A glass container groups its own glass, if any,
    /// with every glass element in its paint subtree that no nested container or layer claims
    /// (see [`Self::collect_glass`]). The group draws where the container's own glass would,
    /// before its background. The container's clip-path and mask layers clip it, and so does
    /// its effect layer, to the border box, when that layer is pushed for opacity, a filter or
    /// a kept backdrop-filter.
    ///
    /// A container that clips its content draws its members inside that clip instead, as a
    /// second group, so its own glass, outside the clip, stays a lone group with the node id.
    fn glass_groups(
        &self,
        node: &Node,
        frame: &CssBox,
        transform: Affine,
        paints_own_box: bool,
        clips_content: bool,
    ) -> GlassGroups {
        let spacing = node.glass_container();
        let own_props = (paints_own_box && node.has_glass())
            .then(|| node.glass_props())
            .flatten();
        if spacing.is_none() && own_props.is_none() {
            return GlassGroups::default();
        }

        let mut members = Vec::new();
        if let Some(props) = &own_props
            && let Some(shape) = self.glass_shape(node, frame, transform, props, 0)
        {
            members.push((shape, props.union.clone()));
        }
        let draws_own = !members.is_empty();

        let Some(spacing) = spacing else {
            let mut own = group_of(members, node.id.as_u64(), 0.0);
            let portal = match (&mut own, &own_props) {
                (Some(group), Some(props)) => self.backdrop(node, props, group),
                _ => None,
            };
            return GlassGroups {
                own,
                inner: None,
                draws_own,
                portal,
            };
        };
        if let Some(props) = &own_props
            && draws_own
        {
            self.ignore_backdrop(node, props);
        }

        let (own, mut members) = if clips_content {
            (group_of(members, node.id.as_u64(), 0.0), Vec::new())
        } else {
            (None, members)
        };
        let children_transform = self.scrolled_transform(node, transform);
        for (child_id, child_transform) in self.paint_children(node, children_transform) {
            self.collect_glass(child_id, child_transform, &mut members);
        }
        let spacing = f64::from(spacing);
        if clips_content {
            GlassGroups {
                own,
                inner: group_of(members, node.id.as_u64() | CLIPPED_MEMBERS_ID_BIT, spacing),
                draws_own,
                portal: None,
            }
        } else {
            GlassGroups {
                own: group_of(members, node.id.as_u64(), spacing),
                inner: None,
                draws_own,
                portal: None,
            }
        }
    }

    /// Sets the backdrop of `node`'s lone `group` from its `props`, and returns the portal
    /// source to re-paint under it, if it has one.
    ///
    /// A `--glass-source` names the source by its `id`; an unknown id paints nothing, and a
    /// magnify or offset beside it is ignored. Without one, a magnify other than 1 or an offset
    /// other than 0 makes the backdrop a `Region`.
    #[cfg_attr(
        not(feature = "tracing"),
        expect(unused_variables, reason = "only the logs read `node`")
    )]
    fn backdrop(&self, node: &Node, props: &GlassProps, group: &mut GlassGroup) -> Option<NodeId> {
        if let Some(id) = &props.source {
            let source = self.dom.get_element_by_id(id);
            #[cfg(feature = "tracing")]
            if source.is_none()
                && self
                    .glass_source_missing_logged
                    .borrow_mut()
                    .insert(node.id)
            {
                tracing::warn!(
                    name: "blitz.glass.source_missing",
                    node = %node.id,
                    source = %id,
                    "no element has the glass source's id, so the portal paints nothing"
                );
            }
            #[cfg(feature = "tracing")]
            if glass::has_region(props)
                && self
                    .glass_source_ignored_logged
                    .borrow_mut()
                    .insert(node.id)
            {
                tracing::warn!(
                    name: "blitz.glass.source_ignored",
                    node = %node.id,
                    "a glass portal ignores its magnify and offset"
                );
            }
            return source;
        }
        if glass::has_region(props)
            && let Some(shape) = group.shapes.first()
            && let Some(region) = glass::region_source(shape, props.magnify, props.source_offset)
        {
            group.source = region;
        }
        None
    }

    /// Logs that `node`'s glass, drawn in a container's group, ignores its portal source,
    /// magnify and offset.
    #[cfg_attr(
        not(feature = "tracing"),
        expect(unused_variables, reason = "only the logs read `node` and `props`")
    )]
    fn ignore_backdrop(&self, node: &Node, props: &GlassProps) {
        #[cfg(feature = "tracing")]
        if (props.source.is_some() || glass::has_region(props))
            && self
                .glass_source_ignored_logged
                .borrow_mut()
                .insert(node.id)
        {
            tracing::warn!(
                name: "blitz.glass.source_ignored",
                node = %node.id,
                "glass in a container's group ignores its source, magnify and offset"
            );
        }
    }

    /// Re-paints `source` under `group`, whose element paints under `transform`, clipped to
    /// the group's shapes, unless `source` is already being re-painted.
    ///
    /// The re-paint is content only: while it runs, no glass is drawn and no portal opens.
    ///
    /// The source's own layout offset is cancelled, so its border box lands on the glass
    /// element's. `clip_rect` is the glass element's culling rectangle, in screen px.
    fn draw_portal(
        &self,
        scene: &mut impl PaintScene,
        group: &GlassGroup,
        source: NodeId,
        transform: Affine,
        clip_rect: Rect,
    ) {
        if self.glass_active_sources.borrow().contains(&source) {
            return;
        }
        let Some(node) = self.dom.as_ref().get_node(source) else {
            return;
        };
        let location = node.final_layout().location;
        let origin = Vec2::new(f64::from(location.x), f64::from(location.y)) * self.scale;
        let source_transform = transform * Affine::translate(-origin);

        let mut clip = BezPath::new();
        for shape in &group.shapes {
            if let Some(outline) = glass::shape_outline(&shape.kind) {
                let to_device = Affine::scale(self.scale) * shape.transform;
                clip.extend(&(to_device * outline));
            }
        }
        let screen = Affine::translate((-self.initial_x, -self.initial_y));
        let clip_rect = clip_rect.intersect(screen.transform_rect_bbox(clip.bounding_box()));

        let pushed = self.layer_manager.maybe_push_layer(
            scene,
            true,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &clip,
            None,
            None,
        );
        // Past the layer limit there is no clip, and an unclipped copy would cover the page.
        if !pushed {
            return;
        }
        self.glass_active_sources.borrow_mut().push(source);
        self.render_node(scene, source, source_transform, clip_rect);
        self.glass_active_sources.borrow_mut().pop();
        self.layer_manager.maybe_pop_layer(scene, pushed);
    }

    /// Whether `node`, with `styles`, is a hidden portal source or paints as part of one.
    ///
    /// A positioned or z-indexed descendant can paint outside its source's subtree, hoisted to
    /// an outer stacking context or containing block, so it is hidden with its source.
    fn hidden_at_home(&self, node: &Node, styles: &ComputedValues) -> bool {
        use style::computed_values::position::T as Position;

        let hidden = self.glass_hidden_sources.borrow();
        if hidden.is_empty() {
            return false;
        }
        if hidden.contains(&node.id) {
            return true;
        }
        let escapes = styles.get_box().position != Position::Static
            || !styles.get_position().z_index.is_auto();
        let tree = self.dom.tree();
        escapes
            && std::iter::successors(node.parent, |&id| tree[id].parent)
                .any(|id| hidden.contains(&id))
    }

    /// The portal sources that a glass element hides at their home position.
    ///
    /// It reads only glass properties and layout. Glass that never paints (`display: none`, a
    /// zero-size box or a non-invertible transform) is left out, and so is glass that is a
    /// container or has one as an ancestor: a group's glass ignores its source, so hiding it
    /// would leave the source painted nowhere.
    ///
    /// A hidden source that is an ancestor of its own portal paints nowhere, as in UIKit. A
    /// hidden source is still hit-testable, so authors add `pointer-events: none` to it.
    fn hidden_sources(&self) -> HashSet<NodeId> {
        let dom = self.dom;
        let tree = dom.tree();
        let in_container = |node: &Node| {
            std::iter::successors(Some(node), |n| n.parent.map(|id| &tree[id]))
                .any(|n| n.glass_container().is_some())
        };
        let paints = |node: &Node| {
            let size = node.final_layout().size;
            let det = node
                .transform()
                .as_deref()
                .copied()
                .unwrap_or_default()
                .determinant();
            !matches!(node.taffy_display(), taffy::Display::None)
                && size.width > 0.0
                && size.height > 0.0
                && det != 0.0
                && det.is_finite()
        };
        tree.iter()
            .filter(|(_, node)| {
                node.flags.is_in_document()
                    && node.has_glass()
                    && paints(node)
                    && !in_container(node)
            })
            .filter_map(|(_, node)| node.glass_props())
            .filter(|props| props.source_hidden)
            .filter_map(|props| dom.get_element_by_id(props.source.as_deref()?))
            .collect()
    }

    /// Adds the glass of `node_id`, painted under `parent_transform`, and of its paint subtree
    /// to a container's `members`.
    ///
    /// It skips what doesn't paint and nested containers. Glass under a layer between it and
    /// the container (opacity, filter, clip-path, mask, blend, isolation, CSS `clip`, an
    /// overflow clip or a backdrop-filter) stays out, and so does glass whose own layer would
    /// hold it, so each draws as a lone group inside its layers.
    fn collect_glass(
        &self,
        node_id: NodeId,
        parent_transform: Affine,
        members: &mut Vec<(GlassShape, Option<style::Atom>)>,
    ) {
        let node = &self.dom.as_ref().tree()[node_id];
        // The same early returns as `render_element`, which skip the node's whole subtree.
        if !matches!(
            node.data,
            NodeData::Element(_) | NodeData::AnonymousBlock(_)
        ) || matches!(node.taffy_display(), taffy::Display::None)
            || (node.local_name() == "input" && node.attr(local_name!("type")) == Some("hidden"))
        {
            return;
        }
        let Some(styles) = node.primary_styles() else {
            return;
        };
        if styles.get_inherited_box().visibility != StyloVisibility::Visible
            || styles.get_effects().opacity == 0.0
        {
            return;
        }
        // A nested container groups its own subtree when it paints, and a layer over the
        // node's own glass holds the glass of its subtree too.
        if node.glass_container().is_some() || self.layers_own_glass(node, &styles) {
            return;
        }

        let transform = self.node_transform(node, parent_transform);
        let mut collected = false;
        if !node.is_inline_with_only_block_children()
            && node.has_glass()
            && let Some(props) = node.glass_props()
        {
            let frame = self.css_box(node, &styles, node.final_layout());
            let id = members.len() as u32;
            if let Some(shape) = self.glass_shape(node, &frame, transform, &props, id) {
                self.ignore_backdrop(node, &props);
                self.glass_collected.borrow_mut().insert(node_id);
                members.push((shape, props.union));
                collected = true;
            }
        }

        // The overflow clip and a kept backdrop-filter layer hold only what the node paints
        // after its own glass would.
        let backdrop_layer = !collected && !styles.get_effects().backdrop_filter.0.is_empty();
        if self.should_clip_node(node, &styles) || backdrop_layer {
            return;
        }
        let children_transform = self.scrolled_transform(node, transform);
        for (child_id, child_transform) in self.paint_children(node, children_transform) {
            self.collect_glass(child_id, child_transform, members);
        }
    }

    /// Whether painting `node` pushes a layer around everything it paints, its own glass
    /// included: opacity, filter, clip-path, mask, blend, isolation or CSS `clip`.
    fn layers_own_glass(&self, node: &Node, styles: &ComputedValues) -> bool {
        use style::computed_values::position::T as Position;
        use style::values::generics::ClipRectOrAuto;

        let effects = styles.get_effects();
        node.applies_atomic_paint_effect()
            || styles.get_box().isolation == Isolation::Isolate
            || convert_blend_mode(effects.mix_blend_mode) != BlendMode::default()
            || (matches!(
                styles.get_box().position,
                Position::Absolute | Position::Fixed
            ) && matches!(effects.clip, ClipRectOrAuto::Rect(_)))
    }

    /// The children `node` paints, in paint order, each with the transform it paints under.
    ///
    /// `transform` is `node`'s own transform with its scroll offset applied. The order is the
    /// negative z-index hoisted children, the regular children, then the positive ones.
    fn paint_children<'n>(
        &'n self,
        node: &'n Node,
        transform: Affine,
    ) -> impl Iterator<Item = (NodeId, Affine)> + 'n {
        let hoisted = move |child: &blitz_dom::HoistedPaintChild| {
            let position = child.position(node.tree(), node.id);
            let pos = Vec2::new(position.x as f64, position.y as f64) * self.scale;
            (child.node_id, transform.pre_translate(pos))
        };
        let negative = node
            .stacking_context
            .iter()
            .flat_map(|sc| sc.neg_z_hoisted_children())
            .map(hoisted);
        let positive = node
            .stacking_context
            .iter()
            .flat_map(|sc| sc.pos_z_hoisted_children())
            .map(hoisted);

        // The list is re-borrowed per child, so painting a child never overlaps a borrow.
        let mut index = 0;
        let regular = std::iter::from_fn(move || {
            let child_id = *node.paint_children.borrow().as_ref()?.get(index)?;
            index += 1;
            Some(child_id)
        })
        .map(move |child_id| {
            // Fixed-position children of the root element are positioned against
            // the viewport and do not scroll with it, so cancel out the viewport
            // scroll (applied in `paint_scene`). A fixed box contained by a
            // transformed ancestor scrolls with it like any out-of-flow box.
            let child = &self.dom.as_ref().tree()[child_id];
            let child_transform = if child.taffy_position() == taffy::Position::Fixed
                && Some(node.id) == self.root_element_id
            {
                let scroll = self.dom.as_ref().viewport_scroll();
                transform.pre_translate(Vec2::new(scroll.x, scroll.y) * self.scale)
            } else if let Some(offset) = child.lifted_paint_offset(node.id) {
                transform.pre_translate(Vec2::new(offset.x as f64, offset.y as f64) * self.scale)
            } else {
                transform
            };
            (child_id, child_transform)
        });

        negative.chain(regular).chain(positive)
    }

    /// `node`'s boxes and radii in device px for `layout`, as its background paints them.
    fn css_box(&self, node: &Node, style: &ComputedValues, layout: &Layout) -> CssBox {
        let continuous = node.corner_style() == Some(CornerStyleProp::Continuous);
        create_css_rect(style, layout, self.scale, continuous)
    }

    /// Draws `group`, whose shapes are in CSS px.
    fn draw_glass(&self, scene: &mut impl PaintScene, group: &GlassGroup) {
        if scene.supports_glass() {
            scene.draw_glass(group);
            return;
        }
        // The tint fallback fills each shape under its own transform, in device px, with no
        // glass backend to apply the document scale.
        let mut group = group.clone();
        for shape in &mut group.shapes {
            shape.transform = Affine::scale(self.scale) * shape.transform;
        }
        if let BackdropSource::Region { transform, .. } = &mut group.source {
            *transform = Affine::scale(self.scale) * *transform;
        }
        scene.draw_glass(&group);
    }

    /// Draw the current tree to current render surface
    /// Eventually we'll want the surface itself to be passed into the render function, along with things like the viewport
    ///
    /// This assumes styles are resolved and layout is complete.
    /// Make sure you do those before trying to render
    pub fn paint_scene(&self, scene: &mut impl PaintScene) {
        if self.dom.has_pending_critical_resources() {
            return;
        }
        self.glass_collected.borrow_mut().clear();
        self.glass_active_sources.borrow_mut().clear();
        *self.glass_hidden_sources.borrow_mut() = self.hidden_sources();

        // Simply render the document (the root element (note that this is not the same as the root node)))
        // scene.reset();
        let viewport_scroll = self.dom.as_ref().viewport_scroll();

        // A document without a root element (e.g. an empty iframe sub-document) has
        // nothing to paint.
        let Some(root_element) = self.dom.as_ref().try_root_element() else {
            return;
        };
        let root_id = root_element.id;
        let bg_width = (self.width as f32).max(root_element.final_layout().size.width);
        let bg_height = (self.height as f32).max(root_element.final_layout().size.height);

        let background_color = {
            let html_styles = root_element.primary_styles();
            let html_color = html_styles
                .as_ref()
                .map_or(&GenericColor::TRANSPARENT_BLACK, |s| {
                    s.get_background_color()
                });
            if *html_color == GenericColor::TRANSPARENT_BLACK {
                root_element
                    .children
                    .iter()
                    .find_map(|id| {
                        self.dom
                            .as_ref()
                            .get_node(*id)
                            .filter(|node| node.data.is_element_with_tag_name(&local_name!("body")))
                    })
                    .and_then(|body| body.primary_styles())
                    .map(|style| {
                        let current_color = style.clone_color();
                        style
                            .get_background_color()
                            .resolve_to_absolute(&current_color)
                    })
            } else {
                let current_color = html_styles.as_ref().unwrap().clone_color();
                Some(html_color.resolve_to_absolute(&current_color))
            }
        };

        if let Some(bg_color) = background_color {
            let bg_color = bg_color.as_srgb_color();
            let rect = Rect::from_origin_size(
                (self.initial_x, self.initial_y),
                (bg_width as f64, bg_height as f64),
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, bg_color, None, &rect);
        }

        // The root clip rectangle is the viewport (in screen coordinates, with the
        // initial offset already subtracted). Elements outside of this are culled, and
        // scrollports narrow this rectangle further for their descendants.
        let viewport_clip_rect = Rect::new(0.0, 0.0, self.width as f64, self.height as f64);

        self.render_element(
            scene,
            root_id,
            Affine::translate(Vec2 {
                x: self.initial_x - (viewport_scroll.x * self.scale),
                y: self.initial_y - (viewport_scroll.y * self.scale),
            }),
            viewport_clip_rect,
        );

        // Render debug overlay
        if self.dom.devtools().highlight_hover {
            if let Some(node_id) = self.dom.as_ref().get_hover_node_id() {
                render_debug_overlay(
                    scene,
                    self.dom,
                    node_id,
                    self.scale,
                    self.initial_x,
                    self.initial_y,
                );
            }
        }
        if let Some(node_id) = self.dom.devtools().highlight_node {
            // Only nodes with element data have a layout to visualise
            if self
                .dom
                .as_ref()
                .get_node(node_id)
                .is_some_and(|node| node.element_data().is_some())
            {
                render_debug_overlay(
                    scene,
                    self.dom,
                    node_id,
                    self.scale,
                    self.initial_x,
                    self.initial_y,
                );
            }
        }
    }

    fn node_transform(&self, node: &Node, parent: Affine) -> Affine {
        let location = node.final_layout().location;
        parent
            * Affine::translate(Vec2::new(location.x as f64, location.y as f64) * self.scale)
            * node.transform().as_deref().copied().unwrap_or_default()
    }

    fn scrolled_transform(&self, node: &Node, transform: Affine) -> Affine {
        transform.then_translate(Vec2 {
            x: -node.scroll_offset().x * self.scale,
            y: -node.scroll_offset().y * self.scale,
        })
    }

    fn should_clip_node(&self, node: &Node, styles: &ComputedValues) -> bool {
        let box_style = styles.get_box();
        let contain_paint = box_style.slow_clone_contain().contains(Contain::PAINT) && {
            let display = styles.clone_display();
            let is_internal_table_box_other_than_cell = display.outside()
                == DisplayOutside::InternalTable
                && display.inside() != DisplayInside::TableCell;
            !display.is_inline_flow() && !is_internal_table_box_other_than_cell
        };
        let element = node.element_data();
        self.root_element_id != Some(node.id)
            && (element.and_then(|e| e.raster_image_data()).is_some()
                || element.and_then(|e| e.sub_doc_data()).is_some()
                || element.and_then(|e| e.text_input_data()).is_some()
                || contain_paint
                || !matches!(box_style.overflow_x, Overflow::Visible)
                || !matches!(box_style.overflow_y, Overflow::Visible))
    }

    fn draw_text_mask(
        &self,
        scene: &mut impl PaintScene,
        node_id: NodeId,
        transform: Affine,
        root: bool,
    ) {
        let node = &self.dom.as_ref().tree()[node_id];
        if matches!(node.taffy_display(), taffy::Display::None) || (!root && node.is_out_of_flow())
        {
            return;
        }
        let Some(styles) = node.primary_styles() else {
            return;
        };
        if styles.get_inherited_box().visibility != StyloVisibility::Visible
            || node.element_data().is_none()
        {
            return;
        }
        let cx = self.element_cx(node, *node.final_layout(), transform, None);
        let clip = if cx.text_input.is_some() {
            cx.frame.text_input_clip_path()
        } else {
            cx.frame.padding_box_path()
        };
        let pushed = self.layer_manager.maybe_push_layer(
            scene,
            self.should_clip_node(node, &styles),
            BlendMode::default(),
            1.0,
            transform,
            &clip,
            None,
            None,
        );
        let transform = self.scrolled_transform(node, transform);
        if node.flags.is_inline_root() {
            if let Some(layout) = &cx.element.inline_layout_data {
                let layout_box = node.final_layout();
                let x = (layout_box.padding.left + layout_box.border.left) as f64
                    - node.scroll_offset().x;
                let y = (layout_box.padding.top + layout_box.border.top) as f64
                    - node.scroll_offset().y
                    + layout.block_offset as f64;
                let text_transform =
                    transform * Affine::translate((x * self.scale, y * self.scale));
                crate::text::stroke_text(
                    scene,
                    layout.layout.lines(),
                    self.dom,
                    text_transform,
                    TextPaintOptions {
                        scale: self.scale,
                        brush_override: Some(Color::BLACK),
                    },
                    node.id,
                    &mut self.draw_text_context.borrow_mut(),
                );
            }
        }
        if let Some(children) = &*node.layout_children.borrow() {
            for &child_id in children {
                let child = &self.dom.as_ref().tree()[child_id];
                self.draw_text_mask(
                    scene,
                    child_id,
                    self.node_transform(child, transform),
                    false,
                );
            }
        }
        self.layer_manager.maybe_pop_layer(scene, pushed);
    }

    /// Renders a node, but is guaranteed that the node is an element
    /// This is because the font_size is calculated from layout resolution and all text is rendered directly here, instead
    /// of a separate text stroking phase.
    ///
    /// In Blitz, text styling gets its attributes from its container element/resolved styles
    /// In other libraries, text gets its attributes from a `text` element - this is not how HTML works.
    ///
    /// Approaching rendering this way guarantees we have all the styles we need when rendering text with not having
    /// to traverse back to the parent for its styles, or needing to pass down styles
    fn render_element(
        &self,
        scene: &mut impl PaintScene,
        node_id: NodeId,
        parent_style_transform: Affine,
        clip_rect: Rect,
    ) {
        let node = &self.dom.as_ref().tree()[node_id];

        // Early return if the element is hidden
        if matches!(node.taffy_display(), taffy::Display::None) {
            return;
        }

        // Only draw elements with a style
        let Some(styles) = node.primary_styles() else {
            return;
        };

        // A hidden portal source paints only through its portals.
        let in_portal = !self.glass_active_sources.borrow().is_empty();
        if !in_portal && self.hidden_at_home(node, &styles) {
            return;
        }

        // Hide inputs with type=hidden
        // Implemented here rather than using the style engine for performance reasons
        if node.local_name() == "input" && node.attr(local_name!("type")) == Some("hidden") {
            return;
        }

        // Hide elements with a visibility style other than visible
        if styles.get_inherited_box().visibility != StyloVisibility::Visible {
            return;
        }

        let effects = styles.get_effects();
        let opacity = effects.opacity;
        if opacity == 0.0 {
            return;
        }
        let has_opacity = opacity < 1.0;

        let should_clip = self.should_clip_node(node, &styles);
        let is_text_input = node
            .element_data()
            .and_then(|el| el.text_input_data())
            .is_some();

        // Apply padding/border offset to inline root
        let taffy::Layout {
            size,
            border,
            padding,
            ..
        } = *node.final_layout();
        let box_size = Size::new(
            size.width as f64 * self.scale,
            size.height as f64 * self.scale,
        );
        let border_box = Rect::from_origin_size(Point::ORIGIN, box_size);
        let scaled_pb = (padding + border).map(f64::from);
        let content_position = kurbo::Point {
            x: scaled_pb.left,
            y: scaled_pb.top,
        };
        let content_box_size = kurbo::Size {
            width: (size.width as f64 - scaled_pb.left - scaled_pb.right) * self.scale,
            height: (size.height as f64 - scaled_pb.top - scaled_pb.bottom) * self.scale,
        };

        // Don't render things that are out of view
        let overflow = *node.scrollable_overflow();
        let transform = self.node_transform(node, parent_style_transform);
        let screen_transform = Affine::translate(Vec2 {
            x: -self.initial_x,
            y: -self.initial_y,
        }) * transform;
        let screen_bbox = screen_transform.transform_rect_bbox(overflow.union(border_box));

        // Cull elements that fall entirely outside the current clip rectangle. In addition to
        // the viewport, `clip_rect` is narrowed by any ancestor scrollport (see below), so this
        // also culls elements scrolled out of view inside a clipping/scrolling container.
        if screen_bbox.x1 < clip_rect.x0
            || screen_bbox.x0 > clip_rect.x1
            || screen_bbox.y1 < clip_rect.y0
            || screen_bbox.y0 > clip_rect.y1
        {
            return;
        }

        // Cull elements whose transformed geometry is not representable in f32 (e.g.
        // `transform: scale(99e99)`): the renderer works in f32 and flattening such paths
        // can attempt to allocate an unbounded number of line segments.
        let f32_max = f64::from(f32::MAX);
        if !screen_bbox.is_finite()
            || [
                screen_bbox.x0,
                screen_bbox.y0,
                screen_bbox.x1,
                screen_bbox.y1,
            ]
            .iter()
            .any(|c| c.abs() > f32_max)
        {
            return;
        }

        // Optimise zero-area (/very small area) clips by not rendering at all
        let clip_area = content_box_size.width * content_box_size.height;
        let overflow_area =
            node.scrollable_overflow().width() * node.scrollable_overflow().height();
        if should_clip && clip_area < 0.01 && overflow_area < 0.01 {
            return;
        }

        #[cfg(feature = "custom-widget")]
        let custom_widget_scene = self.custom_widget_scenes.get(&(self.dom.id(), node_id));
        #[cfg(not(feature = "custom-widget"))]
        let custom_widget_scene = None;

        // Apply CSS transform property (where transforms are 2d)

        let mut cx = self.element_cx(node, *node.final_layout(), transform, custom_widget_scene);

        // If this element clips its overflow it establishes a scrollport: narrow the clip
        // rectangle passed to descendants to the visible (clipped) region so that content
        // scrolled out of view is culled rather than drawn and clipped away. The box used
        // here matches the clip applied to the content below.
        let child_clip_rect = if should_clip {
            let clip_box = if is_text_input {
                cx.frame.text_input_clip_path()
            } else {
                cx.frame.padding_box_path()
            };
            clip_rect.intersect(screen_transform.transform_rect_bbox(clip_box.bounding_box()))
        } else {
            clip_rect
        };

        // Compute clip-path (if any) and wrap all rendering in a clip layer
        let clip_path_shape = cx.clip_path_shape();
        let has_clip_path = clip_path_shape.is_some();
        let default_clip = cx.frame.border_box_path();
        let mut clip_path_for_layer = clip_path_shape.unwrap_or(default_clip);
        clip_path_for_layer.apply_affine(Affine::scale(self.scale));

        // CSS 2 `clip` property (absolutely positioned elements only). Applies to the whole of
        // the element's rendering (including outlines and shadows), so it is the outermost layer.
        let css_clip_rect = cx.css_clip_rect();
        if css_clip_rect.is_some_and(|rect| rect.is_zero_area()) {
            return;
        }
        let css_clip_layer_pushed = self.layer_manager.maybe_push_layer(
            scene,
            css_clip_rect.is_some(),
            BlendMode::default(),
            1.0,
            cx.transform,
            &css_clip_rect.unwrap_or(Rect::ZERO),
            None,
            None,
        );

        // Use the viewport in scene coordinates so outlines, shadows and overflowing
        // descendants are not clipped to the element's border box.
        let blend = convert_blend_mode(effects.mix_blend_mode);
        let isolate = styles.get_box().isolation == Isolation::Isolate;
        let blend_layer_pushed = self.layer_manager.maybe_push_layer(
            scene,
            isolate || blend != BlendMode::default(),
            blend,
            if isolate { 1.0 - f32::EPSILON } else { 1.0 },
            Affine::IDENTITY,
            &Rect::new(
                self.initial_x,
                self.initial_y,
                self.initial_x + self.width as f64,
                self.initial_y + self.height as f64,
            ),
            None,
            None,
        );

        let paints_own_box = !node.is_inline_with_only_block_children();
        // Glass, when drawn, replaces the element's own backdrop-filter, whether this element
        // draws it or a container's group already does.
        // A portal's copy is content only: no glass, and so no nested portals.
        let glass = if self.glass_collected.borrow().contains(&node_id) {
            GlassGroups {
                draws_own: true,
                ..GlassGroups::default()
            }
        } else if in_portal {
            GlassGroups {
                draws_own: paints_own_box && node.has_glass(),
                ..GlassGroups::default()
            }
        } else {
            self.glass_groups(node, &cx.frame, cx.transform, paints_own_box, should_clip)
        };

        cx.draw_outline(scene);
        if paints_own_box {
            cx.draw_outset_box_shadow(scene);
        }

        // clip-path clip ayer
        self.layer_manager.maybe_with_layer(
            scene,
            has_clip_path,
            BlendMode::default(),
            1.0,
            cx.transform,
            &clip_path_for_layer,
            None,
            None,
            |scene| {
                // If the element has a CSS `mask`, then push an isolation layer for the
                // masked content. The mask is applied when the layer is popped below.
                let mask_layer_pushed = cx.maybe_push_css_mask_layer(scene);
                // `cx.transform` is mutated to apply scroll offsets while drawing content.
                // Save it so that the mask can be drawn untransformed by scroll offsets.
                let unscrolled_transform = cx.transform;

                let filter = convert_filters(&effects.filter.0).map(Arc::new);
                let backdrop_filter = match glass.draws_own {
                    true => None,
                    false => convert_filters(&effects.backdrop_filter.0).map(Arc::new),
                };

                // Adjust effect layer clip by filter expansion area
                //
                // Returns a rectangle centered at the origin representing how much the filter
                // expands the processing region in each direction. The rect coordinates are:
                // - x0: negative left expansion
                // - y0: negative top expansion
                // - x1: positive right expansion
                // - y1: positive bottom expansion
                let filter_expansion_area = filter
                    .as_ref()
                    .map(|f| f.expansion_rect())
                    .unwrap_or(Rect::ZERO);

                let mut effect_layer_clip = cx.frame.border_box_path().bounding_box();
                effect_layer_clip.x0 += filter_expansion_area.x0;
                effect_layer_clip.y0 += filter_expansion_area.y0;
                effect_layer_clip.x1 += filter_expansion_area.x1;
                effect_layer_clip.y1 += filter_expansion_area.y1;

                // Opacity/Filter layer if box has opacity or a filter.
                // Clipped to border-box as it needs to include the background and borders.
                self.layer_manager.maybe_with_layer(
                    scene,
                    has_opacity || filter.is_some() || backdrop_filter.is_some(),
                    BlendMode::default(),
                    opacity,
                    cx.transform,
                    &effect_layer_clip,
                    filter,
                    backdrop_filter,
                    |scene| {
                        if let Some(group) = &glass.own {
                            if let Some(source) = glass.portal {
                                self.draw_portal(scene, group, source, cx.transform, clip_rect);
                            }
                            self.draw_glass(scene, group);
                        }
                        if paints_own_box {
                            cx.draw_background(scene);
                            cx.draw_inset_box_shadow(scene);
                        }
                        cx.draw_table_row_backgrounds(scene);
                        cx.draw_table_borders(scene);
                        if paints_own_box {
                            cx.draw_border(scene);
                        }
                        cx.stroke_devtools(scene);

                        // TODO: allow layers with opacity to be unclipped (overflow: visible)
                        let clip = if is_text_input {
                            &cx.frame.text_input_clip_path()
                        } else {
                            &cx.frame.padding_box_path()
                        };

                        // Clip layer if box requires clipping. Opacity set to 1.0
                        self.layer_manager.maybe_with_layer(
                            scene,
                            should_clip,
                            BlendMode::default(),
                            1.0, // opacity
                            cx.transform,
                            clip,
                            None,
                            None,
                            |scene| {
                                // Now that background has been drawn, offset pos and cx in order to draw our contents scrolled
                                let content_position = Point {
                                    x: content_position.x - node.scroll_offset().x,
                                    y: content_position.y - node.scroll_offset().y,
                                };

                                cx.transform = self.scrolled_transform(node, cx.transform);
                                // Before the content, as the group of a container that
                                // doesn't clip draws before its background.
                                if let Some(group) = &glass.inner {
                                    self.draw_glass(scene, group);
                                }
                                cx.draw_image(scene);
                                #[cfg(feature = "svg")]
                                cx.draw_svg(scene);
                                #[cfg(feature = "custom-widget")]
                                cx.draw_custom_widget(scene);
                                cx.draw_sub_document(scene);
                                cx.draw_input(scene);
                                cx.draw_text_input_text(scene, content_position);
                                cx.draw_inline_layout(scene, content_position);
                                cx.draw_marker(scene, content_position);
                                cx.draw_children(scene, cx.transform, child_clip_rect);
                            },
                        );

                        // Overlay scrollbars, drawn unscrolled above the
                        // clipped content.
                        #[cfg(feature = "scrollbars")]
                        {
                            cx.transform = unscrolled_transform;
                            cx.draw_scrollbars(scene);
                        }
                    },
                );

                // Apply the CSS `mask` (if any) to the content drawn above
                cx.transform = unscrolled_transform;
                cx.maybe_pop_css_mask_layer(scene, mask_layer_pushed);
            },
        );

        self.layer_manager
            .maybe_pop_layer(scene, blend_layer_pushed);
        self.layer_manager
            .maybe_pop_layer(scene, css_clip_layer_pushed);
    }

    fn render_node(
        &self,
        scene: &mut impl PaintScene,
        node_id: NodeId,
        parent_style_transform: Affine,
        clip_rect: Rect,
    ) {
        let node = &self.dom.as_ref().tree()[node_id];

        match &node.data {
            NodeData::Element(_) | NodeData::AnonymousBlock(_) => {
                self.render_element(scene, node_id, parent_style_transform, clip_rect)
            }
            NodeData::Text(TextNodeData { .. }) => {
                // Text nodes should never be rendered directly
                // (they should always be rendered as part of an inline layout)
                // unreachable!()
            }
            NodeData::Document(_) => {}
            // NodeData::Doctype => {}
            NodeData::Comment { .. } => {} // NodeData::ProcessingInstruction { .. } => {}
        }
    }

    fn element_cx(
        &'dom self,
        node: &'dom Node,
        layout: Layout,
        transform: Affine,
        custom_widget_scene: Option<&'a Scene>,
    ) -> ElementCx<'dom, 'a> {
        let style = node
            .stylo_element_data()
            .primary_styles()
            .as_ref()
            .map(|styles| (*styles).clone())
            .unwrap_or_else(|| {
                ComputedValues::initial_values_with_font_override(Font::initial_values()).to_arc()
            });

        let scale = self.scale;

        // todo: maybe cache this so we don't need to constantly be figuring it out
        // It is quite a bit of math to calculate during render/traverse
        // Also! we can cache the bezpaths themselves, saving us a bunch of work
        let frame = self.css_box(node, &style, &layout);

        let element = node.element_data().unwrap();

        ElementCx {
            context: self,
            frame,
            scale,
            style,
            node,
            element,
            transform,
            #[cfg(feature = "svg")]
            svg: match element.image_data() {
                Some(blitz_dom::node::ImageData::Svg(svg)) => Some(svg),
                _ => None,
            },
            text_input: element.text_input_data(),
            list_item: element.list_item_data.as_deref(),
            devtools: self.dom.devtools(),
            custom_widget_scene,
        }
    }
}

/// Set on a clipping container's node id to give its members' group an id of its own, apart
/// from the container's own glass. This is the top bit of the id's 32-bit slot version, which
/// grows by two each time the slot is reused, so a collision needs one slot reused 2^30 times.
const CLIPPED_MEMBERS_ID_BIT: u64 = 1 << 63;

/// The glass groups an element draws.
#[derive(Default)]
struct GlassGroups {
    /// The group drawn just before the element's background.
    own: Option<GlassGroup>,
    /// A clipping container's members, drawn inside its clip before its content.
    inner: Option<GlassGroup>,
    /// Whether the element's own glass is drawn, by itself or by its container.
    draws_own: bool,
    /// The portal source re-painted under `own`, for a lone glass element.
    portal: Option<NodeId>,
}

/// The group of `members`, in order, with every pair that shares a union; `None` when empty.
fn group_of(
    members: Vec<(GlassShape, Option<style::Atom>)>,
    id: u64,
    merge_spacing: f64,
) -> Option<GlassGroup> {
    if members.is_empty() {
        return None;
    }
    let mut unions = Vec::new();
    for (i, (a, union)) in members.iter().enumerate() {
        let Some(union) = union else { continue };
        for (b, other) in &members[i + 1..] {
            if other.as_ref() == Some(union) {
                unions.push((a.id, b.id));
            }
        }
    }
    let mut group =
        GlassGroup::new(members.into_iter().map(|(shape, _)| shape).collect()).with_id(id);
    group.merge_spacing = merge_spacing;
    group.unions = unions;
    Some(group)
}

fn to_image_quality(image_rendering: ImageRendering) -> peniko::ImageQuality {
    match image_rendering {
        ImageRendering::Auto => peniko::ImageQuality::Medium,
        ImageRendering::CrispEdges => peniko::ImageQuality::Low,
        ImageRendering::Pixelated => peniko::ImageQuality::Low,
    }
}

/// The transform from an SVG's rendered tree to a viewport of the given size.
#[cfg(feature = "svg")]
fn svg_viewport_affine(svg: &blitz_dom::node::SvgImageData, width: f32, height: f32) -> Affine {
    let t = svg.viewport_transform(width, height);
    Affine::new([t.sx, t.ky, t.kx, t.sy, t.tx, t.ty].map(f64::from))
}

/// Ensure that the `resized_image` field has a correctly sized image
fn to_peniko_image(image: &RasterImageData, quality: peniko::ImageQuality) -> peniko::ImageBrush {
    peniko::ImageBrush {
        image: ImageData {
            data: image.data.clone(),
            format: peniko::ImageFormat::Rgba8,
            width: image.width,
            height: image.height,
            alpha_type: peniko::ImageAlphaType::Alpha,
        },
        sampler: ImageSampler {
            x_extend: peniko::Extend::Repeat,
            y_extend: peniko::Extend::Repeat,
            quality,
            alpha: 1.0,
        },
    }
}

/// A context of loaded and hot data to draw the element from
struct ElementCx<'dom, 'a> {
    context: &'dom BlitzDomPainter<'dom, 'a>,
    frame: CssBox,
    style: style::servo_arc::Arc<ComputedValues>,
    scale: f64,
    node: &'dom Node,
    element: &'dom ElementData,
    transform: Affine,
    #[cfg(feature = "svg")]
    svg: Option<&'dom blitz_dom::node::SvgImageData>,
    text_input: Option<&'dom TextInputData>,
    list_item: Option<&'dom ListItemLayout>,
    devtools: &'dom DevtoolSettings,
    #[cfg_attr(not(feature = "custom-widget"), expect(unused))]
    custom_widget_scene: Option<&'a Scene>,
}

/// Converts parley BoundingBox into peniko Rect
fn convert_rect(rect: &parley::BoundingBox) -> kurbo::Rect {
    peniko::kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

impl ElementCx<'_, '_> {
    /// Paint overlay scrollbar thumbs for scroll containers: `overflow:
    /// scroll`, or `auto` when the content overflows (never `hidden`/`clip`,
    /// which scroll only programmatically). Thumbs appear on scroll and fade
    /// out after a delay ([`BaseDocument::scrollbar_opacity`]); never-scrolled
    /// containers paint nothing, keeping thumbs out of static reftest
    /// screenshots.
    ///
    /// Geometry comes from [`Node::scrollbar_thumb`], shared with the
    /// thumb-drag hit testing in blitz-dom.
    #[cfg(feature = "scrollbars")]
    fn draw_scrollbars(&self, scene: &mut impl PaintScene) {
        // css-scrollbars-1 scrollbar-color: author thumb/track colors
        use blitz_dom::node::{ScrollbarColor, ScrollbarRef};
        use taffy::AbsoluteAxis;
        let (custom_thumb, custom_track) = match self.node.scrollbar_color() {
            ScrollbarColor::Auto => (None, None),
            ScrollbarColor::Colors { thumb, track } => {
                (Some(thumb.as_srgb_color()), Some(track.as_srgb_color()))
            }
        };

        let drag_target = self.context.scrollbar_drag_target;
        let hovered_thumb = self.context.hovered_scrollbar;

        // scrollbar-color doesn't affect overlay visibility: persistence is
        // UA policy, not author styling.
        let node_id = self.node.id;
        let opacity = self.context.dom.scrollbar_opacity(node_id);
        if opacity == 0.0 {
            return;
        }

        // Default thumb palette for the used color scheme; thumbs paint as
        // fill plus a thin contrast stroke so they read over same-colored
        // content.
        let dark_scheme =
            self.context.dom.viewport().color_scheme == blitz_traits::shell::ColorScheme::Dark;
        let (thumb_rest, thumb_hover, thumb_active, stroke_color) = if dark_scheme {
            (
                Color::from_rgba8(214, 214, 214, 178),
                Color::from_rgba8(190, 190, 190, 222),
                Color::from_rgba8(172, 172, 172, 255),
                Color::from_rgba8(0, 0, 0, 102),
            )
        } else {
            (
                Color::from_rgba8(128, 128, 128, 178),
                Color::from_rgba8(152, 152, 152, 222),
                Color::from_rgba8(170, 170, 170, 255),
                Color::from_rgba8(255, 255, 255, 102),
            )
        };

        // Chromium's hovered/pressed scrollbar contrast ratios.
        const HOVER_CONTRAST: f32 = 1.8;
        const ACTIVE_CONTRAST: f32 = 1.3;

        for axis in [AbsoluteAxis::Vertical, AbsoluteAxis::Horizontal] {
            if !self.node.wants_scrollbar(axis) {
                continue;
            }
            let Some(thumb) = self.node.scrollbar_thumb(axis) else {
                continue;
            };

            let rect = thumb.scale_from_origin(self.scale);

            // Track (only when the author specified a track color)
            if let Some(track_color) = custom_track {
                let padding_box = self.frame.padding_box;
                let track_rect = match axis {
                    AbsoluteAxis::Horizontal => {
                        Rect::new(padding_box.x0, rect.y0, padding_box.x1, rect.y1)
                    }
                    AbsoluteAxis::Vertical => {
                        Rect::new(rect.x0, padding_box.y0, rect.x1, padding_box.y1)
                    }
                };
                scene.fill(
                    Fill::NonZero,
                    self.transform,
                    track_color.multiply_alpha(opacity),
                    None,
                    &track_rect,
                );
            }

            let this = ScrollbarRef { node_id, axis };
            let is_active = drag_target == Some(this);
            let is_hovered = hovered_thumb == Some(this);
            let color = match custom_thumb {
                Some(base) if is_active => crate::color::blend_for_contrast(base, ACTIVE_CONTRAST),
                Some(base) if is_hovered => crate::color::blend_for_contrast(base, HOVER_CONTRAST),
                Some(base) => base,
                None if is_active => thumb_active,
                None if is_hovered => thumb_hover,
                None => thumb_rest,
            };
            let radius = match axis {
                AbsoluteAxis::Horizontal => rect.height() / 2.0,
                AbsoluteAxis::Vertical => rect.width() / 2.0,
            };
            scene.fill(
                Fill::NonZero,
                self.transform,
                color.multiply_alpha(opacity),
                None,
                &rect.to_rounded_rect(radius),
            );
            // Contrast stroke, default thumbs only: an author-specified
            // scrollbar-color is rendered exactly as given.
            if custom_thumb.is_none() {
                let stroke_width = self.scale;
                let stroke_rect = rect.inset(-stroke_width / 2.0);
                scene.stroke(
                    &Stroke::new(stroke_width),
                    self.transform,
                    stroke_color.multiply_alpha(opacity),
                    None,
                    &stroke_rect.to_rounded_rect(radius - stroke_width / 2.0),
                );
            }
        }
    }

    fn draw_inline_layout(&self, scene: &mut impl PaintScene, pos: Point) {
        if self.node.flags.is_inline_root() {
            let text_layout = self.element
                .inline_layout_data
                .as_ref()
                .unwrap_or_else(|| {
                    panic!("Tried to render node marked as inline root that does not have an inline layout: {:?}", self.node);
                });

            // Text keeps its unrounded position; only box edges are snapped to whole pixels.
            let subpixel_offset = text_layout.content_box_subpixel_offset;
            let pos = Point {
                x: pos.x + subpixel_offset.x as f64,
                y: pos.y + subpixel_offset.y as f64 + text_layout.block_offset as f64,
            };
            let transform =
                self.transform * Affine::translate((pos.x * self.scale, pos.y * self.scale));

            // Render inline element backgrounds (e.g. `<span style="background: ...">`)
            // behind the text and selection highlight.
            crate::text::draw_inline_backgrounds(
                scene,
                text_layout.layout.lines(),
                self.context.dom,
                transform,
                self.node.id,
            );

            // Render text selection highlight (if any) using cached selection ranges
            if let Some(&(sel_start, sel_end)) = self.context.selection_ranges.get(&self.node.id) {
                crate::text::draw_text_selection(
                    scene,
                    &text_layout.layout,
                    transform,
                    sel_start,
                    sel_end,
                );
            }

            // Render text
            let mut draw_text_context = self.context.draw_text_context.borrow_mut();
            crate::text::stroke_text(
                scene,
                text_layout.layout.lines(),
                self.context.dom,
                transform,
                TextPaintOptions {
                    scale: self.scale,
                    brush_override: None,
                },
                self.node.id,
                &mut draw_text_context,
            );
        }
    }

    fn draw_text_input_text(&self, scene: &mut impl PaintScene, pos: Point) {
        // Render the text in text inputs
        if let Some(input_data) = self.text_input {
            // For single-line inputs, add an offset to vertically center the text input layout
            // within the content box of it's node.
            let y_offset = self.node.text_input_v_centering_offset(self.scale);
            let subpixel_offset = input_data.content_box_subpixel_offset;
            let pos = Point {
                x: pos.x + subpixel_offset.x as f64,
                y: pos.y + subpixel_offset.y as f64 + y_offset,
            };

            // Apply the scroll offset (stored in CSS pixels, scaled here to device pixels) so
            // that the caret stays visible. Single-line inputs scroll horizontally; multi-line
            // inputs scroll vertically.
            let scroll_offset = input_data.scroll_offset as f64 * self.scale;
            let (scroll_x, scroll_y) = if input_data.is_multiline {
                (0.0, scroll_offset)
            } else {
                (scroll_offset, 0.0)
            };

            let transform = self.transform
                * Affine::translate((pos.x * self.scale - scroll_x, pos.y * self.scale - scroll_y));

            if self.node.is_focussed() {
                // Render selection/caret
                for (rect, _line_idx) in input_data.editor.selection_geometry().iter() {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        SELECTION_COLOR,
                        None,
                        &convert_rect(rect),
                    );
                }
                if let Some(cursor) = input_data.editor.cursor_geometry(1.5) {
                    let color = self.style.get_inherited_text().color;
                    let caret_color = match &self.style.get_inherited_ui().caret_color.0 {
                        ColorOrAuto::Auto => color,
                        ColorOrAuto::Color(caret_color) => caret_color.resolve_to_absolute(&color),
                    };

                    scene.fill(
                        Fill::NonZero,
                        transform,
                        caret_color.as_srgb_color(),
                        None,
                        &convert_rect(&cursor),
                    );
                };
            }

            // Render text
            let mut draw_text_context = self.context.draw_text_context.borrow_mut();
            crate::text::stroke_text(
                scene,
                input_data.editor.try_layout().unwrap().lines(),
                self.context.dom,
                transform,
                TextPaintOptions {
                    scale: self.scale,
                    brush_override: None,
                },
                self.node.id,
                &mut draw_text_context,
            );
        }
    }

    fn draw_marker(&self, scene: &mut impl PaintScene, pos: Point) {
        if let Some(ListItemLayout {
            marker,
            position: ListItemLayoutPosition::Outside(layout),
            content_box_subpixel_offset,
        }) = self.list_item
        {
            // Right align and pad the bullet when rendering outside
            let x_padding = match marker {
                Marker::Char(_) => 8.0,
                Marker::String(_) => 0.0,
            };
            // Outside markers are placed outside the list item's border box
            // (`pos` is the origin of its content box)
            let item_layout = self.node.final_layout();
            let x_offset = -(layout.full_width() / layout.scale()
                + x_padding
                + item_layout.padding.left
                + item_layout.border.left);

            // Align the marker with the baseline of the first line of text in the list item
            let y_offset = if let Some((text_layout, first_text_line)) = &self
                .element
                .inline_layout_data
                .as_ref()
                .and_then(|text_layout| Some((text_layout, text_layout.layout.lines().next()?)))
            {
                (first_text_line.metrics().baseline
                    - layout.lines().next().unwrap().metrics().baseline)
                    / layout.scale()
                    + text_layout.block_offset
            } else {
                0.0
            };

            let pos = Point {
                x: pos.x + (content_box_subpixel_offset.x + x_offset) as f64,
                y: pos.y + (content_box_subpixel_offset.y + y_offset) as f64,
            };

            let transform =
                self.transform * Affine::translate((pos.x * self.scale, pos.y * self.scale));

            let mut draw_text_context = self.context.draw_text_context.borrow_mut();
            crate::text::stroke_text(
                scene,
                layout.lines(),
                self.context.dom,
                transform,
                TextPaintOptions {
                    scale: self.scale,
                    brush_override: None,
                },
                self.node.id,
                &mut draw_text_context,
            );
        }
    }

    fn draw_children(
        &self,
        scene: &mut impl PaintScene,
        parent_style_transform: Affine,
        clip_rect: Rect,
    ) {
        for (child_id, child_transform) in self
            .context
            .paint_children(self.node, parent_style_transform)
        {
            self.render_node(scene, child_id, child_transform, clip_rect);
        }
    }

    #[cfg(feature = "svg")]
    fn draw_svg(&self, scene: &mut impl PaintScene) {
        use style::properties::generated::longhands::object_fit::computed_value::T as ObjectFit;

        let Some(svg) = self.svg else {
            return;
        };

        let width = self.frame.content_box.width() as f32;
        let height = self.frame.content_box.height() as f32;
        let svg_size = svg.tree.size();

        let x = self.frame.content_box.origin().x;
        let y = self.frame.content_box.origin().y;

        // An SVG with an aspect ratio but no natural size is sized by the default sizing
        // algorithm, which for `none` and `scale-down` contains it within the content box.
        let object_fit = match self.style.clone_object_fit() {
            ObjectFit::None | ObjectFit::ScaleDown
                if svg.intrinsic_width().is_none()
                    && svg.intrinsic_height().is_none()
                    && svg.viewbox_aspect_ratio().is_some() =>
            {
                ObjectFit::Contain
            }
            object_fit => object_fit,
        };
        let object_position = self.style.get_object_position();

        // Apply object-fit algorithm
        let container_size = taffy::Size { width, height };
        let object_size = taffy::Size {
            width: svg_size.width(),
            height: svg_size.height(),
        };
        let paint_size = compute_object_fit(container_size, Some(object_size), object_fit);

        // Compute object-position
        let x_offset = object_position.horizontal.resolve(
            CSSPixelLength::new(container_size.width - paint_size.width) / self.scale as f32,
        ) * self.scale as f32;
        let y_offset = object_position.vertical.resolve(
            CSSPixelLength::new(container_size.height - paint_size.height) / self.scale as f32,
        ) * self.scale as f32;
        let x = x + x_offset.px() as f64;
        let y = y + y_offset.px() as f64;

        let viewport = svg_viewport_affine(svg, paint_size.width, paint_size.height);
        let transform = (self.transform * viewport).then_translate(Vec2 { x, y });

        self.with_replaced_content_clip(
            scene,
            container_size,
            paint_size,
            x_offset.px(),
            y_offset.px(),
            |scene| {
                anyrender_svg::render_svg_tree(scene, &svg.tree, transform);
            },
        );
    }

    /// Clip replaced content to the content box when its object-fit/object-position
    /// rect overflows it.
    fn with_replaced_content_clip<S: PaintScene>(
        &self,
        scene: &mut S,
        container_size: taffy::Size<f32>,
        paint_size: taffy::Size<f32>,
        x_offset: f32,
        y_offset: f32,
        paint: impl FnOnce(&mut S),
    ) {
        let overflows = x_offset < 0.0
            || y_offset < 0.0
            || x_offset + paint_size.width > container_size.width
            || y_offset + paint_size.height > container_size.height;
        self.context.layer_manager.maybe_with_layer(
            scene,
            overflows,
            BlendMode::default(),
            1.0,
            self.transform,
            &self.frame.content_box_path(),
            None,
            None,
            paint,
        );
    }

    fn draw_image(&self, scene: &mut impl PaintScene) {
        if let Some(image) = self.element.raster_image_data() {
            let width = self.frame.content_box.width() as u32;
            let height = self.frame.content_box.height() as u32;
            let x = self.frame.content_box.origin().x;
            let y = self.frame.content_box.origin().y;

            let object_fit = self.style.clone_object_fit();
            let object_position = self.style.get_object_position();
            let image_rendering = self.style.clone_image_rendering();
            let quality = to_image_quality(image_rendering);

            // Apply object-fit algorithm
            let container_size = taffy::Size {
                width: width as f32,
                height: height as f32,
            };
            let object_size = taffy::Size {
                width: image.width as f32,
                height: image.height as f32,
            };
            let paint_size = compute_object_fit(container_size, Some(object_size), object_fit);

            // Compute object-position
            let x_offset = object_position.horizontal.resolve(
                CSSPixelLength::new(container_size.width - paint_size.width) / self.scale as f32,
            ) * self.scale as f32;
            let y_offset = object_position.vertical.resolve(
                CSSPixelLength::new(container_size.height - paint_size.height) / self.scale as f32,
            ) * self.scale as f32;
            let x = x + x_offset.px() as f64;
            let y = y + y_offset.px() as f64;

            let x_scale = paint_size.width as f64 / object_size.width as f64;
            let y_scale = paint_size.height as f64 / object_size.height as f64;
            let transform = self
                .transform
                .pre_translate(Vec2 { x, y })
                .pre_scale_non_uniform(x_scale, y_scale);

            self.with_replaced_content_clip(
                scene,
                container_size,
                paint_size,
                x_offset.px(),
                y_offset.px(),
                |scene| {
                    scene.draw_image(to_peniko_image(image, quality).as_ref(), transform);
                },
            );
        }
    }

    #[cfg(feature = "custom-widget")]
    fn draw_custom_widget(&self, scene: &mut impl PaintScene) {
        if let Some(widget_scene) = self.custom_widget_scene {
            let x = self.frame.content_box.origin().x;
            let y = self.frame.content_box.origin().y;
            let transform = self.transform.then_translate(Vec2 { x, y });

            // TODO: eliminate clone
            scene.append_scene(widget_scene.clone(), transform);
        }
    }

    fn draw_sub_document(&self, scene: &mut impl PaintScene) {
        if let Some(sub_doc) = self.element.sub_doc_data().map(|doc| doc.inner()) {
            let scale = self.scale;
            let width = self.frame.content_box.width() as u32;
            let height = self.frame.content_box.height() as u32;

            // TODO: Support arbitrary transforms of subdocuments
            let translation = self.transform.translation();
            let initial_x = translation.x + self.frame.content_box.origin().x;
            let initial_y = translation.y + self.frame.content_box.origin().y;
            // let transform = self.transform.then_translate(Vec2 { x, y });

            let painter = BlitzDomPainter::new(
                &sub_doc,
                scale,
                width,
                height,
                initial_x,
                initial_y,
                self.custom_widget_scenes,
            );
            painter.paint_scene(scene);
        }
    }

    fn stroke_devtools(&self, scene: &mut impl PaintScene) {
        if self.devtools.show_layout {
            let shape = &self.frame.border_box;
            let stroke = Stroke::new(self.scale);

            let stroke_color = match self.node.taffy_display() {
                taffy::Display::Block | taffy::Display::FlowRoot => {
                    Color::new([1.0, 0.0, 0.0, 1.0])
                }
                taffy::Display::Flex => Color::new([0.0, 1.0, 0.0, 1.0]),
                taffy::Display::Grid => Color::new([0.0, 0.0, 1.0, 1.0]),
                taffy::Display::None => Color::new([0.0, 0.0, 1.0, 1.0]),
            };

            scene.stroke(&stroke, self.transform, stroke_color, None, &shape);
        }
    }
}
impl<'dom, 'a> std::ops::Deref for ElementCx<'dom, 'a> {
    type Target = BlitzDomPainter<'dom, 'a>;
    fn deref(&self) -> &Self::Target {
        self.context
    }
}

fn insets_from_taffy_rect(input: taffy::Rect<f64>) -> Insets {
    Insets {
        x0: input.left,
        y0: input.top,
        x1: input.right,
        y1: input.bottom,
    }
}

/// Convert Stylo and Taffy types into Kurbo types
///
/// `continuous` is `--corner-style: continuous`, which overrides `corner-shape`.
fn create_css_rect(
    style: &ComputedValues,
    layout: &Layout,
    scale: f64,
    continuous: bool,
) -> CssBox {
    // Resolve and rescale
    // We have to scale since document pixels are not same same as rendered pixels
    let width: f64 = layout.size.width as f64;
    let height: f64 = layout.size.height as f64;
    let border_box = Rect::new(0.0, 0.0, width * scale, height * scale);
    let border = insets_from_taffy_rect(layout.border.map(|p| p as f64 * scale));
    let padding = insets_from_taffy_rect(layout.padding.map(|p| p as f64 * scale));
    let outline_width = style.get_outline().outline_width.0.to_f64_px() * scale;

    // Resolve the radii to a length. need to downscale since the radii are in document pixels
    let resolve_w = CSSPixelLength::new(width as _);
    let resolve_h = CSSPixelLength::new(height as _);
    let resolve_radii = |radius: &BorderCornerRadius| -> Vec2 {
        Vec2 {
            x: scale * radius.0.width.0.resolve(resolve_w).px() as f64,
            y: scale * radius.0.height.0.resolve(resolve_h).px() as f64,
        }
    };
    let s_border = style.get_border();
    let border_radii = NonUniformRoundedRectRadii {
        top_left: resolve_radii(&s_border.border_top_left_radius),
        top_right: resolve_radii(&s_border.border_top_right_radius),
        bottom_right: resolve_radii(&s_border.border_bottom_right_radius),
        bottom_left: resolve_radii(&s_border.border_bottom_left_radius),
    };

    let corner_curves = [
        &s_border.corner_top_left_shape,
        &s_border.corner_top_right_shape,
        &s_border.corner_bottom_right_shape,
        &s_border.corner_bottom_left_shape,
    ]
    .map(|shape| match continuous {
        true => CornerCurve::Continuous,
        false => CornerCurve::from_superellipse(shape.k),
    });

    CssBox::new(border_box, border, padding, outline_width, border_radii)
        .with_corner_curves(corner_curves)
}
