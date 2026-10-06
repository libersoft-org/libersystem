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
//   hidcheck ring       one view of the pointer ring - a second client's answer beside a phase that is watching
//
// AND THE INPUT FIGURES, which are a measurement and no gate's - the driver roadmap's integration item records them:
//
//   hidcheck route virtio|usb
//                       which of the machine's input devices QEMU feeds: the virtio-input functions - each binding
//                       disabled and enabled again through the device policy, so the fresh DRIVER_OK makes it the
//                       device QEMU feeds - or, `usb`, the HID devices on the xHCI bus, the virtio-input bindings left
//                       disabled. `route virtio` puts the machine back
//   hidcheck rate keys|pointer [focused]
//                       thirty-two cued single events the host answers - each cue's time to the event's arrival here
//                       bounds injection-to-delivery from above - and then a stream the host sends, counted until it
//                       has been quiet for a second, every time by this guest's clock. `focused` holds a surface with
//                       input focus through a pointer run, as a graphical application would, so the text console is
//                       not the one following the pointer

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
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

// A surface presented once and holding input focus, and the one-shot proof of it a stream is opened with.
fn focused(display: u64) -> (surface::Surface, u64) {
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
	(held, proof)
}

fn focus(display: u64, input_client: u64) -> Focus {
	let (held, proof) = focused(display);
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
	let opened = clock();
	let deadline = opened + WINDOW_TICKS;
	// EACH DISTINCT VIEW OF THE RING, said - at most eight, with how many polls and ticks into the window - so a
	// window that sees nothing says what it saw instead.
	let mut views: Vec<(usize, Option<(u16, u16, u8)>)> = Vec::new();
	let mut polls = 0u32;
	while clock() < deadline && !pointer.moved {
		sleep_until(clock() + TICKS / 5);
		pointer.poll(input_client);
		polls += 1;
		let view = (pointer.last.len(), pointer.last.last().copied());
		if views.len() < 8 && views.last() != Some(&view) {
			views.push(view);
			say(alloc::format!("the ring holds {} event(s), the last {:?} - poll {polls}, tick {}", view.0, view.1, clock() - opened).as_bytes());
		}
	}
	say(alloc::format!("the window closed after {polls} poll(s) and {} tick(s)", clock() - opened).as_bytes());
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

// ------------------------------------------------------------------ the input figures

// How many cued single events a rate phase asks for, how long one is waited for, and how long a stream must be quiet
// before it is over. THIRTY-TWO, InputService's pointer ring: by the stream's cue the ring holds the rounds' events and
// nothing older, so nothing in it can be taken for a move of the stream.
const ROUNDS: u32 = 32;
const CUE_TICKS: u64 = 2 * TICKS;
const QUIET_TICKS: u64 = TICKS;

// WHERE QEMU'S INPUT GOES. QEMU hands a host key or tablet event to the device of its kind that became active LAST: a
// virtio-input function becomes active at its driver's DRIVER_OK and inactive at its reset, a USB keyboard when it is
// created and a USB tablet at its first poll. So `virtio` disables and enables every virtio-input binding - each comes
// back with a fresh DRIVER_OK - and `usb` disables them, which resets both functions and leaves the HID devices on the
// xHCI bus the only ones QEMU can feed. A STORED DISABLE OUTLIVES A BOOT, so a measurement ends with `route virtio`.
fn route(device_client: u64, policy: u64, to: &[u8]) {
	let indices: Vec<u32> = bindings(device_client).iter().filter(|binding| binding.artifact == "virtio_input").map(|binding| binding.index).collect();
	if indices.is_empty() {
		fail(b"route: there is no virtio-input binding");
	}
	for &index in &indices {
		apply(policy, index, PolicyVerb::Disable, b"route: a virtio-input binding could not be disabled");
	}
	await_states(device_client, "virtio_input", indices.len(), &[BindingState::Disabled], b"route: the virtio-input bindings did not stop");
	if to == b"usb" {
		say(format!("routed to the USB HID devices: {} virtio-input binding(s) disabled", indices.len()).as_bytes());
		return;
	}
	for &index in &indices {
		apply(policy, index, PolicyVerb::Enable, b"route: a virtio-input binding could not be enabled again");
	}
	await_states(device_client, "virtio_input", indices.len(), &[BindingState::Online], b"route: the virtio-input bindings did not bind again");
	say(format!("routed to the virtio-input devices: {} binding(s) bound again", indices.len()).as_bytes());
}

// THE CUE THE HOST ANSWERS, written straight into the kernel's console ring: the console mirror a `print` takes is
// buffered, and a cue that waited in it would be measured as the device's delay.
fn cue(what: &str) {
	debug_write(format!("hidcheck-cue {what}\n").as_bytes());
}

fn millis(ns: u64) -> String {
	format!("{}.{:02} ms", ns / 1_000_000, ns % 1_000_000 / 10_000)
}

// The cued rounds' spread - the fastest, the middle and the slowest - and how many never arrived.
fn rounds_line(what: &str, mut samples: Vec<u64>, missed: u32) -> String {
	samples.sort_unstable();
	match (samples.first(), samples.last()) {
		(Some(&low), Some(&high)) => format!("{what} - {} cued event(s): cue to arrival {} fastest, {} median, {} slowest; {missed} never arrived", samples.len(), millis(low), millis(samples[samples.len() / 2]), millis(high)),
		_ => format!("{what} - no cued event arrived ({missed} cue(s))"),
	}
}

// A stream as counted here: how many, from the first to the last, and the longest wait between two.
fn stream_line(what: &str, count: u64, first: u64, last: u64, gap: u64, extra: &str) -> String {
	let span = last.saturating_sub(first);
	let rate = if span == 0 { 0 } else { (count.saturating_sub(1)) * 1_000_000_000 / span };
	format!("{what} - stream: {count} event(s) in {}, {rate} a second, the longest gap {}{extra}", millis(span), millis(gap))
}

// THE KEYS, on the live stream a focused surface's proof opens - which is how an application receives them.
fn rate_keys(display: u64, input_client: u64) {
	let (_held, proof) = focused(display);
	let Some(stream) = input::Client::new(ChannelTransport { chan: input_client }).subscribe_keys(&proof) else { fail(b"rate: the key stream was refused to a focused surface") };
	let mut buf = [0u8; 64];
	// When the next key event arrived; `Some(None)` when none came by `until`, and `None` when the stream closed -
	// InputService's answer to a reader that fell a channel behind.
	let mut next = |until: u64| -> Option<Option<u64>> {
		loop {
			match recv_caps_deadline(stream, &mut buf, until) {
				DeadlineCaps::Message { len, mut handles } => {
					let at = clock_ns();
					let event = input::subscribe_keys_read(&buf[..len], &mut handles);
					for &handle in handles.as_slice() {
						close(handle);
					}
					if event.is_some() {
						return Some(Some(at));
					}
				}
				DeadlineCaps::TimedOut => return Some(None),
				DeadlineCaps::Closed => return None,
			}
		}
	};
	let mut samples: Vec<u64> = Vec::new();
	let mut missed = 0u32;
	for round in 1..=ROUNDS {
		let asked = clock_ns();
		cue(&format!("{round}"));
		match next(clock() + CUE_TICKS) {
			Some(Some(at)) => samples.push(at - asked),
			Some(None) => missed += 1,
			None => fail(b"rate: the key stream closed during the cued events"),
		}
	}
	say(rounds_line("keys", samples, missed).as_bytes());
	cue("stream");
	let Some(Some(first)) = next(clock() + 10 * TICKS) else { fail(b"rate: no key of the stream arrived") };
	let (mut count, mut last, mut gap, mut closed) = (1u64, first, 0u64, false);
	loop {
		match next(clock() + QUIET_TICKS) {
			Some(Some(at)) => {
				gap = gap.max(at - last);
				last = at;
				count += 1;
			}
			Some(None) => break,
			None => {
				closed = true;
				break;
			}
		}
	}
	let extra = if closed { ", then InputService closed the stream: this reader fell a channel behind" } else { "" };
	say(stream_line("keys", count, first, last, gap, extra).as_bytes());
	close(stream);
}

// THE POINTER, as its one client API gives it: a snapshot of the last thirty-two events InputService mapped. So the host
// NUMBERS ITS MOVES BY WHERE THEY LAND - the stream's move n at column n mod 80 of row n / 80, the cued rounds on the
// last row alone - and an event is counted once by its position, whichever snapshot it was seen in. A snapshot a tick
// sees every event while fewer than thirty-two arrive between two of them, 3200 a second; a move that never became an
// event of its own - folded into the next by the driver, or lost before it - is one the host sent that never arrived.
fn rate_pointer(input_client: u64, display: u64, focus: bool) {
	// A FOCUSED SURFACE, when asked for, for the whole run: the console stops following the pointer while it holds focus.
	let _held = focus.then(|| {
		let (held, proof) = focused(display);
		close(proof);
		held
	});
	let mut samples: Vec<u64> = Vec::new();
	let mut missed = 0u32;
	let mut seen = snapshot(input_client);
	for round in 1..=ROUNDS {
		let asked = clock_ns();
		cue(&format!("{round}"));
		let until = clock() + CUE_TICKS;
		loop {
			let now = snapshot(input_client);
			if now != seen {
				samples.push(clock_ns() - asked);
				seen = now;
				break;
			}
			if clock() >= until {
				missed += 1;
				break;
			}
		}
	}
	say(rounds_line("pointer", samples, missed).as_bytes());
	cue("stream");
	let until = clock() + 10 * TICKS;
	let mut arrived: Vec<bool> = alloc::vec![false; STREAM_POSITIONS];
	let (mut count, mut first, mut last, mut gap, mut highest) = (0u64, 0u64, 0u64, 0u64, 0usize);
	let mut quiet_from = 0u64;
	// THE SLOWEST SNAPSHOT: a subscription InputService answers late is InputService not running its loop.
	let mut slowest = 0u64;
	loop {
		let asked = clock_ns();
		let now = snapshot(input_client);
		let at = clock_ns();
		slowest = slowest.max(at - asked);
		let mut fresh = 0u64;
		for &(col, row, _) in &now {
			let position = row as usize * GRID_COLUMNS + col as usize;
			if position != 0 && position < STREAM_POSITIONS && !arrived[position] {
				arrived[position] = true;
				highest = highest.max(position);
				fresh += 1;
			}
		}
		if fresh != 0 {
			if count == 0 {
				first = at;
			} else {
				gap = gap.max(at - last);
			}
			count += fresh;
			last = at;
			quiet_from = clock();
		} else if count == 0 && clock() >= until {
			fail(b"rate: no pointer event of the stream arrived");
		} else if count != 0 && clock() >= quiet_from + QUIET_TICKS {
			break;
		}
		sleep_until(clock() + 1);
	}
	let extra = format!(", the last move numbered {highest}: {} of the moves up to it never arrived as an event of their own; the slowest snapshot {}", highest as u64 - count.min(highest as u64), millis(slowest));
	say(stream_line("pointer", count, first, last, gap, &extra).as_bytes());
}

// InputService's grid, and the stream's positions on it: every row but the last, which the cued rounds use.
const GRID_COLUMNS: usize = 80;
const STREAM_POSITIONS: usize = GRID_COLUMNS * 49;

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
	// ONE VIEW OF THE POINTER RING, and nothing else: a second client's answer beside a phase that is watching.
	if phase == b"ring" {
		let view = snapshot(input_client);
		say(alloc::format!("ring: {} event(s), the last {:?}", view.len(), view.last()).as_bytes());
		exit();
	}
	if phase == b"malformed" {
		malformed(device_client, policy);
		exit();
	}
	// THE INPUT FIGURES' TWO PHASES, which take a second word.
	let which = args.split(|&b| b == b' ').filter(|word| !word.is_empty()).nth(1).unwrap_or(&[]);
	match (phase, which) {
		(b"route", b"virtio" | b"usb") => {
			route(device_client, policy, which);
			exit();
		}
		(b"rate", b"keys") => {
			rate_keys(display, input_client);
			exit();
		}
		(b"rate", b"pointer") => {
			let focus = args.split(|&b| b == b' ').filter(|word| !word.is_empty()).nth(2) == Some(b"focused".as_slice());
			rate_pointer(input_client, display, focus);
			exit();
		}
		_ => {}
	}
	let held = focus(display, input_client);
	match phase {
		b"watch" => verdict(b"watch", watch(input_client, &held, b"watching - raise the reports now")),
		b"storm" => verdict(b"storm", watch(input_client, &held, b"hold the lines now, then raise the reports")),
		b"cycle" => cycle(device_client, policy, input_client, &held),
		_ => fail(b"usage: hidcheck watch | storm | cycle | malformed | ring | route virtio|usb | rate keys|pointer [focused]"),
	}
	exit();
}
