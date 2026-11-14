//! Integration between Dioxus and Blitz
use crate::{
    NodeId, mutation_buffer::MutationBuffer, qual_name, trace, write_once_attr::WriteOnceAttr,
};
use blitz_dom::{BaseDocument, Document, DocumentMutator, Widget};
use blitz_traits::events::DomEventKind;
use dioxus_core::{BorrowedAttributeValue, ElementId, MultiWriter, RenderTargetId, WriteMutations};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::mem;
use std::rc::Rc;
use std::str::FromStr as _;

pub(crate) trait RealDom {
    type NodeId: Copy;

    fn create_element(&mut self, tag: &str, ns: Option<&str>) -> Self::NodeId;

    fn create_text(&mut self, value: &str) -> Self::NodeId;

    fn deep_clone(&mut self, node: Self::NodeId) -> Self::NodeId;

    fn nth_child(&mut self, parent: Self::NodeId, index: usize) -> Self::NodeId;

    fn append_children(&mut self, parent: Self::NodeId, children: &[Self::NodeId]);

    fn insert_after(&mut self, anchor: Self::NodeId, nodes: &[Self::NodeId]);

    fn insert_before(&mut self, anchor: Self::NodeId, nodes: &[Self::NodeId]);

    fn replace(&mut self, target: Self::NodeId, replacements: &[Self::NodeId]);

    fn remove(&mut self, node: Self::NodeId);

    fn set_attribute(
        &mut self,
        node: Self::NodeId,
        name: &str,
        ns: Option<&str>,
        value: BorrowedAttributeValue<'_>,
    );

    fn set_text(&mut self, node: Self::NodeId, value: &str);

    fn add_event_listener(&mut self, node: Self::NodeId, element_id: ElementId, name: &str);

    fn remove_event_listener(&mut self, node: Self::NodeId, element_id: ElementId, name: &str);
}

#[derive(Clone, Copy, Debug)]
struct StackEntry<N> {
    node: N,
    element_id: Option<ElementId>,
}

#[derive(Debug)]
struct StackState<N> {
    /// The node the stack starts from, mapped to `ElementId::ROOT`
    root: N,
    stack: Vec<StackEntry<N>>,
    element_to_node: Vec<Option<N>>,
}

impl<N: Copy> StackState<N> {
    fn new(root: N) -> Self {
        Self {
            root,
            stack: vec![StackEntry {
                node: root,
                element_id: Some(ElementId::ROOT),
            }],
            element_to_node: vec![Some(root)],
        }
    }

    /// Moves the stack's root, and `ElementId::ROOT`, to `root`. Only valid between flushes,
    /// when the root is the only entry left on the stack.
    fn set_root(&mut self, root: N) {
        debug_assert_eq!(self.stack.len(), 1, "the root moved mid-flush");
        self.root = root;
        self.stack[0].node = root;
        self.element_to_node[ElementId::ROOT.raw()] = Some(root);
    }

    fn element_to_node(&self, id: ElementId) -> Option<N> {
        self.element_to_node.get(id.raw()).copied().flatten()
    }

    fn lookup(&self, id: ElementId) -> N {
        self.element_to_node(id)
            .unwrap_or_else(|| panic!("renderer asked for unknown ElementId {}", id.raw()))
    }

    fn set_mapping(&mut self, id: ElementId, node: N) {
        let index = id.raw();
        if self.element_to_node.len() <= index {
            self.element_to_node.resize(index + 1, None);
        }
        self.element_to_node[index] = Some(node);
    }

    fn clear_mapping(&mut self, entry: StackEntry<N>) {
        if let Some(id) = entry.element_id
            && let Some(slot) = self.element_to_node.get_mut(id.raw())
        {
            *slot = None;
        }
    }

    fn push(&mut self, node: N, element_id: Option<ElementId>) {
        self.stack.push(StackEntry { node, element_id });
    }

    fn pop_entry(&mut self) -> StackEntry<N> {
        self.stack.pop().expect("renderer stack unexpectedly empty")
    }

    fn top(&self) -> StackEntry<N> {
        *self
            .stack
            .last()
            .expect("renderer stack unexpectedly empty")
    }

    fn replace_top(&mut self, node: N) {
        *self
            .stack
            .last_mut()
            .expect("renderer stack unexpectedly empty") = StackEntry {
            node,
            element_id: None,
        };
    }

    fn pop_nodes(&mut self, m: usize) -> Vec<N> {
        let split = self.stack.len() - m;
        self.stack
            .split_off(split)
            .into_iter()
            .map(|entry| entry.node)
            .collect()
    }
}

pub(crate) struct StackWriter<'a, R: RealDom> {
    state: &'a mut StackState<R::NodeId>,
    backend: R,
}

impl<'a, R: RealDom> StackWriter<'a, R> {
    fn new(state: &'a mut StackState<R::NodeId>, backend: R) -> Self {
        Self { state, backend }
    }
}

impl<R: RealDom> WriteMutations for StackWriter<'_, R> {
    fn push_id(&mut self, id: ElementId) {
        let node = self.state.lookup(id);
        self.state.push(node, Some(id));
    }

    fn set_id(&mut self, id: ElementId) {
        let node = self.state.top().node;
        self.state.set_mapping(id, node);
    }

    fn child(&mut self, index: usize) {
        let parent = self.state.top().node;
        let child = self.backend.nth_child(parent, index);
        self.state.replace_top(child);
    }

    fn pop(&mut self) {
        self.state.pop_entry();
    }

    fn create_element(&mut self, tag: &str, ns: Option<&str>) {
        let node = self.backend.create_element(tag, ns);
        self.state.push(node, None);
    }

    fn create_text(&mut self, value: &str) {
        let node = self.backend.create_text(value);
        self.state.push(node, None);
    }

    fn clone(&mut self) {
        let node = self.state.top().node;
        let cloned = self.backend.deep_clone(node);
        self.state.replace_top(cloned);
    }

    fn append_children(&mut self, m: usize) {
        let children = self.state.pop_nodes(m);
        let parent = self.state.top().node;
        self.backend.append_children(parent, &children);
    }

    fn replace_with(&mut self, m: usize) {
        let replacements = self.state.pop_nodes(m);
        let target = self.state.pop_entry();
        self.backend.replace(target.node, &replacements);
        self.state.clear_mapping(target);
    }

    fn insert_after(&mut self, m: usize) {
        let nodes = self.state.pop_nodes(m);
        let anchor = self.state.top().node;
        self.backend.insert_after(anchor, &nodes);
    }

    fn insert_before(&mut self, m: usize) {
        let nodes = self.state.pop_nodes(m);
        let anchor = self.state.top().node;
        self.backend.insert_before(anchor, &nodes);
    }

    fn set_attribute(&mut self, name: &str, ns: Option<&str>, value: BorrowedAttributeValue<'_>) {
        let node = self.state.top().node;
        self.backend.set_attribute(node, name, ns, value);
    }

    fn set_text(&mut self, value: &str) {
        let node = self.state.top().node;
        self.backend.set_text(node, value);
    }

    fn add_event_listener(&mut self, name: &str) {
        let entry = self.state.top();
        let element_id = entry
            .element_id
            .expect("event listener target must be mapped to an ElementId");
        self.backend
            .add_event_listener(entry.node, element_id, name);
    }

    fn remove_event_listener(&mut self, name: &str) {
        let entry = self.state.top();
        let element_id = entry
            .element_id
            .expect("event listener target must be mapped to an ElementId");
        self.backend
            .remove_event_listener(entry.node, element_id, name);
    }

    fn remove(&mut self) {
        let entry = self.state.pop_entry();
        self.backend.remove(entry.node);
        self.state.clear_mapping(entry);
    }
}

/// The overlay and removal requests that `dioxus-native`'s portal hooks queue for the document.
///
/// [`DioxusDocument`](crate::DioxusDocument) provides one as a root context and acts on it
/// after each render.
#[derive(Clone, Debug, Default)]
pub struct PortalHosts(Rc<RefCell<PortalRequests>>);

#[derive(Debug, Default)]
struct PortalRequests {
    /// Targets that want an overlay host but don't have one yet
    overlays: Vec<RenderTargetId>,
    /// Targets to register as outlets, with their keys
    outlets: Vec<(String, RenderTargetId)>,
    /// Targets whose owner dropped, to be removed once nothing is mounted in them
    removals: Vec<RenderTargetId>,
}

impl PortalHosts {
    /// Asks the document to host `target` in an overlay appended to `<body>`.
    pub fn request_overlay(&self, target: RenderTargetId) {
        self.0.borrow_mut().overlays.push(target);
    }

    /// Asks the document to host `target` in the element with `data-portal-outlet="<key>"`.
    pub fn request_outlet(&self, key: &str, target: RenderTargetId) {
        self.0.borrow_mut().outlets.push((key.to_owned(), target));
    }

    /// Asks the document to remove `target` and its host once nothing is mounted in it.
    pub fn request_removal(&self, target: RenderTargetId) {
        self.0.borrow_mut().removals.push(target);
    }

    fn wants_host(&self, target: RenderTargetId) -> bool {
        let requests = self.0.borrow();
        requests.overlays.contains(&target)
            || requests.outlets.iter().any(|(_, outlet)| *outlet == target)
    }

    pub(crate) fn take_overlays(&self) -> Vec<RenderTargetId> {
        mem::take(&mut self.0.borrow_mut().overlays)
    }

    pub(crate) fn take_outlets(&self) -> Vec<(String, RenderTargetId)> {
        mem::take(&mut self.0.borrow_mut().outlets)
    }

    pub(crate) fn take_removals(&self) -> Vec<RenderTargetId> {
        mem::take(&mut self.0.borrow_mut().removals)
    }
}

/// The mutation state of one render target.
#[derive(Debug, Default)]
struct TargetState {
    /// Stack machine state, rooted at the target's host; `None` while the target waits for one
    stack: Option<StackState<NodeId>>,
    /// Mutations written by the VirtualDom which have not yet been applied to the document
    buffered: MutationBuffer,
    /// Mounted events queued as elements are mounted
    queued_mounted: Vec<ElementId>,
    /// The key of the outlet that hosts the target, if an outlet does
    outlet: Option<String>,
}

impl TargetState {
    fn hosted(host: NodeId) -> Self {
        Self {
            stack: Some(StackState::new(host)),
            ..Self::default()
        }
    }
}

/// The state of the Dioxus integration with the RealDom
#[derive(Debug)]
pub struct DioxusState {
    /// Each render target's state, ROOT included, replayed in id order
    targets: BTreeMap<RenderTargetId, TargetState>,
    /// Requests from the portal hooks
    portal_hosts: PortalHosts,
    /// Targets whose writes were dropped since they had no state, each warned about once
    dropped_targets: BTreeSet<RenderTargetId>,
    /// The outlet registry: each key's render target
    outlets: BTreeMap<String, RenderTargetId>,
    /// The current host of each outlet target that has one, kept alive when its element goes
    outlet_hosts: Vec<NodeId>,
    /// Whether a mutation may have added, moved or removed an outlet element
    outlets_dirty: bool,
    /// Count of each handler type (indexed by `DomEventKind` discriminant)
    pub(crate) event_handler_counts: [u32; 64],
}

impl DioxusState {
    /// Initialize the DioxusState in the RealDom
    pub fn create(root_id: NodeId) -> Self {
        Self {
            targets: BTreeMap::from([(RenderTargetId::ROOT, TargetState::hosted(root_id))]),
            portal_hosts: PortalHosts::default(),
            dropped_targets: BTreeSet::new(),
            outlets: BTreeMap::new(),
            outlet_hosts: Vec::new(),
            outlets_dirty: false,
            event_handler_counts: [0; 64],
        }
    }

    /// Convert an ElementId to a NodeId
    pub fn element_to_node_id(&self, element_id: ElementId) -> NodeId {
        self.try_element_to_node_id(element_id).unwrap()
    }

    /// Attempt to convert an ElementId to a NodeId. This will return None if the ElementId is not in the RealDom.
    pub fn try_element_to_node_id(&self, element_id: ElementId) -> Option<NodeId> {
        self.try_target_element_to_node_id(RenderTargetId::ROOT, element_id)
    }

    pub(crate) fn try_target_element_to_node_id(
        &self,
        target: RenderTargetId,
        element_id: ElementId,
    ) -> Option<NodeId> {
        self.targets
            .get(&target)?
            .stack
            .as_ref()?
            .element_to_node(element_id)
    }

    pub(crate) fn portal_hosts(&self) -> &PortalHosts {
        &self.portal_hosts
    }

    /// The host of `target`, if it has one.
    pub(crate) fn host(&self, target: RenderTargetId) -> Option<NodeId> {
        Some(self.targets.get(&target)?.stack.as_ref()?.root)
    }

    /// Roots `target` at `host`, keeping any writes buffered while it waited for one.
    pub(crate) fn set_host(&mut self, target: RenderTargetId, host: NodeId) {
        self.dropped_targets.remove(&target);
        self.targets.entry(target).or_default().stack = Some(StackState::new(host));
    }

    /// Forgets `target`, so its later writes are dropped, and returns the host to drop with it.
    ///
    /// An outlet's element belongs to the app, so it is returned only once it has left the
    /// document.
    pub(crate) fn remove_target(
        &mut self,
        doc: &BaseDocument,
        target: RenderTargetId,
    ) -> Option<NodeId> {
        let state = self.targets.remove(&target)?;
        let host = state.stack?.root;
        let Some(key) = state.outlet else {
            return Some(host);
        };
        if self.outlets.get(&key) == Some(&target) {
            self.outlets.remove(&key);
        }
        self.outlet_hosts.retain(|outlet| *outlet != host);
        doc.get_node(host)
            .is_some_and(|node| node.parent.is_none())
            .then_some(host)
    }

    /// Registers `target` as the outlet for `key`, hosted by the element with
    /// `data-portal-outlet="<key>"` once one exists.
    ///
    /// A key that is already registered keeps its target, and `target`'s writes are dropped.
    pub(crate) fn register_outlet(&mut self, key: String, target: RenderTargetId) {
        if let Some(existing) = self.outlets.get(&key)
            && *existing != target
        {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                name: "dioxus_native.portal.duplicate_outlet",
                key = %key,
                render_target = ?target,
                "the outlet key already has a render target, so this one's writes are dropped"
            );
            self.targets.remove(&target);
            self.dropped_targets.insert(target);
            return;
        }
        self.targets.entry(target).or_default().outlet = Some(key.clone());
        self.outlets.insert(key, target);
        self.outlets_dirty = true;
    }

    /// Hosts each outlet target in the first element, in tree order, whose
    /// `data-portal-outlet` names its key, moving its content when that element changes.
    ///
    /// A target whose element has left the document keeps its old host, detached, until another
    /// one appears. Returns whether a target got its first host, which leaves buffered writes
    /// to apply.
    fn bind_outlets(&mut self, doc: &mut BaseDocument) -> bool {
        if self.outlets.is_empty() || !mem::take(&mut self.outlets_dirty) {
            return false;
        }
        let elements = outlet_elements(doc);
        let mut first_hosts = false;
        for (key, target) in &self.outlets {
            let Some(state) = self.targets.get_mut(target) else {
                continue;
            };
            let element = elements.get(key.as_str()).copied();
            match (state.stack.as_mut(), element) {
                (None, Some(element)) => {
                    state.stack = Some(StackState::new(element));
                    first_hosts = true;
                }
                (Some(stack), Some(element)) if stack.root != element => {
                    let old = stack.root;
                    let mut docm = doc.mutate();
                    docm.reparent_children(old, element);
                    stack.set_root(element);
                    if docm.parent_id(old).is_none() {
                        docm.remove_and_drop_node(old);
                    }
                }
                (Some(stack), None)
                    if doc.get_node(stack.root).is_some_and(|n| n.parent.is_some()) =>
                {
                    // The element is still in the document, but no longer names the key.
                    let mut docm = doc.mutate();
                    let parked = docm.create_element(qual_name("div", None), Vec::new());
                    docm.reparent_children(stack.root, parked);
                    stack.set_root(parked);
                }
                _ => {}
            }
        }
        self.outlet_hosts = self
            .outlets
            .values()
            .filter_map(|target| self.host(*target))
            .collect();
        first_hosts
    }

    /// The target `node` is content of: the one hosted by its nearest proper DOM ancestor, or
    /// ROOT if none is. A host itself is content of the target it was rendered in.
    pub(crate) fn target_of_node(&self, doc: &BaseDocument, node: NodeId) -> RenderTargetId {
        if self.targets.len() == 1 {
            return RenderTargetId::ROOT;
        }
        let mut current = doc.get_node(node).and_then(|node| node.parent);
        while let Some(node) = current {
            let hosted = self.targets.iter().find_map(|(target, state)| {
                (state.stack.as_ref()?.root == node).then_some(*target)
            });
            if let Some(target) = hosted {
                return target;
            }
            current = doc.get_node(node).and_then(|node| node.parent);
        }
        RenderTargetId::ROOT
    }

    /// Takes each target's queued mounted events, ROOT first.
    pub(crate) fn take_queued_mounted(&mut self) -> Vec<(RenderTargetId, ElementId)> {
        let mut queued = Vec::new();
        for (target, state) in &mut self.targets {
            queued.extend(state.queued_mounted.drain(..).map(|id| (*target, id)));
        }
        queued
    }

    /// Apply the buffered dioxus mutations to `doc`, ROOT first and then each other target in id
    /// order, driving the blitz backend from each target's stack machine.
    ///
    /// Outlets are bound after each pass, and a pass runs again when one got its first host, so
    /// the writes it buffered while waiting are applied too.
    pub(crate) fn apply_buffered_mutations(&mut self, doc: &mut BaseDocument) {
        loop {
            self.apply_hosted_targets(doc);
            if !self.bind_outlets(doc) {
                break;
            }
        }
    }

    /// Replays the buffered writes of every target that has a host.
    fn apply_hosted_targets(&mut self, doc: &mut BaseDocument) {
        let Self {
            targets,
            outlet_hosts,
            outlets_dirty,
            event_handler_counts,
            ..
        } = self;
        for state in targets.values_mut() {
            let TargetState {
                stack: Some(stack),
                buffered,
                queued_mounted,
                ..
            } = state
            else {
                continue;
            };
            let mut writer: MutationWriter<'_> = StackWriter::new(
                stack,
                BlitzBackend {
                    docm: doc.mutate(),
                    event_handler_counts,
                    queued_mounted_events: queued_mounted,
                    outlet_hosts,
                    outlets_dirty,
                },
            );
            buffered.replay(&mut writer);
        }
    }
}

/// The first element in tree order for each `data-portal-outlet` key in `doc`.
fn outlet_elements(doc: &BaseDocument) -> BTreeMap<String, NodeId> {
    let name = qual_name("data-portal-outlet", None);
    let mut elements = BTreeMap::new();
    let mut pending = vec![doc.root_node().id];
    while let Some(id) = pending.pop() {
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        if let Some(key) = node.attr(name.local.clone()) {
            elements.entry(key.to_owned()).or_insert(id);
        }
        // Pushed in reverse, so children are visited in tree order.
        pending.extend(node.children.iter().rev().copied());
    }
    elements
}

impl MultiWriter for DioxusState {
    type Writer = MutationBuffer;

    /// The buffer for `id`. A target nothing hosts or asked to host gets `None`, so its writes
    /// are dropped.
    fn writer_for(&mut self, id: RenderTargetId) -> Option<&mut MutationBuffer> {
        if !self.targets.contains_key(&id) {
            if !self.portal_hosts.wants_host(id) {
                if self.dropped_targets.insert(id) {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        name: "dioxus_native.portal.unknown_target",
                        render_target = ?id,
                        "no host is registered for the render target, so its writes are dropped"
                    );
                }
                return None;
            }
            self.targets.insert(id, TargetState::default());
        }
        self.targets.get_mut(&id).map(|state| &mut state.buffered)
    }
}

/// A writer for mutations that can be used with the RealDom.
///
/// The stack-machine bookkeeping lives in [`StackWriter`]; this renderer only
/// supplies the real tree semantics via [`BlitzBackend`].
pub(crate) type MutationWriter<'a> = StackWriter<'a, BlitzBackend<'a>>;

/// The blitz-backed "real semantics" for the dioxus stack machine.
pub(crate) struct BlitzBackend<'a> {
    /// The realdom mutation handle associated with this writer
    docm: DocumentMutator<'a>,
    /// Count of each handler type, kept so event dispatch can skip unused kinds
    event_handler_counts: &'a mut [u32; 64],
    /// Mounted events queued as elements are mounted
    queued_mounted_events: &'a mut Vec<ElementId>,
    /// The outlet hosts, which a removal detaches instead of dropping
    outlet_hosts: &'a [NodeId],
    /// Set when a mutation may have added, moved or removed an outlet element
    outlets_dirty: &'a mut bool,
}

impl BlitzBackend<'_> {
    /// Detaches each outlet host in `node`'s subtree that no other outlet host contains, so the
    /// content of its target outlives the element until another outlet takes it. Returns
    /// whether `node` is such a host itself, and so must not be dropped.
    fn keep_outlet_hosts(&mut self, node: NodeId) -> bool {
        for &host in self.outlet_hosts {
            let mut ancestor = Some(host);
            while let Some(current) = ancestor {
                if current == node {
                    self.docm.remove_node(host);
                    *self.outlets_dirty = true;
                    break;
                }
                ancestor = self.docm.parent_id(current);
                if ancestor.is_some_and(|parent| self.outlet_hosts.contains(&parent)) {
                    break;
                }
            }
        }
        self.outlet_hosts.contains(&node)
    }
}

impl RealDom for BlitzBackend<'_> {
    type NodeId = NodeId;

    fn create_element(&mut self, tag: &str, ns: Option<&str>) -> NodeId {
        trace!("create_element tag:{tag} ns:{ns:?}");
        self.docm.create_element(qual_name(tag, ns), Vec::new())
    }

    fn create_text(&mut self, value: &str) -> NodeId {
        trace!("create_text text:{value}");
        self.docm.create_text_node(value)
    }

    fn deep_clone(&mut self, node: NodeId) -> NodeId {
        trace!("clone node:{node}");
        // A clone of a template can carry a static `data-portal-outlet`.
        *self.outlets_dirty = true;
        self.docm.deep_clone_node(node)
    }

    fn nth_child(&mut self, parent: NodeId, index: usize) -> NodeId {
        trace!("nth_child parent:{parent} index:{index}");
        self.docm.child_ids(parent)[index]
    }

    fn append_children(&mut self, parent: NodeId, children: &[NodeId]) {
        trace!("append_children parent:{parent} children:{children:?}");
        self.docm.append_children(parent, children);
    }

    fn insert_after(&mut self, anchor: NodeId, nodes: &[NodeId]) {
        trace!("insert_after anchor:{anchor} nodes:{nodes:?}");
        self.docm.insert_nodes_after(anchor, nodes);
    }

    fn insert_before(&mut self, anchor: NodeId, nodes: &[NodeId]) {
        trace!("insert_before anchor:{anchor} nodes:{nodes:?}");
        self.docm.insert_nodes_before(anchor, nodes);
    }

    fn replace(&mut self, target: NodeId, replacements: &[NodeId]) {
        trace!("replace target:{target} replacements:{replacements:?}");
        self.docm.insert_nodes_before(target, replacements);
        if !self.keep_outlet_hosts(target) {
            self.docm.remove_and_drop_node(target);
        }
    }

    fn remove(&mut self, node: NodeId) {
        trace!("remove node:{node}");
        if !self.keep_outlet_hosts(node) {
            self.docm.remove_and_drop_node(node);
        }
    }

    fn set_attribute(
        &mut self,
        node: NodeId,
        name: &str,
        ns: Option<&str>,
        value: BorrowedAttributeValue<'_>,
    ) {
        self.set_attribute_impl(node, name, ns, value);
    }

    fn set_text(&mut self, node: NodeId, value: &str) {
        trace!("set_text node:{node} value:{value}");
        self.docm.set_node_text(node, value);
    }

    fn add_event_listener(&mut self, node: NodeId, element_id: ElementId, name: &str) {
        // Mounted events are fired immediately after the element is mounted.
        if name == "mounted" {
            self.queued_mounted_events.push(element_id);
            return;
        }

        // We're going to actually set the listener here as a placeholder - in JS this would also be a placeholder
        // we might actually just want to attach the attribute to the root element (delegation)
        let value = BorrowedAttributeValue::Text("<rust func>");
        self.set_attribute_impl(node, name, None, value);

        // Also set the data-dioxus-id attribute so we can find the element later
        let value = element_id.raw().to_string();
        let value = BorrowedAttributeValue::Text(&value);
        self.set_attribute_impl(node, "data-dioxus-id", None, value);

        if let Ok(kind) = DomEventKind::from_str(name) {
            let idx = kind.discriminant() as usize;
            self.event_handler_counts[idx] += 1;
        }
    }

    fn remove_event_listener(&mut self, _node: NodeId, _element_id: ElementId, name: &str) {
        if let Ok(kind) = DomEventKind::from_str(name) {
            let idx = kind.discriminant() as usize;
            self.event_handler_counts[idx] -= 1;
        }
    }
}

impl BlitzBackend<'_> {
    fn set_attribute_impl(
        &mut self,
        node_id: NodeId,
        local_name: &str,
        ns: Option<&str>,
        value: BorrowedAttributeValue<'_>,
    ) {
        fn is_falsy(val: BorrowedAttributeValue<'_>) -> bool {
            match val {
                BorrowedAttributeValue::None => true,
                BorrowedAttributeValue::Text(val) => val == "false",
                BorrowedAttributeValue::Bool(val) => !val,
                BorrowedAttributeValue::Int(val) => val == 0,
                BorrowedAttributeValue::Float(val) => val == 0.0,
                BorrowedAttributeValue::Any(_) => false,
            }
        }

        if local_name == "data-portal-outlet" {
            *self.outlets_dirty = true;
        }

        // Set/unset subdocument for <web-view __webview_document>
        if local_name == "__webview_document" {
            match value {
                BorrowedAttributeValue::Any(value) => {
                    if let Some(value) = value
                        .as_any()
                        .downcast_ref::<WriteOnceAttr<Box<dyn Document>>>()
                        && let Some(mut sub_document) = value.take()
                    {
                        sub_document
                            .inner_mut()
                            .set_shell_provider(self.docm.doc.shell_provider.clone());
                        self.docm.set_sub_document(node_id, sub_document);
                    }
                }
                _ => self.docm.remove_sub_document(node_id),
            }
        }

        // Set/unset custom widget for <object data>
        if local_name == "data" {
            let element_name = self.docm.element_name(node_id).unwrap();
            if element_name.local.as_str() == "object" {
                match value {
                    BorrowedAttributeValue::Any(value) => {
                        if let Some(value) = value
                            .as_any()
                            .downcast_ref::<WriteOnceAttr<Box<dyn Widget>>>()
                            && let Some(widget) = value.take()
                        {
                            self.docm.set_custom_widget(node_id, widget);
                        }
                    }
                    _ => self.docm.remove_custom_widget(node_id),
                }
            }
        }

        let falsy = is_falsy(value);
        match value {
            BorrowedAttributeValue::None => {
                set_attribute_inner(&mut self.docm, local_name, ns, None, falsy, node_id)
            }
            BorrowedAttributeValue::Text(value) => {
                set_attribute_inner(&mut self.docm, local_name, ns, Some(value), falsy, node_id)
            }
            BorrowedAttributeValue::Float(value) => {
                let value = value.to_string();
                set_attribute_inner(&mut self.docm, local_name, ns, Some(&value), falsy, node_id);
            }
            BorrowedAttributeValue::Int(value) => {
                let value = value.to_string();
                set_attribute_inner(&mut self.docm, local_name, ns, Some(&value), falsy, node_id);
            }
            BorrowedAttributeValue::Bool(value) => {
                let value = value.to_string();
                set_attribute_inner(&mut self.docm, local_name, ns, Some(&value), falsy, node_id);
            }
            BorrowedAttributeValue::Any(_) => {
                // FIXME: support all attribute types
            }
        };
    }
}

fn set_attribute_inner(
    docm: &mut DocumentMutator<'_>,
    local_name: &str,
    ns: Option<&str>,
    value: Option<&str>,
    is_falsy: bool,
    node_id: NodeId,
) {
    trace!("set_attribute node_id:{node_id} ns: {ns:?} name:{local_name}, value:{value:?}");

    // Dioxus has overloaded the style namespace to accumulate style attributes without a `style` block
    // TODO: accumulate style attributes into a single style element.
    if ns == Some("style") {
        match value {
            Some(value) => docm.set_style_property(node_id, local_name, value),
            None => docm.remove_style_property(node_id, local_name),
        }
        return;
    }

    let name = qual_name(local_name, Some(ns.unwrap_or("")));

    // FIXME: more principled handling of special case attributes
    match value {
        None => docm.clear_attribute(node_id, name),
        Some(value) => {
            if is_falsy && is_bool_attr(local_name) {
                docm.clear_attribute(node_id, name);
            } else if local_name == "dangerous_inner_html" {
                docm.set_inner_html(node_id, value);
            } else {
                docm.set_attribute(node_id, name, value);
            }
        }
    }
}

/// HTML boolean attributes are "on" whenever they are present, regardless of their value, so a
/// falsy value (e.g. `disabled: false`) must remove the attribute rather than write `"false"`.
///
/// Mirrors `BOOL_ATTRS` in `dioxus-ssr` and `isBoolAttr` in the web interpreter.
fn is_bool_attr(name: &str) -> bool {
    matches!(
        name,
        "allowfullscreen"
            | "allowpaymentrequest"
            | "async"
            | "autofocus"
            | "autoplay"
            | "checked"
            | "controls"
            | "default"
            | "defer"
            | "disabled"
            | "formnovalidate"
            | "hidden"
            | "inert"
            | "ismap"
            | "itemscope"
            | "loop"
            | "multiple"
            | "muted"
            | "nomodule"
            | "novalidate"
            | "open"
            | "playsinline"
            | "readonly"
            | "required"
            | "reversed"
            | "selected"
            | "truespeed"
            | "webkitdirectory"
    )
}

#[cfg(test)]
mod tests {
    use crate::{DioxusDocument, DocumentConfig};
    use blitz_dom::Document as _;
    use blitz_dom::LocalName;
    use dioxus::prelude::*;
    use dioxus_core::ScopeId;
    use std::{cell::Cell, rc::Rc};

    fn build(app: fn() -> Element) -> DioxusDocument {
        let mut doc = DioxusDocument::new(VirtualDom::new(app), DocumentConfig::default());
        doc.initial_build();
        doc
    }

    fn attr(doc: &DioxusDocument, selector: &str, name: &str) -> Option<String> {
        let inner = doc.inner.borrow();
        let node_id = inner
            .query_selector(selector)
            .unwrap()
            .unwrap_or_else(|| panic!("no node matches {selector}"));
        inner
            .get_node(node_id)
            .unwrap()
            .attr(LocalName::from(name))
            .map(str::to_owned)
    }

    #[test]
    fn falsy_boolean_attributes_are_removed() {
        fn app() -> Element {
            let off = false;
            let on = true;
            rsx! {
                input {
                    id: "off",
                    r#type: "checkbox",
                    disabled: off,
                    readonly: off,
                    required: off,
                    checked: off,
                    title: "false",
                }
                input { id: "literal", r#type: "checkbox", disabled: false }
                input { id: "on", r#type: "checkbox", disabled: on, required: on }
                details { id: "details", open: off }
                select {
                    id: "select",
                    multiple: off,
                    option { id: "option", selected: off }
                }
            }
        }

        let doc = build(app);

        for name in ["disabled", "readonly", "required", "checked"] {
            assert_eq!(
                attr(&doc, "#off", name),
                None,
                "`{name}: false` should be absent"
            );
        }
        assert_eq!(attr(&doc, "#literal", "disabled"), None);
        assert_eq!(attr(&doc, "#details", "open"), None);
        assert_eq!(attr(&doc, "#select", "multiple"), None);
        assert_eq!(attr(&doc, "#option", "selected"), None);

        // Truthy boolean attributes are still present.
        assert_eq!(attr(&doc, "#on", "disabled").as_deref(), Some("true"));
        assert_eq!(attr(&doc, "#on", "required").as_deref(), Some("true"));

        // Non-boolean attributes keep their falsy-looking value.
        assert_eq!(attr(&doc, "#off", "title").as_deref(), Some("false"));
    }

    #[test]
    fn boolean_attributes_follow_their_value_after_mount() {
        fn app() -> Element {
            let on = consume_context::<Rc<Cell<bool>>>().get();
            rsx! {
                input { id: "input", r#type: "checkbox", disabled: on, checked: on }
            }
        }

        let on: Rc<Cell<bool>> = Rc::new(Cell::new(true));
        let vdom = VirtualDom::new(app).with_root_context(Rc::clone(&on));
        let mut doc = DioxusDocument::new(vdom, DocumentConfig::default());
        doc.initial_build();
        doc.inner.borrow_mut().resolve(0.0);

        for value in [true, false, true, false] {
            on.set(value);
            doc.vdom.mark_dirty(ScopeId::APP);
            doc.poll(None);

            let expected = value.then_some("true");
            assert_eq!(attr(&doc, "#input", "disabled").as_deref(), expected);
            assert_eq!(attr(&doc, "#input", "checked").as_deref(), expected);
        }
    }
}
