// hidcheck - the in-guest scenario driver for the HID-over-I2C gate. DEVELOPMENT-ONLY.
//
// It is a LIVE CLIENT of InputService on both of its streams - the pointer, read as the ring snapshots a
// subscription delivers, and the contact stream, opened with the proof of a surface it holds with input focus - and
// it prints one verdict line per phase for the gate to read. What moves the devices is the gate, through the vhost-user
// backend's control socket: each phase first prints the line the gate waits for, then watches.
//
//   hidcheck watch      the touchpad's moves and click arrive as pointer events, in order, and the touchscreen's
//                       two-finger contact and lift as contacts
//   hidcheck storm      the same, after the gate held both lines asserted with no report behind them and each
//                       driver reset its device
//   hidcheck cycle      the virtio-i2c binding disabled through the device policy: both HID bindings stopped as
//                       lost dependencies while the tablet still moves the cursor; enabled again, both bound again
//                       and delivering
//   hidcheck malformed  the touchscreen bound again over a descriptor the gate made malformed: the binding fails,
//                       and nothing is published for it

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{BindingState, LaunchContext, PolicyOutcome, PolicyVerb, device, device_policy_admin, input};
use rt::*;

const TICKS: u64 = 100;
// How long one phase watches for what the gate raised.
const WINDOW_TICKS: u64 = 30 * TICKS;

fn say(line: &[u8]) {
	print(b"hidcheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"hidcheck: FAIL ");
	print(line);
	print(b"\n");
	exit_with(1);
}

// The pointer service's recent events, as one subscription delivers them: a bounded snapshot of its ring.
fn snapshot(input_client: u64) -> Vec<(u16, u16, u8)> {
	let Some(stream) = input::Client::new(ChannelTransport { chan: input_client }).subscribe() else { fail(b"the pointer subscription was refused") };
	let mut events = Vec::new();
	let mut buf = [0u8; 64];
	loop {
		match recv_caps_deadline(stream, &mut buf, clock() + 2 * TICKS) {
			DeadlineCaps::Message { len, mut handles } => {
				if let Some(event) = input::subscribe_read(&buf[..len], &mut handles) {
					events.push((event.col, event.row, event.buttons));
				}
				for &handle in handles.as_slice() {
					close(handle);
				}
			}
			DeadlineCaps::Closed => break,
			DeadlineCaps::TimedOut => fail(b"a pointer snapshot did not end"),
		}
	}
	close(stream);
	events
}

// WHAT THE CURSOR DID over a window, from snapshots through it: the new events follow the longest overlap with the
// previous snapshot. `moved` before `pressed` before `released` is the order the touchpad's script reports in.
#[derive(Default)]
struct Pointer {
	moved: bool,
	pressed: bool,
	released: bool,
	last: Vec<(u16, u16, u8)>,
	at: Option<(u16, u16)>,
	in_order: bool,
}

impl Pointer {
	fn start(input_client: u64) -> Pointer {
		let last = snapshot(input_client);
		let at = last.last().map(|&(col, row, _)| (col, row));
		Pointer { last, at, in_order: true, ..Pointer::default() }
	}

	fn poll(&mut self, input_client: u64) {
		let now = snapshot(input_client);
		let overlap = (0..=self.last.len().min(now.len())).rev().find(|&n| self.last[self.last.len() - n..] == now[..n]).unwrap_or(0);
		for &(col, row, buttons) in &now[overlap..] {
			if self.at.is_some_and(|previous| previous != (col, row)) {
				if self.pressed {
					self.in_order = false;
				}
				self.moved = true;
			}
			self.at = Some((col, row));
			if buttons & 1 != 0 {
				if !self.moved {
					self.in_order = false;
				}
				self.pressed = true;
			} else if self.pressed {
				self.released = true;
			}
		}
		self.last = now;
	}

	fn done(&self) -> bool {
		self.moved && self.pressed && self.released
	}
}

// WHAT THE CONTACT STREAM CARRIED: which fingers went down and which came up.
#[derive(Default)]
struct Contacts {
	down: Vec<u8>,
	up: Vec<u8>,
}

impl Contacts {
	fn poll(&mut self, stream: u64) {
		let mut buf = [0u8; 64];
		loop {
			match try_recv_caps(stream, &mut buf) {
				PolledCaps::Message { len, mut handles } => {
					if let Some(contact) = input::subscribe_contacts_read(&buf[..len], &mut handles) {
						let list = if contact.tip { &mut self.down } else { &mut self.up };
						if !list.contains(&contact.id) {
							list.push(contact.id);
						}
					}
					for &handle in handles.as_slice() {
						close(handle);
					}
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => fail(b"the contact stream closed"),
			}
		}
	}

	fn done(&self) -> bool {
		self.down.len() >= 2 && self.down.iter().all(|id| self.up.contains(id))
	}
}

// A SURFACE WITH INPUT FOCUS, presented once, and the contact stream its proof opens.
struct Focus {
	_surface: surface::Surface,
	contacts: u64,
}

fn focus(display: u64, input_client: u64) -> Focus {
	let client = surface::connect(display);
	let Some(Ok(held)) = surface::Surface::create(&client, surface::wire_extent(64, 64), 2) else { fail(b"no surface") };
	if let Some(Ok(proto::system::AcquiredImage::Image(image))) = held.acquire() {
		let _ = held.present_whole(image);
	}
	let deadline = clock() + 10 * TICKS;
	let proof = loop {
		match held.input_focus() {
			Some(Ok(proof)) => break proof,
			_ if clock() < deadline => sleep_until(clock() + TICKS / 4),
			_ => fail(b"the surface never held input focus"),
		}
	};
	let Some(contacts) = input::Client::new(ChannelTransport { chan: input_client }).subscribe_contacts(&proof) else { fail(b"the contact stream was refused to a focused surface") };
	Focus { _surface: held, contacts }
}

// ONE PHASE'S WATCH: the gate is told, then the pointer and the contacts are awaited within the window.
fn watch(input_client: u64, focus: &Focus, cue: &[u8]) -> (bool, bool) {
	let mut pointer = Pointer::start(input_client);
	let mut contacts = Contacts::default();
	say(cue);
	let deadline = clock() + WINDOW_TICKS;
	while clock() < deadline && !(pointer.done() && contacts.done()) {
		sleep_until(clock() + TICKS / 5);
		pointer.poll(input_client);
		contacts.poll(focus.contacts);
	}
	if !pointer.in_order {
		fail(b"the touchpad's report arrived out of order: a move after the click, or a click before any move");
	}
	(pointer.done(), contacts.done())
}

fn verdict(phase: &[u8], (pointer, contacts): (bool, bool)) {
	if !pointer {
		print(b"hidcheck: FAIL ");
		print(phase);
		print(b": the touchpad's moves and click did not all arrive as pointer events\n");
		exit_with(1);
	}
	if !contacts {
		print(b"hidcheck: FAIL ");
		print(phase);
		print(b": the touchscreen's two contacts and their lift did not arrive as contacts\n");
		exit_with(1);
	}
	print(b"hidcheck: PASS ");
	print(phase);
	print(b"\n");
}

// Every binding, from DeviceService.
fn bindings(device_client: u64) -> Vec<proto::system::BindingRecord> {
	match device::Client::new(ChannelTransport { chan: device_client }).bindings() {
		Some(Ok(bindings)) => bindings,
		_ => fail(b"the device bindings could not be read"),
	}
}

// The index of the binding `artifact` holds whose device's identity contains `mark` (any, for an empty one).
fn index_of(device_client: u64, artifact: &str, mark: &str) -> Option<u32> {
	for binding in bindings(device_client).iter().filter(|binding| binding.artifact == artifact) {
		if mark.is_empty() {
			return Some(binding.index);
		}
		if let Some(Ok(entry)) = device::Client::new(ChannelTransport { chan: device_client }).get(&binding.index)
			&& entry.identity.contains(mark)
		{
			return Some(binding.index);
		}
	}
	None
}

fn states_of(device_client: u64, artifact: &str) -> Vec<BindingState> {
	bindings(device_client).iter().filter(|binding| binding.artifact == artifact).map(|binding| binding.state).collect()
}

// Wait until every `artifact` binding is in `wanted`, and there are `count` of them.
fn await_states(device_client: u64, artifact: &str, count: usize, wanted: &[BindingState], what: &[u8]) {
	let deadline = clock() + 20 * TICKS;
	loop {
		let states = states_of(device_client, artifact);
		if states.len() == count && states.iter().all(|state| wanted.contains(state)) {
			return;
		}
		if clock() >= deadline {
			fail(what);
		}
		sleep_until(clock() + TICKS / 4);
	}
}

fn apply(policy: u64, index: u32, verb: PolicyVerb, what: &[u8]) {
	let deadline = clock() + 10 * TICKS;
	loop {
		match device_policy_admin::Client::new(ChannelTransport { chan: policy }).apply(&index, &verb, "") {
			Some(Ok(PolicyOutcome::Accepted)) => return,
			Some(Ok(PolicyOutcome::Busy)) if clock() < deadline => sleep_until(clock() + TICKS / 4),
			_ => fail(what),
		}
	}
}

// THE CONTROLLER DISABLED AND ENABLED: both HID bindings stop as lost dependencies - the tablet still moving the
// cursor meanwhile - and bind again, delivering.
fn cycle(device_client: u64, policy: u64, input_client: u64, focus: &Focus) {
	let Some(controller) = index_of(device_client, "virtio_i2c", "") else { fail(b"cycle: no virtio-i2c binding") };
	apply(policy, controller, PolicyVerb::Disable, b"cycle: the virtio-i2c binding could not be disabled");
	await_states(device_client, "i2c_hid", 2, &[BindingState::DependencyPending], b"cycle: the HID bindings did not stop as lost dependencies");
	say(b"both HID bindings stopped as lost dependencies");
	// THE TABLET, moved from the host, still reaches the cursor - asked while the HID bindings are stopped, and judged
	// after the controller is enabled again, so a failure here does not leave the controller disabled for what follows.
	let mut pointer = Pointer::start(input_client);
	say(b"the controller is disabled - move the tablet now");
	let deadline = clock() + WINDOW_TICKS;
	while clock() < deadline && !pointer.moved {
		sleep_until(clock() + TICKS / 5);
		pointer.poll(input_client);
	}
	let tablet = pointer.moved;
	if tablet {
		say(b"the tablet still moves the cursor");
	}
	apply(policy, controller, PolicyVerb::Enable, b"cycle: the virtio-i2c binding could not be enabled again");
	await_states(device_client, "i2c_hid", 2, &[BindingState::Online], b"cycle: the HID bindings did not bind again once their controller was back");
	say(b"both HID bindings are online again");
	let delivered = watch(input_client, focus, b"raise the reports again now");
	if !tablet {
		let mut line = alloc::format!("cycle: with the HID bindings stopped, the tablet did not move the cursor - the ring held {} event(s)", pointer.last.len());
		if let Some(&(col, row, buttons)) = pointer.last.last() {
			line.push_str(&alloc::format!(", the last at column {col} row {row} buttons {buttons}"));
		}
		fail(line.as_bytes());
	}
	verdict(b"cycle", delivered);
}

// THE TOUCHSCREEN BOUND AGAIN OVER A MALFORMED DESCRIPTOR: its binding fails, and nothing is published for it.
fn malformed(device_client: u64, policy: u64) {
	let Some(screen) = index_of(device_client, "i2c_hid", "TSCR").or_else(|| index_of(device_client, "i2c_hid", "touchscreen")) else { fail(b"malformed: no touchscreen binding") };
	say(b"make the touchscreen's descriptor malformed now");
	sleep_until(clock() + 3 * TICKS);
	apply(policy, screen, PolicyVerb::Disable, b"malformed: the touchscreen's binding could not be disabled");
	apply(policy, screen, PolicyVerb::Enable, b"malformed: the touchscreen's binding could not be enabled again");
	let deadline = clock() + 20 * TICKS;
	loop {
		let record = bindings(device_client).into_iter().find(|binding| binding.index == screen);
		match record {
			Some(binding) if binding.state == BindingState::Failed => {
				if binding.providers != 0 {
					fail(b"malformed: the failed binding still has a provider published");
				}
				break;
			}
			_ if clock() < deadline => sleep_until(clock() + TICKS / 4),
			_ => fail(b"malformed: the touchscreen's binding did not fail over a malformed descriptor"),
		}
	}
	say(b"PASS malformed");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let args: Vec<u8> = context.arguments.clone().into_bytes();
	// The grants, in the order PermissionManager walks its vocabulary.
	let device_client = recv_tagged(bootstrap, &mut buf, b"DEVICE").unwrap_or(0);
	let policy = recv_tagged(bootstrap, &mut buf, b"DEVPOLICY").unwrap_or(0);
	let input_client = recv_tagged(bootstrap, &mut buf, b"INPUT").unwrap_or(0);
	let display = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or(0);
	if device_client == 0 || policy == 0 || input_client == 0 || display == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let phase = args.split(|&b| b == b' ').next().unwrap_or(&[]);
	if phase == b"malformed" {
		malformed(device_client, policy);
		exit();
	}
	let held = focus(display, input_client);
	match phase {
		b"watch" => verdict(b"watch", watch(input_client, &held, b"watching - raise the reports now")),
		b"storm" => verdict(b"storm", watch(input_client, &held, b"hold the lines now, then raise the reports")),
		b"cycle" => cycle(device_client, policy, input_client, &held),
		_ => fail(b"usage: hidcheck watch | storm | cycle | malformed"),
	}
	exit();
}
