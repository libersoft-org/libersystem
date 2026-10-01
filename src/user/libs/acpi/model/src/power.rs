//! DEVICE POWER STATES AND THE POWER RESOURCES THEY SHARE - what the ACPI service does when a device's driver asks,
//! through its node channel, for a device power state, and what state a system sleep lets a device enter.
//!
//! A DEVICE IN Dx needs every power resource its `_PRx` names ON - `_PR0` for D0, `_PR1`, `_PR2`, `_PR3` for D1, D2 and
//! D3hot; D3cold holds none. A POWER RESOURCE IS SHARED: it is ON while any holder holds it and OFF only when the last
//! one lets it go, which is why the count is the service's and never a driver's. A holder is a device in a state, or a
//! wake node whose `_PRW` names resources the sleep in progress keeps on. A transition takes the new state's resources
//! FIRST - in ascending `resource_order`, the order the specification turns resources on in - and lets go of the old
//! state's afterwards, in descending order, each `_OFF` only where its count reached zero. `_PSx` runs between them: a
//! device is never asked into a state whose power is not there yet, and its power is not taken while it is still in the
//! state that used it.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A device power state. D3cold is D3 with no resource held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DState {
	D0,
	D1,
	D2,
	D3Hot,
	D3Cold,
}

impl DState {
	/// The node channel's encoding: 0 to 3 are D0 to D3hot, 4 is D3cold - as `_SxW` numbers them.
	pub fn from_u8(value: u8) -> Option<DState> {
		match value {
			0 => Some(DState::D0),
			1 => Some(DState::D1),
			2 => Some(DState::D2),
			3 => Some(DState::D3Hot),
			4 => Some(DState::D3Cold),
			_ => None,
		}
	}

	pub fn as_u8(self) -> u8 {
		self as u8
	}

	/// The `_PSx` method that enters it: D3cold is entered with `_PS3`, and its resources are then let go.
	pub fn method(self) -> &'static [u8; 4] {
		match self {
			DState::D0 => b"_PS0",
			DState::D1 => b"_PS1",
			DState::D2 => b"_PS2",
			DState::D3Hot | DState::D3Cold => b"_PS3",
		}
	}

	/// The `_PRx` package that names its resources, none for D3cold.
	pub fn resources(self) -> Option<&'static [u8; 4]> {
		match self {
			DState::D0 => Some(b"_PR0"),
			DState::D1 => Some(b"_PR1"),
			DState::D2 => Some(b"_PR2"),
			DState::D3Hot => Some(b"_PR3"),
			DState::D3Cold => None,
		}
	}

	pub fn name(self) -> &'static str {
		match self {
			DState::D0 => "D0",
			DState::D1 => "D1",
			DState::D2 => "D2",
			DState::D3Hot => "D3hot",
			DState::D3Cold => "D3cold",
		}
	}
}

/// One power resource as a `_PRx` or `_PRW` package names it: its absolute path and its `resource_order`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
	pub path: String,
	pub order: u16,
}

/// What a transition asks of the firmware, in this order: each resource to turn ON, the `_PSx` (none when the device is
/// already in the state), each resource to turn OFF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
	pub on: Vec<String>,
	pub method: Option<&'static [u8; 4]>,
	pub off: Vec<String>,
}

/// EVERY HOLDER'S RESOURCES, EVERY RESOURCE'S COUNT AND EVERY DEVICE'S STATE.
#[derive(Clone, Default, Debug)]
pub struct Resources {
	counts: BTreeMap<String, u32>,
	held: BTreeMap<String, Vec<Resource>>,
	states: BTreeMap<String, DState>,
}

impl Resources {
	pub fn new() -> Resources {
		Resources::default()
	}

	/// How many holders hold `path`.
	pub fn count(&self, path: &str) -> u32 {
		self.counts.get(path).copied().unwrap_or(0)
	}

	/// The state `device` was last put in, none before its first.
	pub fn state(&self, device: &str) -> Option<DState> {
		self.states.get(device).copied()
	}

	/// `holder` HOLDS EXACTLY `wanted` from now on: what to turn ON - each resource whose count left zero, in ascending
	/// order - and what to turn OFF - each whose count reached it, in descending order. A resource it already held stays
	/// held and is switched neither way; one named twice is held once.
	pub fn hold(&mut self, holder: &str, wanted: &[Resource]) -> (Vec<String>, Vec<String>) {
		let before: Vec<Resource> = self.held.remove(holder).unwrap_or_default();
		let mut kept: Vec<Resource> = Vec::new();
		for resource in wanted {
			if !kept.iter().any(|already| already.path == resource.path) {
				kept.push(resource.clone());
			}
		}
		let mut on: Vec<Resource> = Vec::new();
		for resource in &kept {
			if before.iter().any(|held| held.path == resource.path) {
				continue;
			}
			let count = self.counts.entry(resource.path.clone()).or_insert(0);
			*count += 1;
			if *count == 1 {
				on.push(resource.clone());
			}
		}
		let mut off: Vec<Resource> = Vec::new();
		for resource in &before {
			if kept.iter().any(|still| still.path == resource.path) {
				continue;
			}
			if let Some(count) = self.counts.get_mut(&resource.path) {
				*count = count.saturating_sub(1);
				if *count == 0 {
					self.counts.remove(&resource.path);
					off.push(resource.clone());
				}
			}
		}
		on.sort_by_key(|resource| resource.order);
		off.sort_by_key(|resource| core::cmp::Reverse(resource.order));
		if !kept.is_empty() {
			self.held.insert(String::from(holder), kept);
		}
		(on.into_iter().map(|resource| resource.path).collect(), off.into_iter().map(|resource| resource.path).collect())
	}

	/// THE TRANSITION of `device` to `state`, whose resources are `wanted`: the counts moved and what to run. The state
	/// it is already in runs nothing.
	pub fn transition(&mut self, device: &str, state: DState, wanted: &[Resource]) -> Plan {
		if self.state(device) == Some(state) {
			return Plan { on: Vec::new(), method: None, off: Vec::new() };
		}
		let (on, off) = self.hold(device, wanted);
		self.states.insert(String::from(device), state);
		Plan { on, method: Some(state.method()), off }
	}

	/// A HOLDER GONE - a device's node channel closed, a wake node's sleep over: what it held let go, and what that turns
	/// off. Twice is nothing.
	pub fn forget(&mut self, holder: &str) -> Vec<String> {
		self.states.remove(holder);
		self.hold(holder, &[]).1
	}
}

/// THE STATE A DEVICE ENTERS FOR A SYSTEM SLEEP, from what its node says of that sleep - `_SxD`, the shallowest state it
/// may be in (there is none for suspend to idle), and `_SxW`, the deepest it can wake the machine from:
/// - to wake the machine, `_SxW` - never shallower than `_SxD` - or `_SxD` where there is no `_SxW`, or D0 where the
///   firmware says neither, since nothing then says a deeper state still wakes;
/// - otherwise D3cold, the deepest, which `_SxD` never forbids.
///
/// A value outside the states is firmware's mistake and is read as not there.
pub fn sleep_state(sxd: Option<u64>, sxw: Option<u64>, wake: bool) -> DState {
	let shallowest = sxd.and_then(|value| u8::try_from(value).ok()).filter(|value| *value <= 3).and_then(DState::from_u8);
	let deepest_wake = sxw.and_then(|value| u8::try_from(value).ok()).and_then(DState::from_u8);
	if !wake {
		return DState::D3Cold;
	}
	match (shallowest, deepest_wake) {
		(Some(shallowest), Some(deepest)) => deepest.max(shallowest),
		(None, Some(deepest)) => deepest,
		(Some(shallowest), None) => shallowest,
		(None, None) => DState::D0,
	}
}

/// The names `_SxD` and `_SxW` for a sleep `target` - 0 suspend to idle, 3 RAM, 4 disk: `_S0W` alone for suspend to
/// idle, and none for a target that is no sleep state.
pub fn sleep_objects(target: u8) -> Option<(Option<[u8; 4]>, [u8; 4])> {
	match target {
		0 => Some((None, *b"_S0W")),
		1..=4 => {
			let digit = b'0' + target;
			Some((Some([b'_', b'S', digit, b'D']), [b'_', b'S', digit, b'W']))
		}
		_ => None,
	}
}

#[cfg(test)]
mod tests;
