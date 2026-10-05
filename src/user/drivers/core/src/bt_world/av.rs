// THE FIXTURE'S A2DP AND AVRCP: the headset as an A2DP sink with a remote control that levels itself, and the phone as
// an A2DP source whose remote control takes play and pause.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS PROTOCOLS ARE ITS OWN: the AVDTP and AVCTP messages here are written from
// the specifications apart from the host's `service_logic::avdtp` and `avrcp`, and what the headset hears it judges
// with `bt_sbc` - its own frame reader, CRC, allocation and dequantization - so a stream the headset reports as good
// is one two implementations agree on.
//
//   the headset   accepts a stream to its one SBC sink (48 or 44.1 kHz, every mode, bitpool 2 to 53), reports a delay
//                 of 150 ms once configured, and when the stream is suspended says what it heard: the frames, their
//                 configuration, whether every CRC held, and which subband was loudest. Its remote control answers
//                 the absolute volume - registered for, set, and changed on its own word - and presses play.
//   the phone     streams to the host on its word: discovers the host's sink, configures it at 44.1 kHz joint stereo,
//                 bitpool 35, and sends a tone written into one subband on its own clock. Its remote control takes
//                 the host's buttons.

use super::*;
use crate::bt_sbc;

pub(super) const AVDTP: u16 = 0x0019;
pub(super) const AVCTP: u16 = 0x0017;

// AVDTP's signals and message types.
const DISCOVER: u8 = 0x01;
const GET_CAPABILITIES: u8 = 0x02;
const SET_CONFIGURATION: u8 = 0x03;
const OPEN: u8 = 0x06;
const START: u8 = 0x07;
const CLOSE: u8 = 0x08;
const SUSPEND: u8 = 0x09;
const GET_ALL_CAPABILITIES: u8 = 0x0c;
const DELAY_REPORT: u8 = 0x0d;
const COMMAND: u8 = 0;
const ACCEPT: u8 = 2;
const REJECT: u8 = 3;

// The headset's delay, in tenths of a millisecond.
const HEADSET_DELAY: u16 = 1_500;
// The subband the phone's tone is written into.
const PHONE_SUBBAND: usize = 2;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Step {
	#[default]
	Idle,
	Discovering,
	Capabilities,
	Configuring,
	Opening,
	Media,
	Starting,
	Streaming,
	Suspending,
}

#[derive(Default)]
pub(super) struct Av {
	pub signalling: Option<u16>,
	pub media: Option<u16>,
	pub avctp: Option<u16>,
	pub label: u8,
	pub step: Step,
	pub remote_seid: u8,
	// What the headset heard of the stream.
	pub heard: u32,
	pub bad_crc: u32,
	pub energy: [u64; 8],
	pub header: Option<bt_sbc::Header>,
	// The headset's absolute volume, and the label its registration is answered on.
	pub volume: u8,
	pub registered: Option<u8>,
	// The phone's stream: until when, from when, how many frames have gone.
	pub stream_until: u64,
	pub stream_from: u64,
	pub sent: u64,
	pub sequence: u16,
	pub phase: u32,
}

fn message(label: u8, kind: u8, signal: u8, params: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![(label << 4) | kind, signal];
	out.extend_from_slice(params);
	out
}

// The headset's capabilities: transport, SBC at 48 and 44.1 kHz in every mode, block length, subband count and
// allocation, bitpool 2 to 53, and delay reporting.
const HEADSET_CAPABILITIES: [u8; 12] = [0x01, 0x00, 0x07, 0x06, 0x00, 0x00, 0x3f, 0xff, 2, 53, 0x08, 0x00];

impl World {
	fn is_sink(&self, at: usize) -> bool {
		self.devices[at].spec.uuids.contains(&0x110b)
	}

	fn av(&mut self, at: usize) -> Option<&mut Av> {
		self.devices[at].link.as_mut().map(|link| &mut link.av)
	}

	fn av_label(&mut self, at: usize) -> u8 {
		let Some(av) = self.av(at) else { return 0 };
		let label = av.label;
		av.label = (av.label + 1) & 0x0f;
		label
	}

	// AN AVDTP OR AVCTP CHANNEL IS UP: the host's first AVDTP channel is signalling and its second the media; the phone's
	// own are the stream it is setting up.
	pub(super) fn av_opened(&mut self, at: usize, local: u16, psm: u16, purpose: Purpose) -> Vec<Out> {
		let mut out = Vec::new();
		let name = self.short(at);
		let Some(av) = self.av(at) else { return out };
		if psm == AVCTP {
			av.avctp = Some(local);
			self.say(format!("{name} remote control channel open"));
			return out;
		}
		match purpose {
			Purpose::Avdtp => {
				av.signalling = Some(local);
				av.step = Step::Discovering;
				let label = self.av_label(at);
				out.extend(self.av_send(at, local, &message(label, COMMAND, DISCOVER, &[])));
			}
			Purpose::AvdtpMedia => {
				av.media = Some(local);
				av.step = Step::Starting;
				let seid = av.remote_seid;
				let signalling = av.signalling;
				let label = self.av_label(at);
				if let Some(signalling) = signalling {
					out.extend(self.av_send(at, signalling, &message(label, COMMAND, START, &[seid << 2])));
				}
			}
			_ if av.signalling.is_none() => av.signalling = Some(local),
			_ => {
				av.media = Some(local);
				self.say(format!("{name} media channel open"));
			}
		}
		out
	}

	fn av_send(&self, at: usize, local: u16, payload: &[u8]) -> Option<Out> {
		let remote = self.remote_of(at, local)?;
		self.send_pdu(at, remote, payload)
	}

	pub(super) fn av_data(&mut self, at: usize, local: u16, psm: u16, payload: &[u8]) -> Vec<Out> {
		let Some(av) = self.devices[at].link.as_ref().map(|link| &link.av) else { return Vec::new() };
		if psm == AVCTP {
			return self.avctp(at, local, payload);
		}
		if av.media == Some(local) {
			self.media(at, payload);
			return Vec::new();
		}
		if av.signalling == Some(local) {
			return self.avdtp(at, local, payload);
		}
		Vec::new()
	}

	// ------------------------------------------------------------------ AVDTP

	fn avdtp(&mut self, at: usize, local: u16, payload: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		if payload.len() < 2 {
			return out;
		}
		let (label, kind, signal) = (payload[0] >> 4, payload[0] & 3, payload[1] & 0x3f);
		let params = &payload[2..];
		let name = self.short(at);
		match kind {
			COMMAND => {
				let reply = match signal {
					// THE HEADSET'S ONE SINK; the phone's one source.
					DISCOVER => {
						let role = if self.is_sink(at) { 0x08 } else { 0x00 };
						message(label, ACCEPT, DISCOVER, &[1 << 2, role])
					}
					GET_CAPABILITIES | GET_ALL_CAPABILITIES => message(label, ACCEPT, signal, &HEADSET_CAPABILITIES),
					SET_CONFIGURATION => {
						let config = params.get(2..).unwrap_or(&[]);
						let codec = config.windows(2).position(|pair| pair == [0x07, 0x06]).map(|at| &config[at + 4..at + 8]);
						let readable = codec.is_some_and(|bytes| bytes[0].count_ones() == 2 && bytes[1].count_ones() == 3 && bytes[0] & 0x30 != 0);
						if !readable {
							message(label, REJECT, SET_CONFIGURATION, &[0x07, 0x29])
						} else {
							if let Some(av) = self.av(at) {
								av.step = Step::Configuring;
								av.remote_seid = params.get(1).map_or(0, |byte| byte >> 2);
								av.heard = 0;
								av.bad_crc = 0;
								av.energy = [0; 8];
								av.header = None;
							}
							self.say(format!("{name} was configured for SBC"));
							message(label, ACCEPT, SET_CONFIGURATION, &[])
						}
					}
					OPEN => message(label, ACCEPT, OPEN, &[]),
					START => {
						if let Some(av) = self.av(at) {
							av.step = Step::Streaming;
						}
						self.say(format!("{name} started its stream"));
						message(label, ACCEPT, START, &[])
					}
					SUSPEND | CLOSE => {
						out.extend(self.av_send(at, local, &message(label, ACCEPT, signal, &[])));
						self.heard_report(at);
						if let Some(av) = self.av(at) {
							av.step = Step::Idle;
						}
						return out;
					}
					DELAY_REPORT => message(label, ACCEPT, DELAY_REPORT, &[]),
					_ => message(label, 1, signal, &[]),
				};
				out.extend(self.av_send(at, local, &reply));
				// A CONFIGURED SINK REPORTS ITS DELAY to the source.
				if signal == SET_CONFIGURATION && self.is_sink(at) && reply[0] & 3 == ACCEPT {
					let seid = params.first().map_or(1, |byte| byte >> 2);
					let label = self.av_label(at);
					let mut report = alloc::vec![seid << 2];
					report.extend_from_slice(&HEADSET_DELAY.to_be_bytes());
					out.extend(self.av_send(at, local, &message(label, COMMAND, DELAY_REPORT, &report)));
					self.say(format!("{name} reported a delay of {} ms", HEADSET_DELAY / 10));
				}
			}
			ACCEPT => out.extend(self.phone_next(at, local, signal, params)),
			_ => self.say(format!("{name}'s AVDTP request {signal:#04x} was refused")),
		}
		out
	}

	// THE PHONE SETTING ITS STREAM UP, a step for each acceptance.
	fn phone_next(&mut self, at: usize, local: u16, signal: u8, params: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		match signal {
			DISCOVER => {
				// The host's free audio sink.
				let sink = params.chunks_exact(2).find(|pair| pair[1] & 0x08 != 0 && pair[1] >> 4 == 0 && pair[0] & 2 == 0).map(|pair| pair[0] >> 2);
				let Some(seid) = sink else {
					self.say(format!("{} found no sink on the host", self.short(at)));
					return out;
				};
				if let Some(av) = self.av(at) {
					av.remote_seid = seid;
					av.step = Step::Capabilities;
				}
				let label = self.av_label(at);
				out.extend(self.av_send(at, local, &message(label, COMMAND, GET_CAPABILITIES, &[seid << 2])));
			}
			GET_CAPABILITIES => {
				let Some(seid) = self.av(at).map(|av| av.remote_seid) else { return out };
				// 44.1 kHz joint stereo, sixteen blocks, eight subbands, loudness, bitpool 35.
				let config = [seid << 2, 1 << 2, 0x01, 0x00, 0x07, 0x06, 0x00, 0x00, 0x21, 0x15, 35, 35];
				if let Some(av) = self.av(at) {
					av.step = Step::Configuring;
				}
				let label = self.av_label(at);
				out.extend(self.av_send(at, local, &message(label, COMMAND, SET_CONFIGURATION, &config)));
			}
			SET_CONFIGURATION => {
				let Some(seid) = self.av(at).map(|av| av.remote_seid) else { return out };
				if let Some(av) = self.av(at) {
					av.step = Step::Opening;
				}
				let label = self.av_label(at);
				out.extend(self.av_send(at, local, &message(label, COMMAND, OPEN, &[seid << 2])));
			}
			OPEN => {
				if let Some(av) = self.av(at) {
					av.step = Step::Media;
				}
				out.extend(self.open_channel(at, AVDTP, Purpose::AvdtpMedia).unwrap_or_default());
			}
			START => {
				let now = self.now;
				if let Some(av) = self.av(at) {
					av.step = Step::Streaming;
					av.stream_from = now;
					av.sent = 0;
				}
				self.say(format!("{} is streaming to the host", self.short(at)));
			}
			SUSPEND => {
				let sent = self.av(at).map_or(0, |av| av.sent);
				self.say(format!("{} stopped streaming after {sent} SBC frames", self.short(at)));
				if let Some(av) = self.av(at) {
					av.step = Step::Idle;
				}
			}
			_ => {}
		}
		out
	}

	// ONE MEDIA PACKET THE HEADSET RECEIVED: every SBC frame in it read and judged.
	fn media(&mut self, at: usize, packet: &[u8]) {
		if packet.len() < 13 || packet[0] >> 6 != 2 {
			return;
		}
		let count = packet[12] & 0x0f;
		let mut frames = &packet[13..];
		let Some(av) = self.av(at) else { return };
		for _ in 0..count {
			let Some(heard) = bt_sbc::hear(frames) else { break };
			av.heard += 1;
			if !heard.crc_good {
				av.bad_crc += 1;
			}
			for (total, energy) in av.energy.iter_mut().zip(heard.energy.iter()) {
				*total = total.saturating_add(*energy);
			}
			av.header = Some(heard.header);
			frames = &frames[heard.length.min(frames.len())..];
		}
	}

	// WHAT THE HEADSET HEARD, said when its stream is suspended.
	fn heard_report(&mut self, at: usize) {
		let name = self.short(at);
		let Some(av) = self.devices[at].link.as_ref().map(|link| &link.av) else { return };
		let Some(header) = av.header else {
			self.say(format!("{name} heard nothing"));
			return;
		};
		let loudest = (0..header.subbands).max_by_key(|&sb| av.energy[sb]).unwrap_or(0);
		let crc = if av.bad_crc == 0 { "every CRC good".into() } else { format!("{} bad CRCs", av.bad_crc) };
		let line = format!("{name} heard {} SBC frames: {} Hz {}, {} subbands, bitpool {}, {crc}, the loudest subband {loudest}", av.heard, header.frequency, header.mode_name(), header.subbands, header.bitpool);
		self.say(line);
	}

	// ------------------------------------------------------------------ the phone's stream

	// THE PHONE STREAMS TO THE HOST for `seconds`, or stops: AVDTP set up from its side.
	pub(super) fn phone_stream(&mut self, at: usize, seconds: u32) -> Result<Vec<Out>, &'static str> {
		let now = self.now;
		let link = self.devices[at].link.as_mut().ok_or("no link")?;
		if seconds == 0 {
			let av = &mut link.av;
			let (signalling, seid) = (av.signalling, av.remote_seid);
			av.stream_until = now;
			av.step = Step::Suspending;
			let label = self.av_label(at);
			return Ok(signalling.and_then(|local| self.av_send(at, local, &message(label, COMMAND, SUSPEND, &[seid << 2]))).into_iter().collect());
		}
		link.av.stream_until = now + u64::from(seconds) * 100;
		if link.av.step == Step::Streaming {
			return Ok(Vec::new());
		}
		match link.av.signalling {
			Some(local) => {
				let label = self.av_label(at);
				Ok(self.av_send(at, local, &message(label, COMMAND, DISCOVER, &[])).into_iter().collect())
			}
			None => self.open_channel(at, AVDTP, Purpose::Avdtp).ok_or("no link"),
		}
	}

	// THE PHONE'S CLOCK: as many frames as have come due since it started - 128 samples at 44.1 kHz each - in packets
	// of five, and the stream suspended when its time is up.
	pub(super) fn phone_tick(&mut self, now: u64) -> Vec<Out> {
		let mut out = Vec::new();
		for at in 0..self.devices.len() {
			let Some(av) = self.devices[at].link.as_ref().map(|link| &link.av) else { continue };
			if av.step != Step::Streaming || av.media.is_none() || self.is_sink(at) {
				continue;
			}
			if now >= av.stream_until {
				let (signalling, seid) = (av.signalling, av.remote_seid);
				let label = self.av_label(at);
				if let Some(av) = self.av(at) {
					av.step = Step::Suspending;
				}
				out.extend(signalling.and_then(|local| self.av_send(at, local, &message(label, COMMAND, SUSPEND, &[seid << 2]))));
				continue;
			}
			let due = (now - av.stream_from) * 44_100 / 100 / 128;
			let media = av.media.unwrap_or(0);
			while self.av(at).is_some_and(|av| av.sent < due) {
				let Some(av) = self.av(at) else { break };
				let mut packet = alloc::vec![0x80, 96];
				packet.extend_from_slice(&av.sequence.to_be_bytes());
				packet.extend_from_slice(&((av.sent * 128) as u32).to_be_bytes());
				packet.extend_from_slice(&0x5048_4f4eu32.to_be_bytes());
				packet.push(5);
				let mut frame = [0u8; 128];
				for _ in 0..5 {
					let length = bt_sbc::tone_frame(PHONE_SUBBAND, 12, &mut av.phase, &mut frame);
					packet.extend_from_slice(&frame[..length]);
				}
				av.sequence = av.sequence.wrapping_add(1);
				av.sent += 5;
				out.extend(self.av_send(at, media, &packet));
			}
		}
		out
	}

	pub fn streaming(&self) -> bool {
		self.devices.iter().any(|device| device.link.as_ref().is_some_and(|link| link.av.step == Step::Streaming && !device.spec.uuids.contains(&0x110b)))
	}

	// ------------------------------------------------------------------ AVCTP

	fn avctp(&mut self, at: usize, local: u16, payload: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		if payload.len() < 6 || u16::from_be_bytes([payload[1], payload[2]]) != 0x110e {
			return out;
		}
		let (label, response) = (payload[0] >> 4, payload[0] & 2 != 0);
		let (ctype, opcode, operands) = (payload[3] & 0x0f, payload[5], &payload[6..]);
		let name = self.short(at);
		let answer = |ctype: u8, operands: &[u8]| {
			let mut frame = alloc::vec![(label << 4) | 2, 0x11, 0x0e, ctype, 0x48, opcode];
			frame.extend_from_slice(operands);
			frame
		};
		if response {
			// THE HOST'S ANSWER TO THE HEADSET'S PLAY.
			if opcode == 0x7c && operands.first().is_some_and(|operation| operation & 0x80 == 0) {
				let what = if ctype == 0x08 { String::from("answered not implemented") } else { format!("answered {ctype:#04x}") };
				self.say(format!("{name}'s play was {what}"));
			}
			return out;
		}
		if opcode == 0x7c {
			// A BUTTON THE HOST PRESSED: the phone takes it.
			let operation = operands.first().copied().unwrap_or(0);
			if operation & 0x80 == 0 {
				let word = match operation & 0x7f {
					0x44 => "play",
					0x46 => "pause",
					0x4b => "next",
					0x4c => "previous",
					_ => "an unknown button",
				};
				self.say(format!("{name} was told {word}"));
			}
			out.extend(self.av_send(at, local, &answer(0x09, operands)));
			return out;
		}
		if opcode != 0x00 || operands.len() < 7 || operands[..3] != [0x00, 0x19, 0x58] {
			out.extend(self.av_send(at, local, &answer(0x08, operands)));
			return out;
		}
		let (pdu, parameters) = (operands[3], &operands[7..]);
		let vendor = |pdu: u8, params: &[u8]| {
			let mut operands = alloc::vec![0x00, 0x19, 0x58, pdu, 0];
			operands.extend_from_slice(&(params.len() as u16).to_be_bytes());
			operands.extend_from_slice(params);
			operands
		};
		let volume = self.av(at).map_or(0x40, |av| av.volume);
		let reply = match pdu {
			// THE EVENTS IT REPORTS: the volume.
			0x10 => answer(0x0c, &vendor(0x10, &[0x03, 1, 0x0d])),
			0x31 if parameters.first() == Some(&0x0d) => {
				if let Some(av) = self.av(at) {
					av.registered = Some(label);
				}
				answer(0x0f, &vendor(0x31, &[0x0d, volume]))
			}
			0x50 if !parameters.is_empty() => {
				let level = parameters[0] & 0x7f;
				if let Some(av) = self.av(at) {
					av.volume = level;
				}
				self.say(format!("{name}'s volume was set to {level}"));
				answer(0x09, &vendor(0x50, &[level]))
			}
			_ => answer(0x08, operands),
		};
		out.extend(self.av_send(at, local, &reply));
		out
	}

	// THE HEADSET'S OWN LEVEL CHANGES, and a controller registered for it is told.
	pub(super) fn headset_volume(&mut self, at: usize, level: u8) -> Result<Vec<Out>, &'static str> {
		let name = self.short(at);
		let av = self.av(at).ok_or("no link")?;
		av.volume = level & 0x7f;
		let (Some(local), Some(label)) = (av.avctp, av.registered.take()) else { return Err("nothing registered for the volume") };
		let mut frame = alloc::vec![(label << 4) | 2, 0x11, 0x0e, 0x0d, 0x48, 0x00, 0x00, 0x19, 0x58, 0x31, 0, 0, 2, 0x0d, level & 0x7f];
		frame.truncate(15);
		self.say(format!("{name} changed its own volume to {}", level & 0x7f));
		Ok(self.av_send(at, local, &frame).into_iter().collect())
	}

	// THE HEADSET PRESSES PLAY, toward the host.
	pub(super) fn headset_play(&mut self, at: usize) -> Result<Vec<Out>, &'static str> {
		let local = self.av(at).and_then(|av| av.avctp).ok_or("the remote control is not connected")?;
		let label = self.av_label(at);
		let mut out = Vec::new();
		for operation in [0x44u8, 0xc4] {
			out.extend(self.av_send(at, local, &[label << 4, 0x11, 0x0e, 0x00, 0x48, 0x7c, operation, 0]));
		}
		Ok(out)
	}
}
