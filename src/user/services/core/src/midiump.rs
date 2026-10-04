// midiump - the MIDI gate's UMP probe. DEVELOPMENT-ONLY.
//
// It holds the MIDI fixture's control endpoint, MIDI inventory, a receiver on the fixture's UMP receive endpoint 3 and
// a sender on its UMP transmit endpoint 4 - which the fixture plays back on endpoint 3 - and prints one verdict line
// per phase for the gate to read. What it checks is what MidiService made of the words: UMP messages cut, counted and
// delivered word for word, and the translation to MIDI 1.0 and back.
//
//   midiump ump        the endpoint describes itself with its blocks; messages on all four groups - MIDI 2.0 channel
//                      voice, MIDI 1.0 in UMP, a SysEx7 in two parts, a groupless utility message - come back word for
//                      word; a batch with a short message, a group the endpoint lacks, a group its words do not carry or
//                      a SysEx continuation with nothing open sends nothing
//   midiump translate  read as MIDI 1.0, UMP comes back as chunks - a group as a cable, MIDI 2.0 translated down, a
//                      SysEx7 as its fragments - and chunks sent on the UMP endpoint come back too; read as UMP again,
//                      a message comes back as itself

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, LaunchContext, MidiBlockDirection, MidiChunk, MidiChunkKind, MidiItem, MidiProtocol, MidiUmp, midi_input, midi_output};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;

fn say(line: &[u8]) {
	print(b"midiump: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"midiump: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

struct Probe {
	input: u64,
	output: u64,
}

impl Probe {
	fn input(&self) -> midi_input::Client<ChannelTransport> {
		midi_input::Client::with_deadline(ChannelTransport { chan: self.input }, clock() + 10 * TICKS)
	}
	fn output(&self) -> midi_output::Client<ChannelTransport> {
		midi_output::Client::with_deadline(ChannelTransport { chan: self.output }, clock() + 5 * TICKS)
	}
	// Everything the receiver has, until a read comes back empty.
	fn drain(&self) -> Vec<MidiItem> {
		let mut items = Vec::new();
		loop {
			let batch = match self.input().read(&64, &300) {
				Some(Ok(batch)) => batch,
				_ => fail(b"a read failed"),
			};
			if batch.events.is_empty() {
				return items;
			}
			items.extend(batch.events.into_iter().map(|event| event.item));
		}
	}
}

fn message(group: Option<u8>, words: &[u32]) -> MidiUmp {
	MidiUmp { group, words: words.to_vec() }
}

fn ump(probe: &Probe) {
	let described = match probe.input().endpoint() {
		Some(Ok(endpoint)) => endpoint,
		_ => fail(b"ump: the receiver could not describe its endpoint"),
	};
	let blocks: Vec<(u8, &str, u8, u8)> = described.blocks.iter().map(|block| (block.id, block.name.as_str(), block.first_group, block.groups)).collect();
	if described.protocol != MidiProtocol::Ump || described.cables != 4 || blocks != [(1, "fixture keys", 0, 2), (2, "fixture pads", 2, 2)] || described.blocks.iter().any(|block| block.direction != MidiBlockDirection::Both) {
		fail(b"ump: the UMP endpoint did not describe itself with its four groups and its two blocks");
	}
	let sent = alloc::vec![
		message(Some(0), &[0x4090_3C00, 0xFFFF_0000]),
		message(Some(1), &[0x21B1_0740]),
		message(Some(2), &[0x3216_0102, 0x0304_0506]),
		message(Some(2), &[0x3231_0700, 0]),
		message(Some(3), &[0x43B2_0700, 0x8000_0000]),
		message(None, &[0x0020_1234]),
	];
	if !matches!(probe.output().send_ump(&sent), Some(Ok(()))) {
		fail(b"ump: the messages were not taken");
	}
	let back = probe.drain();
	let back: Vec<&MidiUmp> = back.iter().filter_map(|item| if let MidiItem::Ump(message) = item { Some(message) } else { None }).collect();
	if back.len() != sent.len() || back.iter().zip(sent.iter()).any(|(got, sent)| got.group != sent.group || got.words != sent.words) {
		fail(b"ump: the messages did not come back word for word");
	}
	// REFUSED WHOLE: each bad batch sends nothing, its good message included.
	for bad in [
		alloc::vec![message(Some(0), &[0x2090_3C40]), message(Some(0), &[0x4090_3C00])],
		alloc::vec![message(Some(5), &[0x25B1_0740])],
		alloc::vec![message(Some(1), &[0x22B1_0740])],
		alloc::vec![message(Some(1), &[0x3121_0102, 0])],
	] {
		if !matches!(probe.output().send_ump(&bad), Some(Err(Error::Invalid))) {
			fail(b"ump: a batch that is not all messages the endpoint carries was not refused");
		}
	}
	if !probe.drain().is_empty() {
		fail(b"ump: a refused batch reached the device");
	}
	say(b"PASS ump: the endpoint's four groups and two blocks as declared; MIDI 2.0 and MIDI 1.0 channel voice, a SysEx7 in two parts and a utility message came back word for word; a bad batch sent nothing");
}

fn chunk(cable: u8, kind: MidiChunkKind, bytes: &[u8]) -> MidiChunk {
	MidiChunk { cable, kind, bytes: bytes.to_vec(), sysex_message: None, start: false, end: false }
}

fn translate(probe: &Probe) {
	if !matches!(probe.input().protocol(&MidiProtocol::Midi1), Some(Ok(()))) {
		fail(b"translate: the receiver would not read MIDI 1.0");
	}
	let sent = alloc::vec![message(Some(1), &[0x2191_3C64]), message(Some(0), &[0x4090_3C00, 0xFFFF_0000]), message(Some(2), &[0x3203_0102, 0x0300_0000])];
	if !matches!(probe.output().send_ump(&sent), Some(Ok(()))) {
		fail(b"translate: the messages were not taken");
	}
	let back = probe.drain();
	let shape: Vec<(u8, MidiChunkKind, &[u8])> = back.iter().filter_map(|item| if let MidiItem::Chunk(chunk) = item { Some((chunk.cable, chunk.kind, chunk.bytes.as_slice())) } else { None }).collect();
	let want: [(u8, MidiChunkKind, &[u8]); 4] = [
		(1, MidiChunkKind::Short, &[0x91, 0x3C, 0x64]),
		(0, MidiChunkKind::Short, &[0x90, 0x3C, 0x7F]),
		(2, MidiChunkKind::SysexStart, &[0xF0, 0x01, 0x02]),
		(2, MidiChunkKind::SysexEnd, &[0x03, 0xF7]),
	];
	if shape != want || back.len() != want.len() {
		fail(b"translate: UMP read as MIDI 1.0 did not come back as its chunks, a group as a cable");
	}
	// CHUNKS ON THE UMP ENDPOINT go out as MIDI 1.0 in UMP and come back as themselves.
	if !matches!(probe.output().send(&alloc::vec![chunk(3, MidiChunkKind::Short, &[0xB3, 0x07, 0x40])]), Some(Ok(()))) {
		fail(b"translate: chunks were not taken on the UMP endpoint");
	}
	match &probe.drain()[..] {
		[MidiItem::Chunk(chunk)] if chunk.cable == 3 && chunk.bytes == [0xB3, 0x07, 0x40] => {}
		_ => fail(b"translate: a chunk sent on the UMP endpoint did not come back"),
	}
	if !matches!(probe.input().protocol(&MidiProtocol::Ump), Some(Ok(()))) || !matches!(probe.output().send_ump(&alloc::vec![message(Some(1), &[0x2191_3C64])]), Some(Ok(()))) {
		fail(b"translate: the receiver would not read UMP again");
	}
	match &probe.drain()[..] {
		[MidiItem::Ump(message)] if message.group == Some(1) && message.words == [0x2191_3C64] => {}
		_ => fail(b"translate: read as UMP again, the message did not come back as itself"),
	}
	say(b"PASS translate: UMP read as MIDI 1.0 came back as chunks - a group as a cable, MIDI 2.0 translated down, a SysEx7 as its fragments - chunks sent on the UMP endpoint came back, and UMP read as UMP again");
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
	let output = recv_tagged(bootstrap, &mut buf, b"MIDIOUTPUT").unwrap_or(0);
	if fixture == 0 || inventory == 0 || input == 0 || output == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let probe = Probe { input, output };
	match args.split(|&b| b == b' ').next().unwrap_or(&[]) {
		b"ump" => ump(&probe),
		b"translate" => translate(&probe),
		_ => fail(b"usage: midiump ump | translate"),
	}
	exit();
}
