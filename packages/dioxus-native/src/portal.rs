//! Render targets that portals write into.

use std::rc::Rc;

use dioxus_core::{Runtime, use_hook_with_cleanup};

pub use dioxus_core::{Portal, RenderTargetId};

/// Creates an overlay render target, removed on drop; currently its writes are dropped.
pub fn use_portal_target() -> RenderTargetId {
    use_render_target()
}

/// Creates a render target for the `data-portal-outlet="key"` element; currently its writes are dropped.
pub fn use_portal_outlet(key: &str) -> RenderTargetId {
    let _ = key;
    use_render_target()
}

fn use_render_target() -> RenderTargetId {
    // A weak handle, because the runtime owns this cleanup closure.
    let runtime = Rc::downgrade(&Runtime::current());
    use_hook_with_cleanup(
        || Runtime::current().create_render_target(),
        move |target| {
            if let Some(runtime) = runtime.upgrade() {
                runtime.remove_render_target(target);
            }
        },
    )
}
