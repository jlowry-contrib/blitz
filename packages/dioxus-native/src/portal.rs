//! Render targets that portals write into.

use std::rc::{Rc, Weak};

use dioxus_core::{Runtime, try_consume_context, use_hook_with_cleanup};
use dioxus_native_dom::PortalHosts;

pub use dioxus_core::{Portal, RenderTargetId};

/// Creates a render target hosted in an overlay after `<main>`, removed once its owner drops.
///
/// Keep every [`Portal`] for this target inside the owner's subtree. The target and its host
/// outlive the owner while a portal's content is still mounted, but a portal that renders
/// nothing doesn't hold the target, and rendering into it after the owner drops panics in
/// dioxus-core. Outside a Dioxus Native document nothing hosts the target, so its writes are
/// dropped.
pub fn use_portal_target() -> RenderTargetId {
    let runtime = Rc::downgrade(&Runtime::current());
    let (target, _) = use_hook_with_cleanup(
        || {
            let target = Runtime::current().create_render_target();
            let hosts = try_consume_context::<PortalHosts>();
            if let Some(hosts) = &hosts {
                hosts.request_overlay(target);
            }
            (target, hosts)
        },
        move |(target, hosts)| match hosts {
            // The document removes the target, then its host, once nothing is mounted in it.
            Some(hosts) => hosts.request_removal(target),
            None => remove_unhosted(&runtime, target),
        },
    );
    target
}

/// Creates a render target hosted by the element with `data-portal-outlet="<key>"`.
///
/// The key is read once. Until both the target and such an element exist, the target renders
/// nothing; its writes wait and are applied when the element appears. If the element goes,
/// the content is kept out of the document until another one with the key appears. While
/// another target holds the key, this one waits and takes the key once that target goes.
///
/// The target is removed once its owner drops and nothing is mounted in it; the element and
/// its own children stay. Keep every [`Portal`] for this target inside the owner's subtree, as
/// with [`use_portal_target`]. Outside a Dioxus Native document nothing hosts the target, so
/// its writes are dropped.
pub fn use_portal_outlet(key: &str) -> RenderTargetId {
    // A weak handle, because the runtime owns this cleanup closure.
    let runtime = Rc::downgrade(&Runtime::current());
    let (target, _) = use_hook_with_cleanup(
        || {
            let target = Runtime::current().create_render_target();
            let hosts = try_consume_context::<PortalHosts>();
            if let Some(hosts) = &hosts {
                hosts.request_outlet(key, target);
            }
            (target, hosts)
        },
        move |(target, hosts)| match hosts {
            Some(hosts) => hosts.request_removal(target),
            None => remove_unhosted(&runtime, target),
        },
    );
    target
}

/// Removes a target no document hosts.
///
/// With nothing to retry it, a target that still has live mounts stays until the runtime drops.
fn remove_unhosted(runtime: &Weak<Runtime>, target: RenderTargetId) {
    if let Some(runtime) = runtime.upgrade() {
        runtime.remove_render_target(target);
    }
}
