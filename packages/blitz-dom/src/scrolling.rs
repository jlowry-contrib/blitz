//! Scrolling: user-initiated (interactive) and programmatic scrolling of nodes and the
//! viewport, and the scroll animations (smooth scrolls and flings) which drive them.

use blitz_traits::events::{BlitzScrollEvent, DomEvent, DomEventData};
use blitz_traits::node_id::NodeId;
use style::values::computed::Overflow;
use stylo_taffy::CopyStyleGetters;

use blitz_traits::shell::ScrollPhysics;

use crate::BaseDocument;
use crate::util::Point;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollBehavior {
    #[default]
    Auto,
    Instant,
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollLogicalPosition {
    Start,
    Center,
    End,
    Nearest,
}

/// What a scroll applies to.
///
/// Per the CSS overflow propagation rules the root element has no scrolling mechanism of its
/// own (its overflow is applied to the viewport), so [`ScrollTarget::Node`] holding the root
/// element is equivalent to [`ScrollTarget::Viewport`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollTarget {
    Node(NodeId),
    Viewport,
}

/// How far to scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScrollAmount {
    /// An absolute scroll offset.
    To(Point<f64>),
    /// A delta to apply to the current scroll offset.
    By(Point<f64>),
}

/// What to do with scroll which the target cannot consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollOverflow {
    /// Discard it: the scroll affects exactly one scroller.
    Clamp,
    /// Transfer it to the next scroller in the scroll chain (the parent node, and eventually
    /// the viewport).
    Chain,
}

/// Who initiated a scroll. Determines which overflow values count as scrollable:
/// `overflow: hidden` boxes have a scrolling box which programmatic scrolls apply to,
/// but no user scrolling mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollSource {
    /// A user-initiated scroll (wheel, touch, scrollbar drag, keyboard).
    User,
    /// A script-initiated scroll (`scrollTo` and friends, fragment navigation).
    Programmatic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollRequest {
    pub(crate) target: ScrollTarget,
    pub(crate) amount: ScrollAmount,
    pub(crate) overflow: ScrollOverflow,
    pub(crate) source: ScrollSource,
    pub(crate) behavior: ScrollBehavior,
    /// Whether to abort a smooth scroll in progress. Set for user-initiated scrolls, so that
    /// an animation does not fight the user's input for the rest of its duration.
    pub(crate) interrupt_animation: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FlingState {
    pub(crate) target: NodeId,
    pub(crate) last_seen_time: f64,
    pub(crate) x_velocity: f64,
    pub(crate) y_velocity: f64,
}

/// State driving a smooth (animated) scroll towards a target offset. Used for
/// fragment navigation (`#anchor` links) and programmatic smooth scrolling.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScrollToState {
    /// What is being scrolled.
    pub(crate) target: ScrollTarget,
    /// The scroll offset at the start of the animation.
    pub(crate) start: Point<f64>,
    /// The scroll offset to animate towards.
    pub(crate) end: Point<f64>,
    /// Frame time, in milliseconds, of the first frame that showed the animation;
    /// `None` until that frame resolves.
    pub(crate) start_time: Option<f64>,
    /// Total duration of the animation in milliseconds.
    pub(crate) duration: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScrollAnimationState {
    None,
    Fling(FlingState),
    /// A smooth scroll of the viewport towards a target offset.
    ScrollTo(ScrollToState),
    /// A touch release under [`ScrollPhysics::Ios`]: a fling, a bounce-back, or both in turn.
    Ios(IosScrollState),
}

/// The fraction of its velocity an iOS fling keeps each millisecond: UIScrollView's
/// `DecelerationRate.normal` (design, blitz-dom).
const IOS_DECELERATION_RATE: f64 = 0.998;

/// The rubber band's coefficient: the value commonly cited for UIScrollView's rubber band,
/// `d · (1 − 1/(x·c/d + 1))` (design, blitz-dom).
const IOS_RUBBER_BAND: f64 = 0.55;

/// The response, in seconds, of the critically damped spring that returns an overscrolled
/// offset to its end. This plan's choice, to be calibrated against the A08 and A09 captures
/// in Plan 5.
const IOS_BOUNCE_RESPONSE: f64 = 0.4;

/// The speed, in px/s, below which a fling or a bounce-back stops (design, blitz-dom).
const IOS_STOP_SPEED: f64 = 0.5;

/// The distance, in px, from its end within which a slowed bounce-back snaps to the end:
/// well under a device pixel at 3×.
const IOS_SETTLE_DISTANCE: f64 = 0.01;

/// The largest displayed overshoot a pan can catch, as a fraction of the scrollport's
/// extent: the rubber band only approaches the full extent, so its inverse needs a margin.
const IOS_MAX_CAUGHT_OVERSHOOT: f64 = 0.99;

/// The displayed distance past an end for an overshoot `x` in a scrollport of `extent`.
fn rubber_band(x: f64, extent: f64) -> f64 {
    extent * (1.0 - 1.0 / (x * IOS_RUBBER_BAND / extent + 1.0))
}

/// The overshoot that displays as `y` past an end: the inverse of [`rubber_band`].
fn rubber_band_inverse(y: f64, extent: f64) -> f64 {
    let y = y.min(extent * IOS_MAX_CAUGHT_OVERSHOOT);
    extent / IOS_RUBBER_BAND * (y / (extent - y))
}

/// How fast the displayed offset moves per unit of overshoot, at an overshoot `x`.
fn rubber_band_slope(x: f64, extent: f64) -> f64 {
    let denominator = x * IOS_RUBBER_BAND / extent + 1.0;
    IOS_RUBBER_BAND / (denominator * denominator)
}

/// The displayed offset on one axis for a pan's unclamped offset `raw`.
fn ios_displayed_offset(raw: f64, min: f64, max: f64, extent: f64) -> f64 {
    if max <= min || extent <= 0.0 {
        raw.clamp(min, max)
    } else if raw < min {
        min - rubber_band(min - raw, extent)
    } else if raw > max {
        max + rubber_band(raw - max, extent)
    } else {
        raw
    }
}

/// The pan offset that displays as `offset` on one axis: the inverse of
/// [`ios_displayed_offset`].
fn ios_raw_offset(offset: f64, min: f64, max: f64, extent: f64) -> f64 {
    if max <= min || extent <= 0.0 {
        offset.clamp(min, max)
    } else if offset < min {
        min - rubber_band_inverse(min - offset, extent)
    } else if offset > max {
        max + rubber_band_inverse(offset - max, extent)
    } else {
        offset
    }
}

/// The motion of one axis of an iOS scroll after the finger lifts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum IosAxisMotion {
    /// The axis doesn't move.
    Rest,
    /// A fling whose velocity decays as `v₀ · 0.998^(1000·t)`.
    Decelerate {
        /// Frame time, in seconds, of the release.
        start: f64,
        /// The offset at release.
        from: f64,
        /// The velocity at release, in px/s.
        velocity: f64,
    },
    /// A critically damped spring back to the end of the scroll range.
    Bounce {
        /// Frame time, in seconds, at which the spring starts.
        start: f64,
        /// The end the spring returns to.
        end: f64,
        /// The displacement from `end` at `start`.
        from: f64,
        /// The velocity at `start`, in px/s.
        velocity: f64,
    },
}

impl IosAxisMotion {
    /// The motion of an axis released at `offset` with `velocity` px/s, at time `start`.
    fn release(start: f64, offset: f64, velocity: f64, min: f64, max: f64) -> Self {
        if offset < min || offset > max {
            let end = offset.clamp(min, max);
            Self::Bounce {
                start,
                end,
                from: offset - end,
                velocity,
            }
        } else if velocity.abs() < IOS_STOP_SPEED || max <= min {
            Self::Rest
        } else {
            Self::Decelerate {
                start,
                from: offset,
                velocity,
            }
        }
    }

    /// The offset at time `t`, or `None` while at rest, with the motion that continues from it.
    fn sample(self, t: f64, min: f64, max: f64) -> (Option<f64>, Self) {
        match self {
            Self::Rest => (None, Self::Rest),
            Self::Decelerate {
                start,
                from,
                velocity,
            } => {
                let log_rate = 1000.0 * IOS_DECELERATION_RATE.ln();
                let distance = |tau: f64| velocity * ((log_rate * tau).exp() - 1.0) / log_rate;
                let elapsed = (t - start).max(0.0);
                let stop = (IOS_STOP_SPEED / velocity.abs()).ln() / log_rate;

                // When the fling reaches the end it heads towards, if it does before stopping.
                let end = if velocity > 0.0 { max } else { min };
                let reach = 1.0 + (end - from) * log_rate / velocity;
                let crossing = (reach > 0.0)
                    .then(|| reach.ln() / log_rate)
                    .filter(|&tau| tau <= stop);

                match crossing {
                    Some(tau) if tau <= elapsed => {
                        let bounce = Self::Bounce {
                            start: start + tau,
                            end,
                            from: 0.0,
                            velocity: velocity * (log_rate * tau).exp(),
                        };
                        bounce.sample(t, min, max)
                    }
                    _ if elapsed >= stop => (Some(from + distance(stop)), Self::Rest),
                    _ => (Some(from + distance(elapsed)), self),
                }
            }
            Self::Bounce {
                start,
                end,
                from,
                velocity,
            } => {
                let omega = std::f64::consts::TAU / IOS_BOUNCE_RESPONSE;
                let elapsed = (t - start).max(0.0);
                let b = velocity + omega * from;
                let decay = (-omega * elapsed).exp();
                let displacement = (from + b * elapsed) * decay;
                let speed = (velocity - omega * b * elapsed) * decay;
                if displacement.abs() < IOS_SETTLE_DISTANCE && speed.abs() < IOS_STOP_SPEED {
                    (Some(end), Self::Rest)
                } else {
                    (Some(end + displacement), self)
                }
            }
        }
    }
}

/// The scroll animation that follows a touch release under [`ScrollPhysics::Ios`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IosScrollState {
    /// The scroller being moved.
    pub(crate) target: ScrollTarget,
    /// The horizontal motion.
    pub(crate) x: IosAxisMotion,
    /// The vertical motion.
    pub(crate) y: IosAxisMotion,
}

/// The scroller a touch pan moves under [`ScrollPhysics::Ios`], with its unclamped offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IosPan {
    /// The scroller the pan moves; the pan doesn't chain to its ancestors.
    pub(crate) target: ScrollTarget,
    /// The offset the finger has dragged to, before the rubber band.
    pub(crate) raw: Point<f64>,
}

/// Cubic ease-in-out easing function, mapping a normalised time `t` in `[0, 1]`
/// to an eased progress value in `[0, 1]`. Used to give smooth scrolls a natural
/// acceleration/deceleration curve.
fn ease_in_out_cubic(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = 2.0 * t - 2.0;
        1.0 + (f * f * f) / 2.0
    }
}

impl BaseDocument {
    /// Apply a scroll to the document, returning whether anything moved.
    ///
    /// This is the single scrolling primitive: user-initiated scrolls
    /// ([`BaseDocument::scroll_chain_by`]) and programmatic scrolls
    /// ([`BaseDocument::scroll_to`]) differ only in the [`ScrollRequest`] they build.
    pub(crate) fn scroll(
        &mut self,
        request: ScrollRequest,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        if request.interrupt_animation
            && matches!(self.scroll_animation, ScrollAnimationState::ScrollTo(_))
        {
            self.scroll_animation = ScrollAnimationState::None;
        }
        // Any other scroll ends an iOS fling or bounce-back, which would otherwise overwrite
        // it on the next frame, and returns its scroller to its range.
        if let ScrollAnimationState::Ios(state) = self.scroll_animation {
            self.scroll_animation = ScrollAnimationState::None;
            self.clamp_scroll_offset(state.target);
        }

        let target = self.canonical_scroll_target(request.target);

        // Text inputs and sub-documents scroll their own content rather than an overflow
        // scrollport, so they only take part in the chained (user-initiated) path.
        if let (ScrollTarget::Node(node_id), ScrollAmount::By(delta), ScrollOverflow::Chain) =
            (target, request.amount, request.overflow)
        {
            if let Some(has_changed) =
                self.scroll_inner_content_by(node_id, delta, request, dispatch_event)
            {
                return has_changed;
            }
        }

        let include_hidden = request.source == ScrollSource::Programmatic;
        let (current, min, max) = self.scroll_state(target, include_hidden);
        let unclamped = match request.amount {
            ScrollAmount::To(to) => to,
            ScrollAmount::By(by) => Point {
                x: current.x + by.x,
                y: current.y + by.y,
            },
        };
        let end = Point {
            x: unclamped.x.clamp(min.x, max.x),
            y: unclamped.y.clamp(min.y, max.y),
        };

        if self.should_scroll_smoothly(target, request.behavior) {
            self.start_scroll_animation(target, end);
            return end != current;
        }

        let has_changed = self.write_scroll_offset(target, end, dispatch_event);

        // Transfer the delta the target could not consume to the next scroller in the chain.
        if request.overflow == ScrollOverflow::Chain {
            let remainder = Point {
                x: unclamped.x - end.x,
                y: unclamped.y - end.y,
            };
            if remainder != Point::ZERO {
                if let Some(next) = self.next_scroller_in_chain(target) {
                    let request = ScrollRequest {
                        target: next,
                        amount: ScrollAmount::By(remainder),
                        ..request
                    };
                    return has_changed | self.scroll(request, dispatch_event);
                }
            }
        }

        has_changed
    }

    /// Scroll a node which scrolls content of its own rather than an overflow scrollport
    /// (a text input or a sub-document), returning `None` if the node is not such a node.
    fn scroll_inner_content_by(
        &mut self,
        node_id: NodeId,
        delta: Point<f64>,
        request: ScrollRequest,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> Option<bool> {
        let node = self.nodes.get_mut(node_id)?;

        if let Some(mut sub_doc) = node.subdoc_mut().map(|doc| doc.inner_mut()) {
            let target = sub_doc
                .get_hover_node_id()
                .map_or(ScrollTarget::Viewport, ScrollTarget::Node);
            // TODO: propagate the remaining scroll to the outer document
            let request = ScrollRequest {
                target,
                amount: ScrollAmount::By(delta),
                ..request
            };
            return Some(sub_doc.scroll(request, dispatch_event));
        }

        // Single-line inputs scroll their text horizontally, multi-line inputs vertically.
        node.element_data()?.text_input_data()?;
        let content_box_width = node.final_layout().content_box_width();
        let content_box_height = node.final_layout().content_box_height();
        let input = node
            .element_data_mut()
            .and_then(|el| el.text_input_data_mut())
            .unwrap();

        // `TextInputData::scroll_by` takes (and returns the unconsumed part of) a delta in
        // the opposite direction to a scroll offset.
        let mut remainder = delta;
        if input.is_multiline {
            remainder.y =
                -(input.scroll_by(-delta.y as f32, content_box_width, content_box_height) as f64);
        } else {
            remainder.x =
                -(input.scroll_by(-delta.x as f32, content_box_width, content_box_height) as f64);
        }

        let has_changed = remainder != delta;

        if remainder != Point::ZERO {
            if let Some(next) = self.next_scroller_in_chain(ScrollTarget::Node(node_id)) {
                let request = ScrollRequest {
                    target: next,
                    amount: ScrollAmount::By(remainder),
                    ..request
                };
                return Some(has_changed | self.scroll(request, dispatch_event));
            }
        }

        Some(has_changed)
    }

    /// The scroller which unconsumed scroll is transferred to: the node's parent, or the
    /// viewport once the chain runs out of nodes.
    fn next_scroller_in_chain(&self, target: ScrollTarget) -> Option<ScrollTarget> {
        match target {
            ScrollTarget::Viewport => None,
            ScrollTarget::Node(node_id) => Some(
                self.nodes
                    .get(node_id)
                    .and_then(|node| node.parent)
                    .map_or(ScrollTarget::Viewport, ScrollTarget::Node),
            ),
        }
    }

    /// Resolve a scroll target to the scroller which actually moves: the root element scrolls
    /// the viewport, per the CSS overflow propagation rules.
    fn canonical_scroll_target(&self, target: ScrollTarget) -> ScrollTarget {
        match target {
            ScrollTarget::Node(node_id)
                if self.try_root_element().is_some_and(|el| el.id == node_id) =>
            {
                ScrollTarget::Viewport
            }
            target => target,
        }
    }

    /// Write a scroll target's offset as given, dispatching a `scroll` event and returning
    /// whether the offset changed.
    ///
    /// Callers clamp the offset to the scrollable range, except while the iOS physics
    /// overscroll it.
    pub(crate) fn write_scroll_offset(
        &mut self,
        target: ScrollTarget,
        offset: Point<f64>,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        match self.canonical_scroll_target(target) {
            ScrollTarget::Viewport => {
                let initial = self.viewport_scroll();
                self.set_viewport_scroll(offset);
                if offset == initial {
                    return false;
                }

                if let Some(root) = self.try_root_element() {
                    let root_id = root.id;
                    let layout = *root.final_layout();
                    let scale = self.viewport.scale() as f64;
                    let event = BlitzScrollEvent {
                        scroll_top: offset.y,
                        scroll_left: offset.x,
                        scroll_width: layout.size.width.max(root.scroll_width()) as i32,
                        scroll_height: layout.size.height.max(root.scroll_height()) as i32,
                        client_width: (self.viewport.window_size.0 as f64 / scale) as i32,
                        client_height: (self.viewport.window_size.1 as f64 / scale) as i32,
                    };
                    dispatch_event(DomEvent::new(root_id, DomEventData::Scroll(event)));
                }

                self.shell_provider.request_redraw();
                true
            }
            ScrollTarget::Node(node_id) => {
                let Some(node) = self.nodes.get_mut(node_id) else {
                    return false;
                };

                let initial = *node.scroll_offset();
                *node.scroll_offset_mut() = offset;
                if offset == initial {
                    return false;
                }

                let scroll_width = node.scroll_width();
                let scroll_height = node.scroll_height();
                let layout = *node.final_layout();
                self.nodes.bump_geometry_generation();
                let event = BlitzScrollEvent {
                    scroll_top: offset.y,
                    scroll_left: offset.x,
                    scroll_width: scroll_width as i32,
                    scroll_height: scroll_height as i32,
                    client_width: layout.size.width as i32,
                    client_height: layout.size.height as i32,
                };
                dispatch_event(DomEvent::new(node_id, DomEventData::Scroll(event)));

                self.show_scrollbars(node_id);
                self.shell_provider.request_redraw();
                true
            }
        }
    }

    pub fn scroll_node_by<F: FnMut(DomEvent)>(
        &mut self,
        node_id: NodeId,
        x: f64,
        y: f64,
        dispatch_event: F,
    ) {
        self.scroll_node_by_has_changed(node_id, x, y, dispatch_event);
    }

    /// Scroll a node by given x and y
    /// Will bubble scrolling up to parent node once it can no longer scroll further
    /// If we're already at the root node, bubbles scrolling up to the viewport
    pub fn scroll_node_by_has_changed<F: FnMut(DomEvent)>(
        &mut self,
        node_id: NodeId,
        x: f64,
        y: f64,
        mut dispatch_event: F,
    ) -> bool {
        self.scroll(
            ScrollRequest {
                target: ScrollTarget::Node(node_id),
                // A user-facing scroll delta moves the content, i.e. the opposite direction
                // to the scroll offset.
                amount: ScrollAmount::By(Point { x: -x, y: -y }),
                overflow: ScrollOverflow::Chain,
                source: ScrollSource::User,
                behavior: ScrollBehavior::Instant,
                interrupt_animation: false,
            },
            &mut dispatch_event,
        )
    }

    pub fn scroll_viewport_by(&mut self, x: f64, y: f64) {
        self.scroll_viewport_by_has_changed(x, y);
    }

    /// Scroll the viewport by the given values
    pub fn scroll_viewport_by_has_changed(&mut self, x: f64, y: f64) -> bool {
        let mut events = Vec::new();
        let changed = self.scroll(
            ScrollRequest {
                target: ScrollTarget::Viewport,
                amount: ScrollAmount::By(Point { x: -x, y: -y }),
                overflow: ScrollOverflow::Clamp,
                source: ScrollSource::User,
                behavior: ScrollBehavior::Instant,
                interrupt_animation: false,
            },
            &mut |event| events.push(event),
        );
        self.queue_scroll_events(events);
        changed
    }

    pub(crate) fn scroll_chain_by(
        &mut self,
        anchor_node_id: Option<NodeId>,
        scroll_x: f64,
        scroll_y: f64,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        self.scroll(
            ScrollRequest {
                target: anchor_node_id.map_or(ScrollTarget::Viewport, ScrollTarget::Node),
                amount: ScrollAmount::By(Point {
                    x: -scroll_x,
                    y: -scroll_y,
                }),
                overflow: ScrollOverflow::Chain,
                source: ScrollSource::User,
                behavior: ScrollBehavior::Instant,
                // A user-initiated scroll aborts any smooth scroll in progress, so that the
                // two do not fight over the scroll offset for the rest of the animation.
                interrupt_animation: true,
            },
            dispatch_event,
        )
    }

    /// Duration (in milliseconds) of an animated scroll.
    const SMOOTH_SCROLL_DURATION_MS: f64 = 300.0;

    /// Returns the current, minimum and maximum scroll offsets for the given
    /// scroll target. `include_hidden` controls whether
    /// `overflow: hidden` axes count as scrollable (they do for programmatic scrolls,
    /// which apply to any scrolling box, but not for user scrolls).
    pub(crate) fn scroll_state(
        &self,
        target: ScrollTarget,
        include_hidden: bool,
    ) -> (Point<f64>, Point<f64>, Point<f64>) {
        match self.canonical_scroll_target(target) {
            ScrollTarget::Viewport => {
                // The viewport scrolls the root element's scrollable overflow, which includes
                // both the root element itself and any content which overflows it (e.g. when
                // the root element has a fixed height but its content is taller). A document
                // without a root element has no scrollable content.
                let (content_width, content_height) = match self.try_root_element() {
                    Some(root) => {
                        let layout = root.final_layout();
                        (
                            layout.size.width.max(root.scroll_width()) as f64,
                            layout.size.height.max(root.scroll_height()) as f64,
                        )
                    }
                    None => (0.0, 0.0),
                };
                let scale = self.viewport.scale() as f64;
                let window_width = self.viewport.window_size.0 as f64 / scale;
                let window_height = self.viewport.window_size.1 as f64 / scale;
                let max = Point {
                    x: (content_width - window_width).max(0.0),
                    y: (content_height - window_height).max(0.0),
                };
                (self.viewport_scroll(), Point::ZERO, max)
            }
            ScrollTarget::Node(node_id) => {
                let Some(node) = self.nodes.get(node_id) else {
                    return (Point::ZERO, Point::ZERO, Point::ZERO);
                };

                // An axis with a non-scrolling overflow value has no scrollable range, even
                // when its content overflows.
                let scrollable = |overflow| match overflow {
                    Overflow::Scroll | Overflow::Auto => true,
                    Overflow::Hidden => include_hidden,
                    _ => false,
                };
                let (can_x_scroll, can_y_scroll) = node
                    .primary_styles()
                    .map(|styles| {
                        (
                            scrollable(styles.clone_overflow_x()),
                            scrollable(styles.clone_overflow_y()),
                        )
                    })
                    .unwrap_or((false, false));
                let (min, max) = node.scroll_range();
                let min = Point {
                    x: if can_x_scroll { min.x } else { 0.0 },
                    y: if can_y_scroll { min.y } else { 0.0 },
                };
                let max = Point {
                    x: match can_x_scroll {
                        true => max.x,
                        false => 0.0,
                    },
                    y: match can_y_scroll {
                        true => max.y,
                        false => 0.0,
                    },
                };
                (*node.scroll_offset(), min, max)
            }
        }
    }

    /// Start a smooth (animated) scroll towards the given absolute scroll offset. The
    /// animation is advanced each frame in [`BaseDocument::resolve_scroll_animation`].
    fn start_scroll_animation(&mut self, target: ScrollTarget, end: Point<f64>) {
        let start = self.scroll_state(target, true).0;

        // The frame time may be stale when no frame has run for a while, so the
        // animation starts from the next frame that resolves it.
        self.scroll_animation = ScrollAnimationState::ScrollTo(ScrollToState {
            target,
            start,
            end,
            start_time: None,
            duration: Self::SMOOTH_SCROLL_DURATION_MS,
        });

        // Ensure the frame loop runs so the animation is driven to completion.
        self.shell_provider.request_redraw();
    }

    fn should_scroll_smoothly(&self, target: ScrollTarget, behavior: ScrollBehavior) -> bool {
        match behavior {
            ScrollBehavior::Auto => {
                let styled_node = match target {
                    ScrollTarget::Node(node_id) => self.nodes.get(node_id),
                    ScrollTarget::Viewport => self.try_root_element(),
                };
                styled_node.is_some_and(|node| {
                    node.primary_styles().is_some_and(|style| {
                        style.clone_scroll_behavior()
                            == style::computed_values::scroll_behavior::T::Smooth
                    })
                })
            }
            ScrollBehavior::Instant => false,
            ScrollBehavior::Smooth => true,
        }
    }

    /// Scroll an element to the given absolute scroll offset in CSS pixels.
    ///
    /// Unlike a user-initiated scroll, a programmatic scroll targets exactly one scroller:
    /// scroll the element cannot consume is discarded rather than transferred to an ancestor.
    pub fn scroll_to(&mut self, node_id: NodeId, x: f64, y: f64, behavior: ScrollBehavior) {
        self.scroll_programmatically(node_id, ScrollAmount::To(Point { x, y }), behavior);
    }

    /// Scroll an element by the given relative offset in CSS pixels.
    pub fn scroll_by(&mut self, node_id: NodeId, x: f64, y: f64, behavior: ScrollBehavior) {
        self.scroll_programmatically(node_id, ScrollAmount::By(Point { x, y }), behavior);
    }

    fn scroll_programmatically(
        &mut self,
        node_id: NodeId,
        amount: ScrollAmount,
        behavior: ScrollBehavior,
    ) {
        if self.nodes.get(node_id).is_none() {
            return;
        }

        let mut events = Vec::new();
        self.scroll(
            ScrollRequest {
                target: ScrollTarget::Node(node_id),
                amount,
                overflow: ScrollOverflow::Clamp,
                source: ScrollSource::Programmatic,
                behavior,
                interrupt_animation: true,
            },
            &mut |event| events.push(event),
        );
        self.queue_scroll_events(events);
    }

    fn aligned_scroll_offset(
        current: f64,
        viewport_size: f64,
        target_start: f64,
        target_size: f64,
        position: ScrollLogicalPosition,
    ) -> f64 {
        let target_end = target_start + target_size;
        match position {
            ScrollLogicalPosition::Start => target_start,
            ScrollLogicalPosition::Center => target_start - (viewport_size - target_size) / 2.0,
            ScrollLogicalPosition::End => target_end - viewport_size,
            ScrollLogicalPosition::Nearest => {
                let viewport_end = current + viewport_size;
                if (target_start >= current && target_end <= viewport_end)
                    || (target_start <= current && target_end >= viewport_end)
                {
                    current
                } else {
                    let start_offset = target_start;
                    let end_offset = target_end - viewport_size;
                    if (start_offset - current).abs() < (end_offset - current).abs() {
                        start_offset
                    } else {
                        end_offset
                    }
                }
            }
        }
    }

    /// Scroll the viewport so that the given element has the requested alignment in each axis.
    pub fn scroll_into_view(
        &mut self,
        node_id: NodeId,
        behavior: ScrollBehavior,
        vertical: ScrollLogicalPosition,
        horizontal: ScrollLogicalPosition,
    ) {
        let Some(node) = self.nodes.get(node_id) else {
            return;
        };
        let target =
            node.absolute_position(node.scroll_offset().x as f32, node.scroll_offset().y as f32);
        let target_size = node.final_layout().size;
        let Some(root_id) = self.try_root_element().map(|root| root.id) else {
            return;
        };
        let scale = self.viewport.scale() as f64;
        let viewport_width = self.viewport.window_size.0 as f64 / scale;
        let viewport_height = self.viewport.window_size.1 as f64 / scale;
        let x = Self::aligned_scroll_offset(
            self.viewport_scroll().x,
            viewport_width,
            target.x as f64,
            target_size.width as f64,
            horizontal,
        );
        let y = Self::aligned_scroll_offset(
            self.viewport_scroll().y,
            viewport_height,
            target.y as f64,
            target_size.height as f64,
            vertical,
        );
        self.scroll_to(root_id, x, y, behavior);
    }

    /// Resolve a URL fragment (the `#...` part of a URL) to a scroll target.
    ///
    /// Returns `None` if the fragment matches no element and is not a top-of-document
    /// fragment. Otherwise returns `Some(target)`, where `target` is `Some(node_id)` for
    /// the element to scroll to, or `None` to scroll to the top of the document (matching
    /// browser behaviour for empty and `top` fragments).
    fn resolve_fragment_scroll_target(&self, fragment: &str) -> Option<Option<NodeId>> {
        // Fragments are percent-encoded in URLs (e.g. `%20`); decode before matching.
        let decoded = percent_encoding::percent_decode_str(fragment)
            .decode_utf8_lossy()
            .into_owned();

        if !decoded.is_empty() {
            if let Some(node_id) = self.get_fragment_target(&decoded) {
                return Some(Some(node_id));
            }
        }

        // An empty fragment, or the special "top" fragment when no matching element exists,
        // scrolls to the top of the document.
        if decoded.is_empty() || decoded.eq_ignore_ascii_case("top") {
            return Some(None);
        }

        None
    }

    fn scroll_to_fragment_with_behavior(
        &mut self,
        fragment: &str,
        behavior: ScrollBehavior,
    ) -> bool {
        match self.resolve_fragment_scroll_target(fragment) {
            Some(Some(node_id)) => {
                self.scroll_into_view(
                    node_id,
                    behavior,
                    ScrollLogicalPosition::Start,
                    ScrollLogicalPosition::Nearest,
                );
                true
            }
            Some(None) => {
                let root_id = self.root_element().id;
                self.scroll_to(root_id, 0.0, 0.0, behavior);
                true
            }
            None => false,
        }
    }

    /// Scroll to the element targeted by the given URL fragment (the `#...` part of a URL).
    ///
    /// An empty fragment (or a `top` fragment that matches no element) scrolls to the top
    /// of the document, matching browser behaviour. Returns `true` if a scroll target was
    /// found.
    pub fn scroll_to_fragment(&mut self, fragment: &str) -> bool {
        self.scroll_to_fragment_with_behavior(fragment, ScrollBehavior::Auto)
    }

    /// Like [`BaseDocument::scroll_to_fragment`], but animates the viewport towards the
    /// target instead of jumping instantly. Returns `true` if a scroll target was found.
    pub fn scroll_to_fragment_smooth(&mut self, fragment: &str) -> bool {
        self.scroll_to_fragment_with_behavior(fragment, ScrollBehavior::Smooth)
    }

    pub fn resolve_scroll_animation(&mut self) {
        match &mut self.scroll_animation {
            ScrollAnimationState::Fling(fling_state) => {
                let time_ms = self.frame_time.t * 1000.0;

                // An event stamped after the frame time mustn't run the fling backwards.
                let time_diff_ms = (time_ms - fling_state.last_seen_time).max(0.0);

                // 0.95 @ 60fps normalized to actual frame times
                let deceleration = 1.0 - ((0.05 / 16.66666) * time_diff_ms);

                fling_state.x_velocity *= deceleration;
                fling_state.y_velocity *= deceleration;
                fling_state.last_seen_time += time_diff_ms;
                let fling_state = fling_state.clone();

                let dx = fling_state.x_velocity * time_diff_ms;
                let dy = fling_state.y_velocity * time_diff_ms;

                let mut events = Vec::new();
                self.scroll_chain_by(Some(fling_state.target), dx, dy, &mut |event| {
                    events.push(event)
                });
                self.queue_scroll_events(events);
                if fling_state.x_velocity.abs() < 0.1 && fling_state.y_velocity.abs() < 0.1 {
                    self.scroll_animation = ScrollAnimationState::None;
                }
            }
            ScrollAnimationState::ScrollTo(scroll_to) => {
                let time_ms = self.frame_time.t * 1000.0;
                let start_time = *scroll_to.start_time.get_or_insert(time_ms);
                let scroll_to = scroll_to.clone();

                // Normalised progress through the animation, clamped to [0, 1].
                let progress = if scroll_to.duration <= 0.0 {
                    1.0
                } else {
                    ((time_ms - start_time) / scroll_to.duration).clamp(0.0, 1.0)
                };
                let eased = ease_in_out_cubic(progress);

                // Interpolate the target offset and move to it.
                let target = Point {
                    x: scroll_to.start.x + (scroll_to.end.x - scroll_to.start.x) * eased,
                    y: scroll_to.start.y + (scroll_to.end.y - scroll_to.start.y) * eased,
                };
                let mut events = Vec::new();
                self.write_scroll_offset(scroll_to.target, target, &mut |event| events.push(event));
                self.queue_scroll_events(events);

                if progress >= 1.0 {
                    self.scroll_animation = ScrollAnimationState::None;
                }
            }
            ScrollAnimationState::Ios(state) => {
                let state = *state;
                let (current, min, max) = self.scroll_state(state.target, false);
                let (x, motion_x) = state.x.sample(self.frame_time.t, min.x, max.x);
                let (y, motion_y) = state.y.sample(self.frame_time.t, min.y, max.y);
                let offset = Point {
                    x: x.unwrap_or(current.x),
                    y: y.unwrap_or(current.y),
                };

                let mut events = Vec::new();
                self.write_scroll_offset(state.target, offset, &mut |event| events.push(event));
                self.queue_scroll_events(events);

                self.scroll_animation =
                    if motion_x == IosAxisMotion::Rest && motion_y == IosAxisMotion::Rest {
                        ScrollAnimationState::None
                    } else {
                        ScrollAnimationState::Ios(IosScrollState {
                            target: state.target,
                            x: motion_x,
                            y: motion_y,
                        })
                    };
            }
            ScrollAnimationState::None => {
                // Do nothing
            }
        }
    }

    /// The extent of a scroller's scrollport on each axis, which scales its rubber band.
    fn scrollport_extent(&self, target: ScrollTarget) -> Point<f64> {
        match self.canonical_scroll_target(target) {
            ScrollTarget::Viewport => {
                let scale = self.viewport.scale() as f64;
                Point {
                    x: self.viewport.window_size.0 as f64 / scale,
                    y: self.viewport.window_size.1 as f64 / scale,
                }
            }
            ScrollTarget::Node(node_id) => {
                self.nodes.get(node_id).map_or(Point::ZERO, |node| Point {
                    x: node.client_width() as f64,
                    y: node.client_height() as f64,
                })
            }
        }
    }

    /// The scroller an iOS pan starting on `node_id` moves: the nearest one in the scroll
    /// chain with a scroll range on an allowed axis.
    ///
    /// Text inputs and sub-documents with no range of their own on an allowed axis are passed
    /// over. Returns `None` under the default physics, and when the pan reaches a text input or
    /// sub-document that can scroll its own content: those keep Blitz's chained scrolling.
    pub(crate) fn ios_pan_start(
        &self,
        node_id: NodeId,
        allow_x: bool,
        allow_y: bool,
    ) -> Option<IosPan> {
        if self.scroll_physics != ScrollPhysics::Ios {
            return None;
        }

        let mut target = Some(self.canonical_scroll_target(ScrollTarget::Node(node_id)));
        while let Some(current) = target {
            if let ScrollTarget::Node(id) = current {
                if self.scrolls_own_content(id, allow_x, allow_y)? {
                    return None;
                }
            }

            let (offset, min, max) = self.scroll_state(current, false);
            if (allow_x && max.x > min.x) || (allow_y && max.y > min.y) {
                let extent = self.scrollport_extent(current);
                return Some(IosPan {
                    target: current,
                    raw: Point {
                        x: ios_raw_offset(offset.x, min.x, max.x, extent.x),
                        y: ios_raw_offset(offset.y, min.y, max.y, extent.y),
                    },
                });
            }
            target = self
                .next_scroller_in_chain(current)
                .map(|next| self.canonical_scroll_target(next));
        }
        None
    }

    /// Whether a text input or sub-document can scroll its own content on an allowed axis,
    /// or `None` when the node isn't in the document.
    fn scrolls_own_content(&self, node_id: NodeId, allow_x: bool, allow_y: bool) -> Option<bool> {
        let node = self.nodes.get(node_id)?;
        if let Some(sub_doc) = node.subdoc() {
            let (_, min, max) = sub_doc.inner().scroll_state(ScrollTarget::Viewport, false);
            return Some((allow_x && max.x > min.x) || (allow_y && max.y > min.y));
        }
        let Some(input) = node.element_data().and_then(|el| el.text_input_data()) else {
            return Some(false);
        };
        // Single-line inputs scroll their text horizontally, multi-line inputs vertically.
        let allowed = if input.is_multiline { allow_y } else { allow_x };
        let layout = node.final_layout();
        let range =
            input.max_scroll_offset(layout.content_box_width(), layout.content_box_height());
        Some(allowed && range > 0.0)
    }

    /// Move an iOS pan's scroller by a finger delta, rubber-banding past either end.
    pub(crate) fn ios_pan_by(
        &mut self,
        pan: &mut IosPan,
        dx: f64,
        dy: f64,
        dispatch_event: &mut dyn FnMut(DomEvent),
    ) -> bool {
        // The finger moves the content, so the offset moves the opposite way.
        pan.raw.x -= dx;
        pan.raw.y -= dy;
        let (_, min, max) = self.scroll_state(pan.target, false);
        let extent = self.scrollport_extent(pan.target);
        let offset = Point {
            x: ios_displayed_offset(pan.raw.x, min.x, max.x, extent.x),
            y: ios_displayed_offset(pan.raw.y, min.y, max.y, extent.y),
        };
        self.write_scroll_offset(pan.target, offset, dispatch_event)
    }

    /// Start the motion that follows an iOS release of `target` at `time` seconds, with the
    /// finger moving at `velocity` px/s.
    pub(crate) fn ios_release(&mut self, target: ScrollTarget, time: f64, velocity: Point<f64>) {
        let (offset, min, max) = self.scroll_state(target, false);
        let extent = self.scrollport_extent(target);
        let x = Self::ios_axis_release(time, offset.x, velocity.x, min.x, max.x, extent.x);
        let y = Self::ios_axis_release(time, offset.y, velocity.y, min.y, max.y, extent.y);
        if x == IosAxisMotion::Rest && y == IosAxisMotion::Rest {
            return;
        }
        self.scroll_animation = ScrollAnimationState::Ios(IosScrollState { target, x, y });
        self.shell_provider.request_redraw();
    }

    /// Return a scroller caught by a touch to its scroll range once the finger lifts at
    /// `time` seconds: it bounces back, or snaps when another scroll animation is running.
    pub(crate) fn ios_settle(&mut self, target: ScrollTarget, time: f64) {
        match &self.scroll_animation {
            ScrollAnimationState::None => self.ios_release(target, time, Point::ZERO),
            ScrollAnimationState::Ios(state) if state.target == target => {}
            _ => self.clamp_scroll_offset(target),
        }
    }

    /// Return a scroller left overscrolled by the iOS physics to its scroll range at once.
    fn clamp_scroll_offset(&mut self, target: ScrollTarget) {
        let (offset, min, max) = self.scroll_state(target, false);
        let clamped = Point {
            x: offset.x.clamp(min.x, max.x),
            y: offset.y.clamp(min.y, max.y),
        };
        let mut events = Vec::new();
        self.write_scroll_offset(target, clamped, &mut |event| events.push(event));
        self.queue_scroll_events(events);
    }

    fn ios_axis_release(
        time: f64,
        offset: f64,
        finger_velocity: f64,
        min: f64,
        max: f64,
        extent: f64,
    ) -> IosAxisMotion {
        // The finger moves the content, so the offset moves the opposite way. Past an end the
        // displayed offset moves only as fast as the rubber band lets it.
        let overshoot = (min - offset).max(offset - max);
        let velocity = if overshoot > 0.0 && extent > 0.0 {
            -finger_velocity * rubber_band_slope(rubber_band_inverse(overshoot, extent), extent)
        } else {
            -finger_velocity
        };
        IosAxisMotion::release(time, offset, velocity, min, max)
    }
}

#[cfg(test)]
mod ios_tests {
    use super::*;

    #[test]
    fn rubber_band_inverts() {
        for x in [0.0, 1.0, 10.0, 100.0, 1000.0] {
            let y = rubber_band(x, 200.0);
            assert!((rubber_band_inverse(y, 200.0) - x).abs() < 1e-9);
        }
    }

    #[test]
    fn rubber_band_slope_matches_a_finite_difference() {
        let (x, h) = (50.0, 1e-6);
        let numeric = (rubber_band(x + h, 200.0) - rubber_band(x - h, 200.0)) / (2.0 * h);
        assert!((rubber_band_slope(x, 200.0) - numeric).abs() < 1e-6);
    }

    #[test]
    fn displayed_offset_bands_only_with_a_scroll_range() {
        assert_eq!(ios_displayed_offset(50.0, 0.0, 100.0, 200.0), 50.0);
        assert_eq!(ios_displayed_offset(-50.0, 0.0, 0.0, 200.0), 0.0);
        assert!(ios_displayed_offset(-50.0, 0.0, 100.0, 200.0) < 0.0);
    }

    #[test]
    fn fling_hands_over_to_the_bounce_at_the_end() {
        let fling = IosAxisMotion::release(0.0, 90.0, 1000.0, 0.0, 100.0);
        let (offset, motion) = fling.sample(0.2, 0.0, 100.0);
        assert!(matches!(motion, IosAxisMotion::Bounce { end: 100.0, .. }));
        assert!(offset.is_some_and(|offset| offset > 100.0));
        let (offset, motion) = motion.sample(3.0, 0.0, 100.0);
        assert_eq!((offset, motion), (Some(100.0), IosAxisMotion::Rest));
    }

    #[test]
    fn slow_release_in_range_rests() {
        assert_eq!(
            IosAxisMotion::release(0.0, 50.0, 0.4, 0.0, 100.0),
            IosAxisMotion::Rest
        );
    }
}
