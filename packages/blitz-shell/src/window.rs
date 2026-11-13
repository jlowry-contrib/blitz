use crate::BlitzShellProvider;
use crate::convert_events::{
    button_source_to_blitz, color_scheme_to_theme, pointer_kind_to_blitz, pointer_source_to_blitz,
    pointer_source_to_blitz_details, theme_to_color_scheme, winit_ime_to_blitz,
    winit_key_event_to_blitz, winit_modifiers_to_kbt_modifiers,
};
use crate::event::{BlitzShellEvent, BlitzShellProxy, create_waker};
use crate::frame::{FrameClock, FrameHook, FrameScheduler, StepAck, TimeSource};
use anyrender::WindowRenderer;
use blitz_dom::{BaseDocument, CSSPixel, Document, SideOffsets2D};
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, BlitzWheelEvent, MouseEventButton,
    MouseEventButtons, PointerCoords, PointerDetails, UiEvent,
};
use blitz_traits::shell::{FrameTime, SafeArea, Viewport};
use winit::dpi::{LogicalPosition, PhysicalInsets, PhysicalPosition, PhysicalSize};
use winit::keyboard::PhysicalKey;

use atomic_refcell::AtomicRefCell;
use std::any::Any;
use std::sync::Arc;
use std::task::Waker;
use winit::event::{ButtonSource, ElementState, MouseButton};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Theme, WindowAttributes, WindowId};
use winit::{event::Modifiers, event::WindowEvent, keyboard::KeyCode, window::Window};

#[cfg(feature = "accessibility")]
use crate::accessibility::AccessibilityState;

// Ignore safe_area_insets on macOS because we don't want to avoid
// drawing in the titlebar.
#[cfg(target_os = "macos")]
fn get_safe_area_insets(_window: &dyn Window) -> PhysicalInsets<u32> {
    Default::default()
}
#[cfg(not(target_os = "macos"))]
fn get_safe_area_insets(window: &dyn Window) -> PhysicalInsets<u32> {
    window.safe_area()
}

/// Returns the viewport size and its offset in the surface for a safe-area mode.
///
/// Under [`SafeArea::Avoid`] the viewport is the surface less the insets, offset by the top
/// and left insets. Under [`SafeArea::Ignore`] it is the whole surface, at the origin.
pub fn viewport_geometry(
    surface: PhysicalSize<u32>,
    insets: PhysicalInsets<u32>,
    mode: SafeArea,
) -> (PhysicalSize<u32>, (u32, u32)) {
    match mode {
        SafeArea::Ignore => (surface, (0, 0)),
        SafeArea::Avoid => (
            PhysicalSize::new(
                surface.width.saturating_sub(insets.left + insets.right),
                surface.height.saturating_sub(insets.top + insets.bottom),
            ),
            (insets.left, insets.top),
        ),
    }
}

/// Returns the new viewport size when the insets changed at an unchanged surface size.
fn refreshed_viewport(
    surface: PhysicalSize<u32>,
    old: PhysicalInsets<u32>,
    new: PhysicalInsets<u32>,
    mode: SafeArea,
) -> Option<PhysicalSize<u32>> {
    (old != new).then(|| viewport_geometry(surface, new, mode).0)
}

/// Converts physical insets into CSS-px offsets at `scale`.
fn css_insets(insets: PhysicalInsets<u32>, scale: f64) -> SideOffsets2D<f32, CSSPixel> {
    let css = |px: u32| (px as f64 / scale) as f32;
    SideOffsets2D::new(
        css(insets.top),
        css(insets.right),
        css(insets.bottom),
        css(insets.left),
    )
}

/// Converts a physical pointer position into CSS-px coordinates, net of the safe-area insets.
pub fn pointer_coords(
    position: PhysicalPosition<f64>,
    scale: f64,
    insets: PhysicalInsets<u32>,
    scroll: blitz_dom::Point<f64>,
) -> PointerCoords {
    let LogicalPosition::<f32> {
        x: screen_x,
        y: screen_y,
    } = position.to_logical(scale);
    let client_x = screen_x - (insets.left as f64 / scale) as f32;
    let client_y = screen_y - (insets.top as f64 / scale) as f32;
    let page_x = client_x + scroll.x as f32;
    let page_y = client_y + scroll.y as f32;

    PointerCoords {
        screen_x,
        screen_y,
        client_x,
        client_y,
        page_x,
        page_y,
    }
}

/// Passes the document's changes to `update` and clears them, unless it is still
/// waiting on render-blocking resources or nothing changed.
fn take_tree_changes(doc: &mut BaseDocument, update: impl FnOnce(&BaseDocument)) {
    if !doc.has_pending_critical_resources() && doc.has_changes() {
        update(doc);
        doc.clear_changes();
    }
}

pub struct WindowConfig<Rend: WindowRenderer> {
    doc: Box<dyn Document>,
    pub(crate) attributes: WindowAttributes,
    renderer: Rend,
    time_source: TimeSource,
    frame_hook: Option<FrameHook<Rend>>,
    safe_area: SafeArea,
}

impl<Rend: WindowRenderer> WindowConfig<Rend> {
    pub fn new(doc: Box<dyn Document>, renderer: Rend) -> Self {
        Self::with_attributes(doc, renderer, WindowAttributes::default())
    }

    pub fn with_attributes(
        doc: Box<dyn Document>,
        renderer: Rend,
        attributes: WindowAttributes,
    ) -> Self {
        WindowConfig {
            doc,
            attributes,
            renderer,
            time_source: TimeSource::default(),
            frame_hook: None,
            safe_area: SafeArea::default(),
        }
    }

    /// Sets whether the document is laid out inside the safe area or under the whole window.
    pub fn with_safe_area(mut self, safe_area: SafeArea) -> Self {
        self.safe_area = safe_area;
        self
    }

    /// Sets the clock that drives the window's frames.
    pub fn with_time_source(mut self, time_source: TimeSource) -> Self {
        self.time_source = time_source;
        self
    }

    /// Sets a hook that each painted frame calls just before painting.
    ///
    /// It receives the renderer, the frame time, the document scale and the colour scheme.
    pub fn with_frame_hook(mut self, hook: FrameHook<Rend>) -> Self {
        self.frame_hook = Some(hook);
        self
    }
}

pub struct View<Rend: WindowRenderer> {
    pub doc: Box<dyn Document>,

    pub renderer: Rend,
    pub waker: Option<Waker>,

    pub proxy: BlitzShellProxy,
    pub window: Arc<dyn Window>,

    /// The state of the keyboard modifiers (ctrl, shift, etc). Winit/Tao don't track these for us so we
    /// need to store them in order to have access to them when processing keypress events
    pub theme_override: Option<Theme>,
    pub keyboard_modifiers: Modifiers,
    pub buttons: MouseEventButtons,
    pub pointer_pos: PhysicalPosition<f64>,
    /// The non-mouse pointers (touch/pen) that are currently pressed, in the
    /// order they were pressed.
    ///
    /// This serves two purposes:
    /// - Multi-touch: it is cloned (cheaply, via [`Arc`]) into every dispatched
    ///   [`BlitzPointerEvent`] so that touch events can report all concurrent
    ///   touches via their `touches` list.
    /// - Cancellation detection: winit signals a cancelled touch with a
    ///   [`WindowEvent::PointerLeft`] that is *not* preceded by a
    ///   [`WindowEvent::PointerButton`] with [`ElementState::Released`]. If a
    ///   pointer is still in this list when it leaves, it was cancelled.
    ///
    /// The events stored here always have an empty `active_pointers` list to
    /// avoid a reference cycle.
    pub active_events: Arc<AtomicRefCell<Vec<BlitzPointerEvent>>>,
    scheduler: FrameScheduler<dyn Document, Rend>,
    pub is_visible: bool,
    pub safe_area_insets: PhysicalInsets<u32>,
    /// Whether the document avoids the safe area or renders under it.
    safe_area: SafeArea,
    /// The size of the window surface the renderer draws into.
    surface_size: PhysicalSize<u32>,

    #[cfg(target_arch = "wasm32")]
    pending_resize: Option<winit::dpi::PhysicalSize<u32>>,
    #[cfg(target_arch = "wasm32")]
    last_resize_at: Option<web_time::Instant>,
    /// True iff a setTimeout has been scheduled and not yet observed by
    /// `apply_pending_resize_if_settled`. Prevents the timer storm that would
    /// otherwise allocate a fresh `Closure` per resize event during a drag.
    #[cfg(target_arch = "wasm32")]
    resize_timer_scheduled: bool,

    #[cfg(feature = "accessibility")]
    /// Accessibility adapter for `accesskit`.
    pub accessibility: AccessibilityState,

    // Calling request_redraw within a WindowEvent doesn't work on iOS. So on iOS we track the state
    // with a boolean and call request_redraw in about_to_wait
    //
    // See https://github.com/rust-windowing/winit/issues/3406
    #[cfg(target_os = "ios")]
    pub ios_request_redraw: std::cell::Cell<bool>,
}

impl<Rend: WindowRenderer> Drop for View<Rend> {
    fn drop(&mut self) {
        // Release the renderer's window surface before the window is dropped.
        // The renderer may be shared (e.g. provided as a context to user code),
        // in which case it can outlive the `View`. A GPU surface must not
        // outlive the window/display it is attached to: dropping it after the
        // event loop has shut down segfaults on Wayland.
        self.renderer.suspend();
    }
}

impl<Rend: WindowRenderer> View<Rend> {
    pub fn init(
        config: WindowConfig<Rend>,
        event_loop: &dyn ActiveEventLoop,
        proxy: &BlitzShellProxy,
    ) -> Self {
        // We create window as invisble and then later make window visible
        // after AccessKit has initialised to avoid AccessKit panics
        let is_visible = config.attributes.visible;
        // Capture the requested surface size before consuming `attributes`, so we can
        // seed the viewport on platforms (winit-web) that report `surface_size() == 0×0`
        // until a layout pass fires.
        let requested_surface_size = config.attributes.surface_size;
        let attrs = config.attributes.with_visible(false);

        let winit_window: Arc<dyn Window> = Arc::from(event_loop.create_window(attrs).unwrap());
        #[cfg(feature = "accessibility")]
        let accessibility = AccessibilityState::new(&*winit_window, proxy.clone());

        if is_visible {
            winit_window.set_visible(true);
        }

        // Create viewport
        let scale = winit_window.scale_factor() as f32;
        let mut size = winit_window.surface_size();
        if (size.width == 0 || size.height == 0)
            && let Some(requested) = requested_surface_size
        {
            size = requested.to_physical(scale as f64);
        }
        // On wasm, when the embedder didn't call `with_surface_size`, winit-web's
        // initial `surface_size()` is 0×0 — its ResizeObserver hasn't fired yet.
        // Resuming the renderer at 0×0 trips a wgpu swapchain-size-0 error, so
        // seed from the canvas element's CSS layout box (host-stylesheet result).
        #[cfg(target_arch = "wasm32")]
        if size.width == 0 || size.height == 0 {
            use winit::platform::web::WindowExtWeb;
            if let Some(canvas) = winit_window.canvas() {
                let css_w = canvas.offset_width().max(0) as u32;
                let css_h = canvas.offset_height().max(0) as u32;
                if css_w > 0 && css_h > 0 {
                    size = winit::dpi::LogicalSize::new(css_w, css_h).to_physical(scale as f64);
                }
            }
        }
        let safe_area_insets = get_safe_area_insets(&*winit_window);
        let theme = winit_window.theme().unwrap_or(Theme::Light);
        let color_scheme = theme_to_color_scheme(theme);
        let (viewport_size, _) = viewport_geometry(size, safe_area_insets, config.safe_area);
        let viewport = Viewport::new(
            viewport_size.width,
            viewport_size.height,
            scale,
            color_scheme,
        );

        // Create shell provider
        let shell_provider =
            BlitzShellProvider::new(winit_window.clone(), proxy.clone(), config.doc.id());

        let mut doc = config.doc;
        let mut inner = doc.inner_mut();
        inner.set_viewport(viewport);
        inner.set_safe_area_insets(css_insets(safe_area_insets, scale as f64));
        inner.set_shell_provider(Arc::new(shell_provider));

        // If the document title is set prior to the window being created then it will
        // have been sent to a dummy ShellProvider and won't get picked up.
        // So we look for it here and set it if present.
        let title = inner.find_title_node().map(|node| node.text_content());
        if let Some(title) = title {
            winit_window.set_title(&title);
        }

        drop(inner);

        let mut scheduler = FrameScheduler::new(config.time_source);
        scheduler.set_frame_hook(config.frame_hook);

        Self {
            renderer: config.renderer,
            waker: None,
            scheduler,
            keyboard_modifiers: Default::default(),
            proxy: proxy.clone(),
            window: winit_window.clone(),
            doc,
            theme_override: None,
            buttons: MouseEventButtons::None,
            active_events: Arc::new(AtomicRefCell::new(Vec::new())),
            safe_area_insets,
            safe_area: config.safe_area,
            surface_size: size,
            #[cfg(target_arch = "wasm32")]
            pending_resize: None,
            #[cfg(target_arch = "wasm32")]
            last_resize_at: None,
            #[cfg(target_arch = "wasm32")]
            resize_timer_scheduled: false,
            pointer_pos: Default::default(),
            is_visible: winit_window.is_visible().unwrap_or(true),
            #[cfg(feature = "accessibility")]
            accessibility,

            #[cfg(target_os = "ios")]
            ios_request_redraw: std::cell::Cell::new(false),
        }
    }

    pub fn replace_document(&mut self, new_doc: Box<dyn Document>, retain_scroll_position: bool) {
        let inner = self.doc.inner();
        let scroll = inner.viewport_scroll();
        let viewport = inner.viewport().clone();
        let safe_area_insets = inner.safe_area_insets();
        let shell_provider = inner.shell_provider.clone();
        drop(inner);

        self.doc = new_doc;

        let mut inner = self.doc.inner_mut();
        inner.set_viewport(viewport);
        inner.set_safe_area_insets(safe_area_insets);
        inner.set_shell_provider(shell_provider);
        drop(inner);

        self.poll();
        self.request_redraw();

        if retain_scroll_position {
            self.doc.inner_mut().set_viewport_scroll(scroll);
        }
    }

    pub fn theme_override(&self) -> Option<Theme> {
        self.theme_override
    }

    pub fn current_theme(&self) -> Theme {
        color_scheme_to_theme(self.doc.inner().viewport().color_scheme)
    }

    pub fn set_theme_override(&mut self, theme: Option<Theme>) {
        self.theme_override = theme;
        let theme = theme.or(self.window.theme()).unwrap_or(Theme::Light);
        self.with_viewport(|v| v.color_scheme = theme_to_color_scheme(theme));
    }

    pub fn downcast_doc_mut<T: 'static>(&mut self) -> &mut T {
        (&mut *self.doc as &mut dyn Any)
            .downcast_mut::<T>()
            .unwrap()
    }

    /// Returns the time, in seconds, of the latest frame.
    pub fn current_animation_time(&self) -> f64 {
        self.scheduler.frame_time().t
    }

    /// Returns the time of the latest frame.
    pub fn frame_time(&self) -> FrameTime {
        self.scheduler.frame_time()
    }

    /// Returns a handle that steps this window, when it runs on the manual clock.
    pub fn frame_clock(&self) -> Option<FrameClock> {
        (self.scheduler.time_source() == TimeSource::Manual)
            .then(|| FrameClock::new(self.proxy.clone(), self.window_id()))
    }
}

impl<Rend: WindowRenderer> View<Rend> {
    /// Start resuming the renderer. Dispatches [`BlitzShellEvent::ResumeReady`]
    /// when initialization completes — synchronously on native, asynchronously
    /// on wasm32. The embedder must call [`complete_resume`](Self::complete_resume)
    /// in response.
    pub fn resume(&mut self) {
        let window_id = self.window_id();
        let animation_time = self.current_animation_time();

        self.doc.inner_mut().resolve(animation_time);

        // The render surface covers the entire window, including the safe area
        let (surface, _) = self.geometry();

        let proxy = self.proxy.clone();
        self.renderer.resume(
            Arc::new(self.window.clone()),
            surface.width,
            surface.height,
            move || {
                proxy.send_event(BlitzShellEvent::ResumeReady { window_id });
            },
        );
    }

    /// Finalize a previously-started resume. Should be called in response to a
    /// [`BlitzShellEvent::ResumeReady`] event. Paints the first frame and
    /// installs the doc poll waker. Returns `true` if the renderer is now active.
    pub fn complete_resume(&mut self) -> bool {
        if !self.renderer.complete_resume() {
            return false;
        }

        let window_id = self.window_id();

        // Resync the renderer to the current viewport. Resize/scale events that
        // arrived while the renderer was Pending were no-ops on the renderer
        // (its `set_size` only matches Active), so the surface created during
        // resume could be at a stale size by the time we get here.
        let animation_time = self.current_animation_time();
        let (surface, (offset_x, offset_y)) = self.geometry();
        let mut inner = self.doc.inner_mut();
        inner.resolve(animation_time);
        let (width, height) = inner.viewport().window_size;
        let scale = inner.viewport().scale_f64();

        #[cfg(feature = "custom-widget")]
        inner.can_create_surfaces(&mut self.renderer as _);

        // The render surface covers the entire window, including the safe area
        self.renderer.set_size(surface.width, surface.height);

        let time = self.scheduler.frame_time();
        let scheme = inner.viewport().color_scheme;
        self.scheduler
            .run_hook(&mut self.renderer, time, scale, scheme);

        self.renderer.render(|scene| {
            paint_scene(scene, &mut inner, scale, width, height, offset_x, offset_y)
        });

        let waker = create_waker(&self.proxy, window_id);
        self.scheduler.set_waker(Some(waker.clone()));
        self.waker = Some(waker);
        true
    }

    pub fn suspend(&mut self) {
        self.waker = None;
        self.scheduler.set_waker(None);
        self.renderer.suspend();

        #[cfg(feature = "custom-widget")]
        self.doc.inner_mut().destroy_surfaces();
    }

    pub fn poll(&mut self) -> bool {
        if let Some(waker) = &self.waker {
            let cx = std::task::Context::from_waker(waker);
            if self.scheduler.poll(&mut *self.doc, Some(cx)) {
                self.request_redraw();
                return true;
            }
        }

        false
    }

    pub fn request_redraw(&self) {
        if self.renderer.is_active() {
            self.window.request_redraw();
            #[cfg(target_os = "ios")]
            self.ios_request_redraw.set(true);
        }
    }

    pub fn redraw(&mut self) {
        #[cfg(target_os = "ios")]
        self.ios_request_redraw.set(false);
        self.refresh_safe_area_insets();
        let can_paint = self.is_visible && self.renderer.is_active();
        let (_, (offset_x, offset_y)) = self.geometry();

        let outcome = self.scheduler.frame(
            &mut *self.doc,
            &mut self.renderer,
            can_paint,
            |doc, renderer| {
                let mut inner = doc.inner_mut();

                // Unregister resources (e.g. textures) from dropped custom widget nodes.
                // Deliberately only on frames that paint: the renderer may be suspended
                // otherwise, and the pending ids wait for the next painted frame.
                #[cfg(feature = "custom-widget")]
                for id in inner.take_pending_resource_deallocations() {
                    renderer.unregister_resource(id);
                }

                let (width, height) = inner.viewport().window_size;
                let scale = inner.viewport().scale_f64();
                renderer.render(|scene| {
                    paint_scene(scene, &mut inner, scale, width, height, offset_x, offset_y)
                });
            },
        );

        // Push the accessibility tree after resolve so that it reflects the current
        // DOM, styles, layout and focus, whether or not the frame painted.
        let mut inner = self.doc.inner_mut();
        #[cfg(feature = "accessibility")]
        let accessibility = &mut self.accessibility;
        take_tree_changes(&mut inner, |_doc| {
            #[cfg(feature = "accessibility")]
            accessibility.update_tree(_doc);
        });
        drop(inner);

        if outcome.request_redraw {
            self.request_redraw();
        }
    }

    /// Queues a step of `dt` seconds, then draws frames until every queued step has run.
    ///
    /// Each frame acknowledges its step, unpainted if the window can't paint.
    pub fn step(&mut self, dt: f64, ack: StepAck) {
        self.scheduler.step(dt, ack);
        while self.scheduler.has_pending_steps() {
            self.redraw();
        }
    }

    pub fn pointer_coords(&self, position: PhysicalPosition<f64>) -> PointerCoords {
        let (_, (left, top)) = self.geometry();
        let inner = self.doc.inner();
        pointer_coords(
            position,
            inner.viewport().scale_f64(),
            PhysicalInsets::new(top, left, 0, 0),
            inner.viewport_scroll(),
        )
    }

    /// Returns the surface size and the viewport's offset in it, under this window's safe-area mode.
    fn geometry(&self) -> (PhysicalSize<u32>, (u32, u32)) {
        let (_, offset) =
            viewport_geometry(self.surface_size, self.safe_area_insets, self.safe_area);
        (self.surface_size, offset)
    }

    /// Lays the document out for a new surface size, refreshing the safe-area insets.
    fn resize_surface(&mut self, size: PhysicalSize<u32>) {
        self.safe_area_insets = get_safe_area_insets(&*self.window);
        self.surface_size = size;
        let (viewport, _) = viewport_geometry(size, self.safe_area_insets, self.safe_area);
        self.with_viewport(|v| v.window_size = (viewport.width, viewport.height));
        self.sync_safe_area_insets();
        self.request_redraw();
    }

    /// Re-reads the safe-area insets, which winit can change without a resize.
    ///
    /// On iOS a rotation or status-bar change only requests a redraw.
    fn refresh_safe_area_insets(&mut self) {
        let insets = get_safe_area_insets(&*self.window);
        let viewport = refreshed_viewport(
            self.surface_size,
            self.safe_area_insets,
            insets,
            self.safe_area,
        );
        if let Some(viewport) = viewport {
            self.safe_area_insets = insets;
            self.with_viewport(|v| v.window_size = (viewport.width, viewport.height));
            self.sync_safe_area_insets();
        }
    }

    /// Passes the safe-area insets to the document, in CSS px.
    fn sync_safe_area_insets(&mut self) {
        let mut inner = self.doc.inner_mut();
        let scale = inner.viewport().scale_f64();
        inner.set_safe_area_insets(css_insets(self.safe_area_insets, scale));
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Store `event` as an active pointer, replacing any existing entry with the
    /// same id. The stored event has an empty `active_pointers` list to avoid a
    /// reference cycle.
    fn set_active_pointer(&self, event: &BlitzPointerEvent) {
        let mut stored = event.clone();
        stored.active_pointers = Default::default();

        let mut active = self.active_events.borrow_mut();
        if let Some(existing) = active.iter_mut().find(|e| e.id == stored.id) {
            *existing = stored;
        } else {
            active.push(stored);
        }
    }

    /// Update the stored position/state of an already-active pointer. Does
    /// nothing if the pointer is not currently active (e.g. a hovering pen).
    fn update_active_pointer(&self, event: &BlitzPointerEvent) {
        let mut active = self.active_events.borrow_mut();
        if let Some(existing) = active.iter_mut().find(|e| e.id == event.id) {
            let mut stored = event.clone();
            stored.active_pointers = Default::default();
            *existing = stored;
        }
    }

    /// Remove an active pointer by id. Returns `true` if it was present.
    fn remove_active_pointer(&self, id: BlitzPointerId) -> bool {
        let mut active = self.active_events.borrow_mut();
        let len_before = active.len();
        active.retain(|e| e.id != id);
        active.len() != len_before
    }

    /// Updates the viewport through `cb`, then sizes the renderer to the stored surface size.
    ///
    /// The renderer is not resized to the viewport: the surface size changes only on resize.
    #[inline]
    pub fn with_viewport(&mut self, cb: impl FnOnce(&mut Viewport)) {
        let mut inner = self.doc.inner_mut();
        let mut viewport = inner.viewport_mut();
        cb(&mut viewport);
        let (width, height) = viewport.window_size;
        drop(viewport);
        drop(inner);
        if width > 0 && height > 0 {
            self.renderer
                .set_size(self.surface_size.width, self.surface_size.height);
            self.request_redraw();
        }
    }

    #[cfg(feature = "accessibility")]
    pub fn build_accessibility_tree(&mut self) {
        let inner = self.doc.inner();
        self.accessibility.update_tree(&inner);
    }

    #[cfg(target_arch = "wasm32")]
    const RESIZE_DEBOUNCE_MS: u32 = 100;

    #[cfg(target_arch = "wasm32")]
    fn schedule_resize_settle_check(&mut self, delay_ms: u32) {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;

        let proxy = self.proxy.clone();
        let window_id = self.window_id();
        let cb = Closure::once_into_js(move || {
            proxy.send_event(BlitzShellEvent::ResizeSettleCheck { window_id });
        });
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                delay_ms as i32,
            );
            self.resize_timer_scheduled = true;
        }
    }

    /// Applies the pending resize iff motion has been quiet for the debounce
    /// window; otherwise re-arms the timer for the remaining time. Called
    /// when a previously scheduled timer fires.
    #[cfg(target_arch = "wasm32")]
    pub fn apply_pending_resize_if_settled(&mut self) {
        self.resize_timer_scheduled = false;
        let Some(last) = self.last_resize_at else {
            return;
        };
        let debounce = std::time::Duration::from_millis(Self::RESIZE_DEBOUNCE_MS as u64);
        let elapsed = web_time::Instant::now().saturating_duration_since(last);
        if elapsed < debounce {
            // Motion ongoing — wait out the rest of the window before re-checking.
            let remaining_ms = (debounce - elapsed).as_millis() as u32;
            self.schedule_resize_settle_check(remaining_ms);
            return;
        }
        let Some(size) = self.pending_resize.take() else {
            return;
        };
        self.last_resize_at = None;
        self.resize_surface(size);
    }

    #[cfg(target_os = "macos")]
    pub fn handle_apple_standard_keybinding(&mut self, command: &str) {
        use blitz_traits::SmolStr;
        let event = UiEvent::AppleStandardKeybinding(SmolStr::new(command));
        self.doc.handle_ui_event(event);
    }

    pub fn handle_winit_event(&mut self, event: WindowEvent) {
        // Update accessibility focus and window size state in response to a Winit WindowEvent
        #[cfg(feature = "accessibility")]
        self.accessibility
            .process_window_event(&*self.window, &event);

        let time_stamp = self.scheduler.now();

        match event {
            WindowEvent::Destroyed => {}
            WindowEvent::ActivationTokenDone { .. } => {},
            WindowEvent::CloseRequested => {
                // Currently handled at the level above in application.rs
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
            }
            WindowEvent::Moved(_) => {}
            WindowEvent::Occluded(is_occluded) => {
                self.is_visible = !is_occluded;
                if self.is_visible {
                    self.request_redraw();
                }
            },
            WindowEvent::SurfaceResized(physical_size) => {
                // On WASM, defer the apply: wgpu's surface.configure clears the canvas,
                // so running it every frame flickers during a drag. The browser stretches
                // the stale backing store until the debounce timer settles.
                #[cfg(target_arch = "wasm32")]
                {
                    self.pending_resize = Some(physical_size);
                    self.last_resize_at = Some(web_time::Instant::now());
                    if !self.resize_timer_scheduled {
                        self.schedule_resize_settle_check(Self::RESIZE_DEBOUNCE_MS);
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.resize_surface(physical_size);
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.with_viewport(|v| v.set_hidpi_scale(scale_factor as f32));
                self.sync_safe_area_insets();
                self.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                let color_scheme = theme_to_color_scheme(self.theme_override.unwrap_or(theme));
                let mut inner = self.doc.inner_mut();
                inner.viewport_mut().color_scheme = color_scheme;
            }
            WindowEvent::Ime(ime_event) => {
                self.doc.handle_ui_event(UiEvent::Ime(winit_ime_to_blitz(ime_event)));
                self.request_redraw();
            },
            WindowEvent::ModifiersChanged(new_state) => {
                // Store new keyboard modifier (ctrl, shift, etc) state for later use
                self.keyboard_modifiers = new_state;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key_code) = event.physical_key && event.state.is_pressed() {
                        let ctrl = self.keyboard_modifiers.state().control_key();
                        let meta = self.keyboard_modifiers.state().meta_key();
                        let alt = self.keyboard_modifiers.state().alt_key();

                        // Ctrl/Super keyboard shortcuts
                        if ctrl | meta {
                            match key_code {
                                KeyCode::Equal => {
                                    self.doc.inner_mut().viewport_mut().zoom_by(0.1);
                                },
                                KeyCode::Minus => {
                                    self.doc.inner_mut().viewport_mut().zoom_by(-0.1);
                                },
                                KeyCode::Digit0 => {
                                    self.doc.inner_mut().viewport_mut().set_zoom(1.0);
                                }
                                _ => {}
                            };
                        }

                        // Alt keyboard shortcuts
                        if alt {
                            match key_code {
                                KeyCode::KeyD => {
                                    let mut inner = self.doc.inner_mut();
                                    inner.devtools_mut().toggle_show_layout();
                                    drop(inner);
                                    self.request_redraw();
                                }
                                KeyCode::KeyH => {
                                    let mut inner = self.doc.inner_mut();
                                    inner.devtools_mut().toggle_highlight_hover();
                                    drop(inner);
                                    self.request_redraw();
                                }
                                KeyCode::KeyT => self.doc.inner_mut().print_taffy_tree(),
                                _ => {}
                            };
                        }

                }

                // Unmodified keypresses
                let key_event_data = winit_key_event_to_blitz(&event, self.keyboard_modifiers.state());
                let event = if event.state.is_pressed() {
                    UiEvent::KeyDown(key_event_data)
                } else {
                    UiEvent::KeyUp(key_event_data)
                };

                self.doc.handle_ui_event(event);
            }
            WindowEvent::PointerEntered { /*device_id*/.. } => {}
            WindowEvent::PointerLeft { position, primary, kind, .. } => {
                let id = pointer_kind_to_blitz(&kind);

                // A `PointerLeft` for a non-mouse pointer that is still pressed
                // (i.e. we never saw a `PointerButton` with `Released` for it)
                // means the system cancelled tracking of this touch/pen. Emit a
                // pointercancel in that case. A mouse simply leaving the window,
                // or a touch that was already released, is not a cancellation.
                // Remove from the active list first so the cancelled pointer is
                // excluded from this event's `touches`. `remove_active_pointer`
                // reports whether the pointer was actually active.
                if id != BlitzPointerId::Mouse && self.remove_active_pointer(id) {
                    let position = position.unwrap_or(self.pointer_pos);
                    self.pointer_pos = position;

                    // The pointer is no longer pressed.
                    self.buttons ^= MouseEventButton::Main.into();

                    let event = BlitzPointerEvent {
                        id,
                        is_primary: primary,
                        coords: self.pointer_coords(position),
                        button: MouseEventButton::Main,
                        buttons: self.buttons,
                        mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                        details: PointerDetails::default(),
                        element: Default::default(),
                        time_stamp,
                        active_pointers: Arc::clone(&self.active_events),
                    };

                    self.doc.handle_ui_event(UiEvent::PointerCancel(event));
                }

                // The pointer left the window: end hover
                let position = position.unwrap_or(self.pointer_pos);
                let event = BlitzPointerEvent {
                    id,
                    is_primary: primary,
                    coords: self.pointer_coords(position),
                    button: Default::default(),
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                    details: PointerDetails::default(),
                    element: Default::default(),
                    time_stamp,
                    active_pointers: Arc::clone(&self.active_events),
                };

                self.doc.handle_ui_event(UiEvent::PointerLeave(event));
                self.request_redraw();
            }
            WindowEvent::PointerMoved { position, source, primary, .. } => {
                self.pointer_pos = position;
                let id = pointer_source_to_blitz(&source);
                let event = BlitzPointerEvent {
                    id,
                    is_primary: primary,
                    coords: self.pointer_coords(position),
                    button: Default::default(),
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                    details: pointer_source_to_blitz_details(&source),
                    element: Default::default(),
                    time_stamp,
                    active_pointers: Arc::clone(&self.active_events),
                };
                // Keep multi-touch positions current (no-op for non-active pointers).
                if id != BlitzPointerId::Mouse {
                    self.update_active_pointer(&event);
                }
                self.doc.handle_ui_event(UiEvent::PointerMove(event));
            }
            WindowEvent::PointerButton { button, state, primary, position, .. } => {
                let id = button_source_to_blitz(&button);
                let coords = self.pointer_coords(position);
                self.pointer_pos = position;
                let button = match &button {
                    ButtonSource::Mouse(mouse_button) => match mouse_button {
                        MouseButton::Left => MouseEventButton::Main,
                        MouseButton::Right => MouseEventButton::Secondary,
                        MouseButton::Middle => MouseEventButton::Auxiliary,
                        // TODO: handle other button types
                        _ => MouseEventButton::Auxiliary,
                    }
                    _ => MouseEventButton::Main,
                };

                match state {
                    ElementState::Pressed => self.buttons |= button.into(),
                    ElementState::Released => self.buttons ^= button.into(),
                }

                let pointer_event = BlitzPointerEvent {
                    id,
                    is_primary: primary,
                    coords,
                    button,
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),

                    // TODO: details for pointer up/down events
                    details: PointerDetails::default(),
                    element: Default::default(),
                    time_stamp,
                    active_pointers: Arc::clone(&self.active_events),
                };

                // Maintain the list of active (pressed) non-mouse pointers. A
                // press adds the pointer *before* dispatch (so touchstart's
                // `touches` includes it). A release is handled after the
                // synthetic move below so the move still sees it, but before the
                // pointerup so touchend's `touches` excludes it.
                if id != BlitzPointerId::Mouse && state == ElementState::Pressed {
                    self.set_active_pointer(&pointer_event);
                }

                // Touch input doesn't emit a `PointerMoved` before the button
                // event the way a mouse does, so synthesise a move to update the
                // hover/hit position to the touch location.
                if id != BlitzPointerId::Mouse {
                    let event = BlitzPointerEvent {
                        id,
                        is_primary: primary,
                        coords,
                        button: Default::default(),
                        buttons: self.buttons,
                        mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                        details: PointerDetails::default(),
                        element: Default::default(),
                        time_stamp,
                        active_pointers: Arc::clone(&self.active_events),
                    };
                    self.doc.handle_ui_event(UiEvent::PointerMove(event));
                }

                if id != BlitzPointerId::Mouse && state == ElementState::Released {
                    self.remove_active_pointer(id);
                }

                let event = pointer_event;

                let event = match state {
                    ElementState::Pressed => UiEvent::PointerDown(event),
                    ElementState::Released => UiEvent::PointerUp(event),
                };

                self.doc.handle_ui_event(event);
                self.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let blitz_delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => BlitzWheelDelta::Lines(x as f64, y as f64),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => BlitzWheelDelta::Pixels(pos.x, pos.y),
                    _ =>  {
                        #[cfg(feature = "tracing")]
                        tracing::error!("unsupported MouseScrollDelta variant: {delta:?}");
                        BlitzWheelDelta::Pixels(0., 0.)
                    }
                };

                let event = BlitzWheelEvent {
                    delta: blitz_delta,
                    coords: self.pointer_coords(self.pointer_pos),
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                    element: Default::default()
                };

                self.doc.handle_ui_event(UiEvent::Wheel(event));
            }
            WindowEvent::Focused(_) => {}
            WindowEvent::TouchpadPressure { .. } => {}
            WindowEvent::PinchGesture { .. } => {},
            WindowEvent::PanGesture { .. } => {},
            WindowEvent::DoubleTapGesture { .. } => {},
            WindowEvent::RotationGesture { .. } => {},
            WindowEvent::DragEntered { .. } => {},
            WindowEvent::DragDropped { .. } => {},
            WindowEvent::DragLeft { .. } => {},
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_changes_are_taken_once() {
        let mut doc = BaseDocument::new(blitz_dom::DocumentConfig::default());
        let root = doc.root_node().id;
        let mut mutator = doc.mutate();
        let name = blitz_dom::QualName::new(None, blitz_dom::ns!(html), "html".into());
        let html = mutator.create_element(name, Vec::new());
        mutator.append_children(root, &[html]);
        drop(mutator);

        let mut calls = 0;
        take_tree_changes(&mut doc, |_| calls += 1);
        take_tree_changes(&mut doc, |_| calls += 1);
        assert_eq!(calls, 1);
        assert!(!doc.has_changes());
    }

    #[test]
    fn pointer_coords_subtracts_the_insets() {
        let coords = pointer_coords(
            PhysicalPosition::new(120.0, 80.0),
            2.0,
            PhysicalInsets::new(20, 40, 0, 0),
            blitz_dom::Point { x: 10.0, y: 5.0 },
        );
        assert_eq!((coords.screen_x, coords.screen_y), (60.0, 40.0));
        assert_eq!((coords.client_x, coords.client_y), (40.0, 30.0));
        assert_eq!((coords.page_x, coords.page_y), (50.0, 35.0));
    }

    /// An iPhone 17 Pro surface at 3x, with its portrait safe-area insets.
    const SURFACE: PhysicalSize<u32> = PhysicalSize::new(1206, 2622);
    const INSETS: PhysicalInsets<u32> = PhysicalInsets::new(186, 0, 102, 0);

    #[test]
    fn ignore_mode_covers_the_whole_surface() {
        let geometry = viewport_geometry(SURFACE, INSETS, SafeArea::Ignore);
        assert_eq!(geometry, (PhysicalSize::new(1206, 2622), (0, 0)));
    }

    #[test]
    fn avoid_mode_matches_today() {
        let geometry = viewport_geometry(SURFACE, INSETS, SafeArea::Avoid);
        assert_eq!(geometry, (PhysicalSize::new(1206, 2334), (0, 186)));
    }

    #[test]
    fn insets_changed_at_same_size_updates_viewport() {
        // A 180° landscape rotation swaps the left and right insets at the same size.
        let surface = PhysicalSize::new(2622, 1206);
        let left = PhysicalInsets::new(0, 186, 63, 0);
        let right = PhysicalInsets::new(0, 0, 63, 186);
        assert_eq!(
            refreshed_viewport(surface, left, right, SafeArea::Avoid),
            Some(PhysicalSize::new(2436, 1143))
        );
        assert_eq!(
            refreshed_viewport(surface, left, right, SafeArea::Ignore),
            Some(surface)
        );
        assert_eq!(
            refreshed_viewport(surface, left, left, SafeArea::Avoid),
            None
        );
    }

    #[test]
    fn pointer_coords_under_ignore_are_screen_coords() {
        let (_, (left, top)) = viewport_geometry(SURFACE, INSETS, SafeArea::Ignore);
        let coords = pointer_coords(
            PhysicalPosition::new(300.0, 90.0),
            3.0,
            PhysicalInsets::new(top, left, 0, 0),
            blitz_dom::Point { x: 0.0, y: 0.0 },
        );
        assert_eq!((coords.screen_x, coords.screen_y), (100.0, 30.0));
        assert_eq!((coords.client_x, coords.client_y), (100.0, 30.0));
    }
}
