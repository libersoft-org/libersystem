// midicheck - the in-guest scenario driver for the MIDI gate. DEVELOPMENT-ONLY.
//
// It holds the MIDI fixture's control endpoint, MIDI inventory and a receiver on endpoint 0 of the fixture
// device, and prints one verdict line per phase for the gate to read. The packets it scripts are raw
// USB-MIDI 1.0 packets; what it checks is what MidiService's decoder made of them. The service's handle
// baseline is the gate's to read, from the system graph, which holds the service's process.
//
//   midicheck receive      exact bytes, cables, kinds and SysEx fragments, in observation order at one receipt
//                          time, after a delayed read
//   midicheck malformed    misalignment, a reserved code, a cable the endpoint lacks: typed faults
//   midicheck cap          a SysEx over 64 kB in many small fragments is aborted by number; a new one recovers
//   midicheck unsupported  output and UMP are unsupported; receiving through inventory is denied
//   midicheck overflow     a flood past the queue ends the receiver with a readable overflow
//   midicheck lost         input the device lost ends the receiver with a source discontinuity
//   midicheck unplug       withdrawal during a SysEx ends the receiver as removed; the device comes back
//   midicheck fresh        a receiver on the replacement starts clean: no inherited SysEx, sequence zero
//   midicheck inherit      a transferred receiver does not outlive its dead owner

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, EventEndReason, LaunchContext, MidiAbortReason, MidiBatch, MidiChunkKind, MidiDirection, MidiEndpointId, MidiFaultCode, MidiItem, MidiProtocol, midi, midi_fixture, midi_input};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;

fn say(line: &[u8]) {
	print(b"midicheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"midicheck: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

struct Probe {
	fixture: u64,
	inventory: u64,
	input: u64,
}

impl Probe {
	fn fixture(&self) -> midi_fixture::Client<ChannelTransport> {
		midi_fixture::Client::with_deadline(ChannelTransport { chan: self.fixture }, clock() + 5 * TICKS)
	}
	fn inject(&self, packets: &[[u8; 4]]) {
		if !matches!(self.fixture().inject(&0, &packets.concat()), Some(Ok(()))) {
			fail(b"the fixture could not deliver a batch");
		}
	}
	fn inject_raw(&self, bytes: &[u8]) {
		if !matches!(self.fixture().inject(&0, &bytes.to_vec()), Some(Ok(()))) {
			fail(b"the fixture could not deliver a batch");
		}
	}
	fn read(&self, max: u16, wait_ms: u32) -> MidiBatch {
		match midi_input::Client::with_deadline(ChannelTransport { chan: self.input }, clock() + 10 * TICKS).read(&max, &wait_ms) {
			Some(Ok(batch)) => batch,
			_ => fail(b"a read failed"),
		}
	}
	// Everything the receiver has, until a read comes back empty.
	fn drain(&self) -> Vec<MidiItem> {
		let mut items = Vec::new();
		loop {
			let batch = self.read(64, 300);
			if batch.events.is_empty() {
				return items;
			}
			items.extend(batch.events.into_iter().map(|event| event.item));
		}
	}
	fn endpoint(&self) -> MidiEndpointId {
		match midi_input::Client::with_deadline(ChannelTransport { chan: self.input }, clock() + 5 * TICKS).endpoint() {
			Some(Ok(endpoint)) => endpoint.id,
			_ => fail(b"the receiver could not describe its endpoint"),
		}
	}
}

fn receive(probe: &Probe) {
	// ONE BATCH, BOTH CABLES, INTERLEAVED: a note on each, a SysEx on cable 0 with a realtime byte inside it,
	// and a program change on cable 1.
	probe.inject(&[[0x09, 0x90, 0x3c, 0x64], [0x19, 0x91, 0x40, 0x50], [0x04, 0xf0, 0x7e, 0x7f], [0x0f, 0xf8, 0x00, 0x00], [0x06, 0x09, 0xf7, 0x00], [0x1c, 0xc1, 0x05, 0x00]]);
	let delivered = clock_ns();
	// A DELAYED READ: the receipt time is the batch's, not the read's.
	sleep_until(clock() + TICKS / 2);
	let batch = probe.read(64, 1000);
	if batch.events.len() != 6 {
		fail(b"receive: the batch did not decode to six events");
	}
	let expected: [(u8, MidiChunkKind, &[u8]); 6] = [
		(0, MidiChunkKind::Short, &[0x90, 0x3c, 0x64]),
		(1, MidiChunkKind::Short, &[0x91, 0x40, 0x50]),
		(0, MidiChunkKind::SysexStart, &[0xf0, 0x7e, 0x7f]),
		(0, MidiChunkKind::Short, &[0xf8]),
		(0, MidiChunkKind::SysexEnd, &[0x09, 0xf7]),
		(1, MidiChunkKind::Short, &[0xc1, 0x05]),
	];
	let first = batch.events[0].header.clone();
	for (at, (event, (cable, kind, bytes))) in batch.events.iter().zip(expected.iter()).enumerate() {
		let MidiItem::Chunk(chunk) = &event.item else { fail(b"receive: an event was not a chunk") };
		if chunk.cable != *cable || chunk.kind != *kind || chunk.bytes != *bytes {
			fail(b"receive: an event's cable, kind or bytes are not the packet's");
		}
		// EQUAL TIME, OBSERVATION ORDER: one completion's packets share one receipt time and keep their order.
		if event.header.received_ns != first.received_ns || event.header.sequence != first.sequence + at as u64 {
			fail(b"receive: the batch's events do not share its receipt time in packet order");
		}
	}
	let MidiItem::Chunk(start) = &batch.events[2].item else { fail(b"receive: no start") };
	let MidiItem::Chunk(end) = &batch.events[4].item else { fail(b"receive: no end") };
	if start.sysex_message.is_none() || start.sysex_message != end.sysex_message || !start.start || start.end || end.start || !end.end {
		fail(b"receive: the SysEx fragments do not carry one message and its boundaries");
	}
	if first.received_ns > delivered || clock_ns() - first.received_ns < 400_000_000 {
		fail(b"receive: the receipt time is not the time the batch was delivered");
	}
	say(b"PASS receive: exact bytes and cables, SysEx fragments under one message with a realtime byte in place, one receipt time in packet order, after a delayed read");
}

fn malformed(probe: &Probe) {
	probe.inject_raw(&[0x09, 0x90, 0x3c, 0x64, 0x00]);
	probe.inject(&[[0x00, 0x00, 0x00, 0x00]]);
	probe.inject(&[[0x59, 0x90, 0x3c, 0x64]]);
	let items = probe.drain();
	let faults: Vec<(Option<u8>, MidiFaultCode)> = items.iter().filter_map(|item| if let MidiItem::Fault(fault) = item { Some((fault.cable, fault.code)) } else { None }).collect();
	if faults != alloc::vec![(None, MidiFaultCode::Alignment), (Some(0), MidiFaultCode::Reserved), (None, MidiFaultCode::Cable)] {
		fail(b"malformed: the faults were not typed and attributed as expected");
	}
	if items.iter().any(|item| matches!(item, MidiItem::Chunk(_))) {
		fail(b"malformed: a refused packet was reinterpreted as a message");
	}
	say(b"PASS malformed: misalignment, a reserved code and a cable the endpoint lacks were typed faults, and nothing was reinterpreted");
}

fn cap(probe: &Probe) {
	probe.inject(&[[0x04, 0xf0, 0x00, 0x00]]);
	let mut items = probe.drain();
	// Past 64 kB in three-byte fragments, sixty-four packets a batch, read as it goes.
	let batch: Vec<[u8; 4]> = alloc::vec![[0x04, 0x01, 0x02, 0x03]; 64];
	for _ in 0..342 {
		probe.inject(&batch);
		items.extend(probe.drain());
	}
	probe.inject(&[[0x05, 0xf7, 0x00, 0x00]]);
	items.extend(probe.drain());
	let aborted: Vec<u32> = items.iter().filter_map(|item| if let MidiItem::Aborted(abort) = item { (abort.reason == MidiAbortReason::Cap).then_some(abort.message) } else { None }).collect();
	let Some(MidiItem::Chunk(start)) = items.first() else { fail(b"cap: the start was not delivered") };
	if aborted.len() != 1 || Some(aborted[0]) != start.sysex_message {
		fail(b"cap: the oversized SysEx was not aborted once, by its number");
	}
	if items.iter().any(|item| matches!(item, MidiItem::Chunk(chunk) if chunk.end)) {
		fail(b"cap: a truncated SysEx was presented as ended");
	}
	// RECOVERY at a new message boundary.
	probe.inject(&[[0x06, 0xf0, 0xf7, 0x00]]);
	match probe.drain().as_slice() {
		[MidiItem::Chunk(chunk)] if chunk.start && chunk.end && chunk.sysex_message != start.sysex_message => {}
		_ => fail(b"cap: a new SysEx after the abort did not arrive whole"),
	}
	say(b"PASS cap: a SysEx past 64 kB in 21 888 three-byte fragments was aborted once by its number, never ended, and a new one arrived whole");
}

fn unsupported(probe: &Probe) {
	let endpoint = probe.endpoint();
	let open = |direction: MidiDirection, protocol: MidiProtocol| midi::Client::with_deadline(ChannelTransport { chan: probe.inventory }, clock() + 5 * TICKS).open(&endpoint, &direction, &protocol);
	if !matches!(open(MidiDirection::Transmit, MidiProtocol::Midi1), Some(Err(Error::Unsupported))) || !matches!(open(MidiDirection::Receive, MidiProtocol::Ump), Some(Err(Error::Unsupported))) {
		fail(b"unsupported: output or UMP was not refused as unsupported");
	}
	if !matches!(open(MidiDirection::Receive, MidiProtocol::Midi1), Some(Err(Error::Denied))) {
		fail(b"unsupported: inventory opened a receiver");
	}
	say(b"PASS unsupported: output and UMP are unsupported, and inventory cannot open a receiver");
}

fn overflow(probe: &Probe) {
	if !matches!(probe.fixture().flood(&0, &5), Some(Ok(()))) {
		fail(b"overflow: the fixture could not flood the endpoint");
	}
	sleep_until(clock() + TICKS / 2);
	let batch = probe.read(64, 1000);
	match batch.end {
		Some(end) if end.reason == EventEndReason::Overflow && batch.events.is_empty() => {}
		_ => fail(b"overflow: the flood did not end the receiver with a readable overflow and nothing continuous"),
	}
	say(b"PASS overflow: a flood past 256 events ended the receiver, discarded what it held, and the next read said overflow");
}

fn lost(probe: &Probe) {
	probe.inject(&[[0x09, 0x90, 0x3c, 0x64]]);
	if !matches!(probe.fixture().lose(&0), Some(Ok(()))) {
		fail(b"lost: the fixture could not report input lost");
	}
	sleep_until(clock() + TICKS / 4);
	let batch = probe.read(64, 1000);
	match batch.end {
		Some(end) if end.reason == EventEndReason::SourceDiscontinuity && batch.events.is_empty() => {}
		_ => fail(b"lost: lost input did not end the receiver with a source discontinuity"),
	}
	say(b"PASS lost: input the device lost ended the receiver with a source discontinuity and nothing that looked complete");
}

fn unplug(probe: &Probe) {
	probe.inject(&[[0x04, 0xf0, 0x01, 0x02]]);
	if !matches!(probe.fixture().withdraw(), Some(Ok(()))) {
		fail(b"unplug: the fixture could not withdraw the device");
	}
	sleep_until(clock() + TICKS / 2);
	let batch = probe.read(64, 1000);
	match batch.end {
		Some(end) if end.reason == EventEndReason::Removed && batch.events.is_empty() => {}
		_ => fail(b"unplug: withdrawal during a SysEx did not end the receiver as removed"),
	}
	if !matches!(probe.fixture().republish(), Some(Ok(()))) {
		fail(b"unplug: the fixture could not republish the device");
	}
	say(b"PASS unplug: withdrawal during a SysEx ended the receiver as removed with nothing partial delivered, and the device came back");
}

fn fresh(probe: &Probe) {
	// A CONTINUATION WITH NOTHING OPEN: no SysEx was inherited from the device's previous life.
	probe.inject(&[[0x04, 0x01, 0x02, 0x03], [0x09, 0x90, 0x3c, 0x64]]);
	let batch = probe.read(64, 1000);
	match batch.events.as_slice() {
		[fault, note] if matches!(&fault.item, MidiItem::Fault(fault) if fault.code == MidiFaultCode::Status) && matches!(note.item, MidiItem::Chunk(_)) && fault.header.sequence == 0 => {}
		_ => fail(b"fresh: the replacement's receiver did not start clean"),
	}
	say(b"PASS fresh: the replacement's receiver started at sequence zero with no inherited SysEx");
}

fn inherit(probe: &Probe) {
	let mut buf = [0u8; 64];
	let handles = loop {
		match try_recv_caps(stdin(), &mut buf) {
			PolledCaps::Message { handles, .. } if !handles.as_slice().is_empty() => break handles,
			PolledCaps::Message { .. } => {}
			PolledCaps::Empty => sleep_until(clock() + TICKS / 10),
			PolledCaps::Closed => fail(b"inherit: the holder sent no endpoint"),
		}
	};
	let inherited = handles.first();
	let deadline = clock() + 5 * TICKS;
	loop {
		let receiving = match midi::Client::with_deadline(ChannelTransport { chan: probe.inventory }, clock() + 2 * TICKS).endpoints() {
			Some(Ok(endpoints)) => endpoints.iter().any(|endpoint| endpoint.id.endpoint == 1 && endpoint.receiving),
			_ => fail(b"inherit: the inventory could not be read"),
		};
		if !receiving {
			break;
		}
		if clock() >= deadline {
			fail(b"inherit: the receiver outlived its owner");
		}
		sleep_until(clock() + TICKS / 10);
	}
	if matches!(midi_input::Client::with_deadline(ChannelTransport { chan: inherited }, clock() + 2 * TICKS).status(), Some(Ok(_))) {
		fail(b"inherit: a transferred receiver kept its dead owner's authority");
	}
	close(inherited);
	say(b"PASS inherit: the receiver ended with its owner and freed its endpoint, and a transferred copy could do nothing");
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
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	let inventory = recv_tagged(bootstrap, &mut buf, CAP_MIDI).unwrap_or(0);
	let input = recv_tagged(bootstrap, &mut buf, b"MIDIINPUT").unwrap_or(0);
	if fixture == 0 || inventory == 0 || input == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let probe = Probe { fixture, inventory, input };
	match args.split(|&b| b == b' ').next().unwrap_or(&[]) {
		b"receive" => receive(&probe),
		b"malformed" => malformed(&probe),
		b"cap" => cap(&probe),
		b"unsupported" => unsupported(&probe),
		b"overflow" => overflow(&probe),
		b"lost" => lost(&probe),
		b"unplug" => unplug(&probe),
		b"fresh" => fresh(&probe),
		b"inherit" => inherit(&probe),
		_ => fail(b"usage: midicheck receive | malformed | cap | unsupported | overflow | lost | unplug | fresh | inherit"),
	}
	exit();
}
