//! Bounded state primitives shared by LiberCommander views and dialogs.

use crate::Key;

/// Maps one terminal key to an application action without allocating.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding<Action> {
	pub key: Key,
	pub action: Action,
}

impl<Action> Binding<Action> {
	pub const fn new(key: Key, action: Action) -> Binding<Action> {
		Binding { key, action }
	}
}

/// Resolve a key through a caller-owned bounded binding table.
pub fn dispatch_key<Action>(bindings: &[Binding<Action>], key: Key) -> Option<&Action> {
	bindings.iter().find(|binding| binding.key == key).map(|binding| &binding.action)
}

/// One active item in a finite focus ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Focus {
	count: u16,
	active: u16,
}

impl Focus {
	pub const fn new(count: u16) -> Focus {
		Focus { count, active: 0 }
	}

	pub const fn count(self) -> u16 {
		self.count
	}

	pub const fn active(self) -> Option<u16> {
		if self.count == 0 { None } else { Some(self.active) }
	}

	pub fn set_count(&mut self, count: u16) {
		self.count = count;
		if count == 0 {
			self.active = 0;
		} else if self.active >= count {
			self.active = count - 1;
		}
	}

	pub fn select(&mut self, index: u16) -> bool {
		if index >= self.count {
			return false;
		}
		self.active = index;
		true
	}

	pub fn next(&mut self) -> Option<u16> {
		if self.count == 0 {
			return None;
		}
		self.active = (self.active + 1) % self.count;
		Some(self.active)
	}

	pub fn previous(&mut self) -> Option<u16> {
		if self.count == 0 {
			return None;
		}
		self.active = if self.active == 0 { self.count - 1 } else { self.active - 1 };
		Some(self.active)
	}
}

/// State for one menu that can be opened and closed independently of rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuState {
	pub focus: Focus,
	open: bool,
}

impl MenuState {
	pub const fn new(items: u16) -> MenuState {
		MenuState { focus: Focus::new(items), open: false }
	}

	pub const fn is_open(self) -> bool {
		self.open
	}

	pub fn open(&mut self) {
		self.open = true;
	}

	pub fn close(&mut self) {
		self.open = false;
	}

	pub fn toggle(&mut self) -> bool {
		self.open = !self.open;
		self.open
	}
}

/// The modal category currently visible to the operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogKind {
	None,
	Confirm,
	Error,
	Progress,
}

/// Modal dialog state with a bounded button focus ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogState {
	kind: DialogKind,
	pub focus: Focus,
}

impl DialogState {
	pub const fn new() -> DialogState {
		DialogState { kind: DialogKind::None, focus: Focus::new(0) }
	}

	pub const fn kind(self) -> DialogKind {
		self.kind
	}

	pub fn show(&mut self, kind: DialogKind, buttons: u16) {
		self.kind = kind;
		self.focus = Focus::new(buttons);
	}

	pub fn close(&mut self) {
		self.kind = DialogKind::None;
		self.focus = Focus::new(0);
	}
}

impl Default for DialogState {
	fn default() -> Self {
		Self::new()
	}
}

/// A bounded operation lifecycle suitable for status and progress presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
	Idle,
	Running,
	Paused,
	Failed,
	Cancelled,
	Complete,
}

/// Counts a planned operation without retaining unbounded paths or error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
	pub state: OperationState,
	pub completed_items: u64,
	pub total_items: Option<u64>,
	pub completed_bytes: u64,
	pub total_bytes: Option<u64>,
}

impl Progress {
	pub const fn new(total_items: Option<u64>, total_bytes: Option<u64>) -> Progress {
		Progress { state: OperationState::Idle, completed_items: 0, total_items, completed_bytes: 0, total_bytes }
	}

	pub fn start(&mut self) {
		self.state = OperationState::Running;
	}

	pub fn advance(&mut self, items: u64, bytes: u64) {
		self.completed_items = self.completed_items.saturating_add(items);
		self.completed_bytes = self.completed_bytes.saturating_add(bytes);
	}

	pub fn pause(&mut self) {
		if self.state == OperationState::Running {
			self.state = OperationState::Paused;
		}
	}

	pub fn resume(&mut self) {
		if self.state == OperationState::Paused {
			self.state = OperationState::Running;
		}
	}

	pub fn fail(&mut self) {
		self.state = OperationState::Failed;
	}

	pub fn cancel(&mut self) {
		self.state = OperationState::Cancelled;
	}

	pub fn complete(&mut self) {
		self.state = OperationState::Complete;
	}

	/// Return the completed byte fraction in thousandths, or None when the total is unknown.
	pub fn byte_fraction_milli(self) -> Option<u16> {
		let total = self.total_bytes?;
		if total == 0 {
			return Some(1000);
		}
		Some(((self.completed_bytes.saturating_mul(1000) / total).min(1000)) as u16)
	}
}
