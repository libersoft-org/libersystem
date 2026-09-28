//! THE NAMESPACE: every named object the loaded tables declared, as a tree of nodes addressed by id, with the
//! object each holds. A method's own names are TEMPORARY - created under the method's node while it runs and gone
//! when it returns.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::name::{NameString, Path, Seg};
use crate::object::{ObjRef, Object, obj};

pub type NodeId = usize;

pub struct Node {
	pub seg: Seg,
	pub parent: Option<NodeId>,
	pub children: BTreeMap<Seg, NodeId>,
	pub object: ObjRef,
}

pub struct Namespace {
	nodes: Vec<Option<Node>>,
}

/// The root, always node 0.
pub const ROOT: NodeId = 0;

impl Default for Namespace {
	fn default() -> Self {
		Namespace::new()
	}
}

impl Namespace {
	pub fn new() -> Namespace {
		let root = Node { seg: Seg(*b"\\___"), parent: None, children: BTreeMap::new(), object: obj(Object::Scope) };
		Namespace { nodes: alloc::vec![Some(root)] }
	}

	pub fn node(&self, id: NodeId) -> Option<&Node> {
		self.nodes.get(id).and_then(|node| node.as_ref())
	}

	pub fn object(&self, id: NodeId) -> Option<ObjRef> {
		self.node(id).map(|node| node.object.clone())
	}

	pub fn parent(&self, id: NodeId) -> Option<NodeId> {
		self.node(id).and_then(|node| node.parent)
	}

	pub fn child(&self, id: NodeId, seg: Seg) -> Option<NodeId> {
		self.node(id).and_then(|node| node.children.get(&seg).copied())
	}

	/// The children of a node, in name order.
	pub fn children(&self, id: NodeId) -> Vec<NodeId> {
		self.node(id).map(|node| node.children.values().copied().collect()).unwrap_or_default()
	}

	/// How many nodes the namespace holds, the root included.
	pub fn len(&self) -> usize {
		self.nodes.iter().filter(|node| node.is_some()).count()
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	pub fn path(&self, id: NodeId) -> Path {
		let mut segs = Vec::new();
		let mut at = id;
		while let Some(node) = self.node(at) {
			let Some(parent) = node.parent else { break };
			segs.push(node.seg);
			at = parent;
		}
		segs.reverse();
		Path(segs)
	}

	/// The node an absolute path names.
	pub fn lookup_path(&self, path: &Path) -> Option<NodeId> {
		let mut at = ROOT;
		for seg in &path.0 {
			at = self.child(at, *seg)?;
		}
		Some(at)
	}

	/// The scope a name string's prefix leads to from `scope`, and the segments left to walk.
	fn anchor(&self, name: &NameString, scope: NodeId) -> Option<NodeId> {
		if name.root {
			return Some(ROOT);
		}
		let mut at = scope;
		for _ in 0..name.parents {
			at = self.parent(at)?;
		}
		Some(at)
	}

	/// RESOLVE a name string used from `scope`: a bare single segment searched in the scope and each enclosing one,
	/// anything else walked exactly.
	pub fn resolve(&self, name: &NameString, scope: NodeId) -> Option<NodeId> {
		if name.segs.is_empty() {
			return None;
		}
		if name.searches() {
			let mut at = Some(scope);
			while let Some(here) = at {
				if let Some(found) = self.child(here, name.segs[0]) {
					return Some(found);
				}
				at = self.parent(here);
			}
			return None;
		}
		let mut at = self.anchor(name, scope)?;
		for seg in &name.segs {
			at = self.child(at, *seg)?;
		}
		Some(at)
	}

	/// Where a declaration of `name` from `scope` goes: its parent node and last segment. The parent must exist.
	pub fn declaration_site(&self, name: &NameString, scope: NodeId) -> Option<(NodeId, Seg)> {
		let (last, init) = name.segs.split_last()?;
		let mut at = self.anchor(name, scope)?;
		for seg in init {
			at = self.child(at, *seg)?;
		}
		Some((at, *last))
	}

	/// ADD a node, or answer the one already there - ASL's `Scope` and a re-declared `External` meet an existing
	/// node. `None` when the parent is gone.
	pub fn add(&mut self, parent: NodeId, seg: Seg, object: ObjRef) -> Option<(NodeId, bool)> {
		if let Some(existing) = self.child(parent, seg) {
			return Some((existing, false));
		}
		self.node(parent)?;
		let id = self.nodes.len();
		self.nodes.push(Some(Node { seg, parent: Some(parent), children: BTreeMap::new(), object }));
		if let Some(Some(parent_node)) = self.nodes.get_mut(parent) {
			parent_node.children.insert(seg, id);
		}
		Some((id, true))
	}

	/// Replace the object a node holds - a declaration meeting an `External` placeholder, or `CopyObject`.
	pub fn set_object(&mut self, id: NodeId, object: ObjRef) {
		if let Some(Some(node)) = self.nodes.get_mut(id) {
			node.object = object;
		}
	}

	/// Remove a node and everything below it: a method's temporary names at its return.
	pub fn remove(&mut self, id: NodeId) {
		if id == ROOT {
			return;
		}
		let children = self.children(id);
		for child in children {
			self.remove(child);
		}
		let (parent, seg) = match self.node(id) {
			Some(node) => (node.parent, node.seg),
			None => return,
		};
		if let Some(parent) = parent
			&& let Some(Some(parent_node)) = self.nodes.get_mut(parent)
			&& parent_node.children.get(&seg) == Some(&id)
		{
			parent_node.children.remove(&seg);
		}
		self.nodes[id] = None;
	}

	/// Every node below `id`, depth first in name order, `id` excluded.
	pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
		let mut out = Vec::new();
		let mut stack: Vec<NodeId> = self.children(id).into_iter().rev().collect();
		while let Some(next) = stack.pop() {
			out.push(next);
			for child in self.children(next).into_iter().rev() {
				stack.push(child);
			}
		}
		out
	}
}
