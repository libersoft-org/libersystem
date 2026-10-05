// MUSIC: A2DP OVER AVDTP AND THE REMOTE CONTROL OVER AVCTP, on a BR/EDR link.
//
// TWO ROLES, ONE STREAM A LINK. This host is an A2DP SOURCE to headphones and speakers - it opens AVDTP, discovers the
// device's sink, configures SBC at the best both support, and streams what AudioService plays to the output endpoint
// it offers - and an A2DP SINK to a phone that streams to it, offered to AudioService as the route a phone's music
// takes to the default output and never as an input. The stream's SEP numbers are ours: 1 the source, 2 the sink.
//
// THE CLOCK. An A2DP sink gives the host no clock it can see, so the output endpoint's periods are acknowledged by the
// host's own timer at the stream's rate (`audio_routing::TimerPacer`), a little ahead so the sink's buffer stays fed -
// AudioService is paced exactly as a sound card's acknowledgments pace it. A phone's stream arrives on the phone's
// clock and is read as capture is; AudioService's jitter buffer absorbs the difference.
//
// THE REMOTE CONTROL. As controller, play, pause, next and previous toward a phone are the operator's (`btctl
// media`). As target, a headset's buttons are answered NOT IMPLEMENTED - there is no media session to act on - and
// the record claims no player category. The ABSOLUTE VOLUME goes both ways: a headset that registers it is sent the
// level AudioService sets, its own changes come back, and its samples are then not scaled; a phone sets this host's
// level for its route the same way.

use super::*;
use proto::system::{AudioEndpoint, AudioEndpointKind};
use service_logic::audio_routing::TimerPacer;
use service_logic::avdtp::{self, Kind, Message, Role, Sbc, signal};
use service_logic::avrcp::{self, Frame, Operation, ctype, pdu};
use service_logic::l2cap_bredr::psm;
use service_logic::sbc;

// This host's stream end-points: the SBC source and the SBC sink.
const OUR_SOURCE: u8 = 1;
const OUR_SINK: u8 = 2;
// How far ahead of the timer an output endpoint's periods are acknowledged: twenty milliseconds of the sink's buffer.
const LEAD_NS: u64 = 20_000_000;
// The latency stated for a sink that reports no delay of its own: what headphones typically buffer.
const DEFAULT_SINK_DELAY_US: u32 = 150_000;
// A phone's decoded audio held for AudioService, at most: two hundred milliseconds, the oldest dropped.
const ROUTE_HOLD_MS: usize = 200;
// SBC frames waiting for their stream to start, at most.
const STARTING_FRAMES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum State {
	#[default]
	Idle,
	Discovering,
	Capabilities,
	Configuring,
	Opening,
	Open,
	Starting,
	Streaming,
	Suspending,
}

// What the remote control has found out about the peer's absolute volume.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Volume {
	// Not asked yet, or being asked.
	#[default]
	Unknown,
	// The peer controls its own level, at this level (0 to 100).
	Device(u8),
	// It does not: AudioService scales.
	None,
}

#[derive(Default)]
pub(crate) struct A2dp {
	pub signalling: Option<u16>,
	pub media: Option<u16>,
	// This host opened the signalling channel: it is the source, and the initiator.
	pub outgoing: bool,
	pub state: State,
	pub label: u8,
	pub ours: u8,
	pub theirs: u8,
	pub config: Option<sbc::Config>,
	// What the peer's capabilities offered, while this host configures.
	pub offered_delay_reporting: bool,
	// The sink's own delay, in tenths of a millisecond, where it reported one.
	pub delay: Option<u16>,
	// The endpoint offered to AudioService for this stream.
	pub endpoint: Option<u32>,
	pub encoder: Option<Box<sbc::Encoder>>,
	pub decoder: Option<Box<sbc::Decoder>>,
	// PCM waiting to fill an SBC frame, SBC frames waiting to fill a media packet.
	pub pcm: Vec<i16>,
	pub frames: Vec<u8>,
	pub frame_count: u8,
	pub sequence: u16,
	pub timestamp: u32,
	// A phone's decoded audio, interleaved, for the route endpoint.
	pub route: VecDeque<i16>,
	pub dropped: u64,
	// THE REMOTE CONTROL: its channel, its next label, the peer's absolute volume, and a command waiting for the
	// channel to open.
	pub avctp: Option<u16>,
	pub avctp_outgoing: bool,
	pub avrcp_label: u8,
	pub volume: Volume,
	pub waiting_command: Option<Operation>,
	// The level this host's route answers a phone's volume with, 0 to 100.
	pub route_volume: u8,
}

impl A2dp {
	fn next_label(&mut self) -> u8 {
		let label = self.label;
		self.label = (self.label + 1) & 0x0f;
		label
	}

	fn next_avrcp_label(&mut self) -> u8 {
		let label = self.avrcp_label;
		self.avrcp_label = (self.avrcp_label + 1) & 0x0f;
		label
	}
}

// What our end-points offer, by SEP.
fn capabilities_of(seid: u8) -> Option<Vec<u8>> {
	(seid == OUR_SOURCE || seid == OUR_SINK).then(|| avdtp::capability_list(&Sbc::OFFERED, true))
}

impl Stack {
	fn a2dp(&mut self, at: usize, handle: u16) -> Option<&mut A2dp> {
		self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).map(|classic| &mut classic.a2dp)
	}

	fn avdtp_send(&mut self, at: usize, handle: u16, message: &Message) {
		let Some(cid) = self.a2dp(at, handle).and_then(|a2dp| a2dp.signalling) else { return };
		self.send_on(at, handle, cid, &message.encode());
	}

	fn avdtp_command(&mut self, at: usize, handle: u16, signal: u8, params: &[u8]) {
		let Some(label) = self.a2dp(at, handle).map(A2dp::next_label) else { return };
		self.avdtp_send(at, handle, &Message::command(label, signal, params));
	}

	// THE OPERATOR'S CONNECT FOR AUDIO, once the sink's record is found: the signalling channel, from this side.
	pub(crate) fn a2dp_connect(&mut self, at: usize, handle: u16) {
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		if classic.a2dp.signalling.is_some() {
			return;
		}
		classic.a2dp.outgoing = true;
		let Some((_, signal)) = classic.channels.open(psm::AVDTP, false) else {
			print(b"BluetoothService: no L2CAP channel left on the link for AVDTP\n");
			return;
		};
		self.send_signal(at, handle, &signal);
	}

	// AN AVDTP OR AVCTP CHANNEL IS UP: the first AVDTP one is signalling and the second the media; AVCTP is the remote
	// control's.
	pub(crate) fn a2dp_channel_opened(&mut self, at: usize, handle: u16, cid: u16, channel_psm: u16) {
		let Some(a2dp) = self.a2dp(at, handle) else { return };
		if channel_psm == psm::AVCTP {
			a2dp.avctp = Some(cid);
			if a2dp.avctp_outgoing {
				self.avrcp_ask_volume(at, handle);
			}
			if let Some(operation) = self.a2dp(at, handle).and_then(|a2dp| a2dp.waiting_command.take()) {
				self.avrcp_press(at, handle, operation);
			}
			return;
		}
		if a2dp.signalling.is_none() {
			a2dp.signalling = Some(cid);
			if a2dp.outgoing {
				a2dp.state = State::Discovering;
				self.avdtp_command(at, handle, signal::DISCOVER, &[]);
			}
			return;
		}
		a2dp.media = Some(cid);
		a2dp.state = State::Open;
		if a2dp.outgoing {
			// THE REMOTE CONTROL'S VOLUME IS ASKED BEFORE THE OUTPUT IS OFFERED: whether the device levels itself is
			// part of the offer.
			a2dp.avctp_outgoing = true;
			match a2dp.avctp {
				Some(_) => self.avrcp_ask_volume(at, handle),
				None => {
					let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
					match classic.channels.open(psm::AVCTP, false) {
						Some((_, signal)) => self.send_signal(at, handle, &signal),
						None => {
							classic.a2dp.volume = Volume::None;
							self.a2dp_offer(at, handle);
						}
					}
				}
			}
		} else {
			self.a2dp_offer(at, handle);
		}
	}

	// THE STREAM'S ENDPOINT, offered to AudioService: an output for a sink this host streams to, the route for a phone
	// streaming here.
	fn a2dp_offer(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link(handle) else { return };
		let peer = link.peer;
		let Some(a2dp) = link.classic.as_ref().map(|classic| &classic.a2dp) else { return };
		if a2dp.endpoint.is_some() {
			return;
		}
		let Some(config) = a2dp.config else { return };
		let outgoing = a2dp.outgoing;
		// THE LATENCY STATED: the sink's own delay where it reported one, otherwise what headphones typically hold, and a
		// packet's worth of SBC frames on the way.
		let frame_us = (config.samples() as u64 * 1_000_000 / u64::from(config.frequency)) as u32;
		let latency_us = if outgoing { a2dp.delay.map_or(DEFAULT_SINK_DELAY_US, |tenths| u32::from(tenths) * 100) + 4 * frame_us } else { 4 * frame_us };
		let (hardware_volume, volume) = match a2dp.volume {
			Volume::Device(level) if outgoing => (true, level),
			_ => (false, 100),
		};
		let name = self.controllers[at].bredr_state.name_of(&peer).unwrap_or_default();
		let endpoint = AudioEndpoint { id: 0, peer: peer_to_wire(&peer), name, kind: if outgoing { AudioEndpointKind::Output } else { AudioEndpointKind::Route }, rate: config.frequency, channels: config.mode.channels() as u8, latency_us, hardware_volume, volume };
		let Some(id) = self.audio.offer(endpoint, at, handle) else { return };
		if let Some(a2dp) = self.a2dp(at, handle) {
			a2dp.endpoint = Some(id);
		}
		print(b"BluetoothService: an A2DP stream is offered to AudioService\n");
	}

	pub(crate) fn a2dp_data(&mut self, at: usize, handle: u16, cid: u16, payload: &[u8]) {
		let Some(a2dp) = self.a2dp(at, handle) else { return };
		if a2dp.signalling == Some(cid) {
			self.avdtp_signal(at, handle, payload);
		} else if a2dp.media == Some(cid) {
			self.avdtp_media(at, handle, payload);
		} else if a2dp.avctp == Some(cid) {
			self.avctp_frame(at, handle, payload);
		}
	}

	pub(crate) fn a2dp_channel_closed(&mut self, at: usize, handle: u16, cid: u16) {
		let Some(a2dp) = self.a2dp(at, handle) else { return };
		if a2dp.avctp == Some(cid) {
			a2dp.avctp = None;
			return;
		}
		if a2dp.signalling == Some(cid) || a2dp.media == Some(cid) {
			// THE STREAM IS OVER: its endpoint leaves AudioService, and the other channel goes with it.
			let other = if a2dp.signalling == Some(cid) { a2dp.media } else { a2dp.signalling };
			let endpoint = a2dp.endpoint.take();
			let avctp = a2dp.avctp;
			*a2dp = A2dp { avctp, ..A2dp::default() };
			if let Some(id) = endpoint {
				self.audio.withdraw(id);
			}
			if let Some(other) = other {
				self.close_channel(at, handle, other);
			}
		}
	}

	// The link is gone: its stream's endpoint leaves.
	pub(crate) fn a2dp_gone(&mut self, link: &mut Link) {
		if let Some(id) = link.classic.as_mut().and_then(|classic| classic.a2dp.endpoint.take()) {
			self.audio.withdraw(id);
		}
	}

	// ------------------------------------------------------------------ signalling

	fn avdtp_signal(&mut self, at: usize, handle: u16, payload: &[u8]) {
		let Some(message) = Message::decode(payload) else {
			// A fragmented or truncated message is answered not understood.
			let label = payload.first().map_or(0, |header| header >> 4);
			self.avdtp_send(at, handle, &Message { label, kind: Kind::GeneralReject, signal: payload.get(1).copied().unwrap_or(0), params: Vec::new() });
			return;
		};
		match message.kind {
			Kind::Command => self.avdtp_command_from_peer(at, handle, message),
			Kind::Accept => self.avdtp_accepted(at, handle, message),
			Kind::Reject | Kind::GeneralReject => {
				print(b"BluetoothService: the peer refused an AVDTP request; the stream is abandoned\n");
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.state = State::Idle;
				}
				if let Some(cid) = self.a2dp(at, handle).and_then(|a2dp| a2dp.signalling) {
					self.close_channel(at, handle, cid);
				}
			}
		}
	}

	// A COMMAND FROM THE PEER: this host's end-points discovered and configured by a phone, or a sink's delay report.
	fn avdtp_command_from_peer(&mut self, at: usize, handle: u16, message: Message) {
		let label = message.label;
		let seid = message.params.first().map(|byte| byte >> 2).unwrap_or(0);
		let busy = self.a2dp(at, handle).is_some_and(|a2dp| a2dp.config.is_some());
		let reply = match message.signal {
			signal::DISCOVER => {
				let mut params = Vec::new();
				params.extend_from_slice(&avdtp::Endpoint { seid: OUR_SOURCE, in_use: busy, media: 0, role: Role::Source }.encode());
				params.extend_from_slice(&avdtp::Endpoint { seid: OUR_SINK, in_use: busy, media: 0, role: Role::Sink }.encode());
				Message::accept(label, signal::DISCOVER, &params)
			}
			signal::GET_CAPABILITIES | signal::GET_ALL_CAPABILITIES => match capabilities_of(seid) {
				Some(list) => Message::accept(label, message.signal, &list),
				None => Message::reject(label, message.signal, &[avdtp::error::BAD_ACP_SEID]),
			},
			signal::SET_CONFIGURATION => {
				let theirs = message.params.get(1).map_or(0, |byte| byte >> 2);
				let config = avdtp::capabilities(message.params.get(2..).unwrap_or(&[])).and_then(|caps| caps.sbc).and_then(|sbc| sbc.config());
				match (capabilities_of(seid), config, busy) {
					(None, _, _) => Message::reject(label, signal::SET_CONFIGURATION, &[0, avdtp::error::BAD_ACP_SEID]),
					(_, _, true) => Message::reject(label, signal::SET_CONFIGURATION, &[0, avdtp::error::SEP_IN_USE]),
					(_, None, _) => Message::reject(label, signal::SET_CONFIGURATION, &[avdtp::category::MEDIA_CODEC, avdtp::error::UNSUPPORTED_CONFIGURATION]),
					(Some(_), Some(config), false) => {
						if let Some(a2dp) = self.a2dp(at, handle) {
							// A PHONE CONFIGURES OUR SINK: this host decodes; one that configures our source asks
							// this host to stream to it - which it does on the phone's word as on the operator's.
							a2dp.ours = seid;
							a2dp.theirs = theirs;
							a2dp.config = Some(config);
							a2dp.outgoing = seid == OUR_SOURCE;
							a2dp.state = State::Configuring;
							a2dp.route_volume = 100;
						}
						Message::accept(label, signal::SET_CONFIGURATION, &[])
					}
				}
			}
			signal::GET_CONFIGURATION => match self.a2dp(at, handle).and_then(|a2dp| a2dp.config) {
				Some(config) => Message::accept(label, signal::GET_CONFIGURATION, &avdtp::capability_list(&Sbc::of(&config), false)),
				None => Message::reject(label, signal::GET_CONFIGURATION, &[avdtp::error::BAD_STATE]),
			},
			signal::OPEN => {
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.state = State::Opening;
				}
				Message::accept(label, signal::OPEN, &[])
			}
			signal::START => {
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.state = State::Streaming;
					if !a2dp.outgoing {
						a2dp.decoder = Some(Box::new(sbc::Decoder::new()));
					}
				}
				Message::accept(label, signal::START, &[])
			}
			signal::SUSPEND => {
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.state = State::Open;
				}
				Message::accept(label, signal::SUSPEND, &[])
			}
			signal::CLOSE | signal::ABORT => {
				let media = self.a2dp(at, handle).and_then(|a2dp| a2dp.media);
				self.avdtp_send(at, handle, &Message::accept(label, message.signal, &[]));
				// The media channel's close is the stream's end: `a2dp_channel_closed` lets the endpoint go.
				if let Some(media) = media {
					self.close_channel(at, handle, media);
				} else if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.config = None;
					a2dp.state = State::Idle;
				}
				return;
			}
			// THE SINK'S DELAY, in tenths of a millisecond: part of the latency this host states for the output.
			signal::DELAY_REPORT => {
				if let (Some(a2dp), Some(bytes)) = (self.a2dp(at, handle), message.params.get(1..3)) {
					a2dp.delay = Some(u16::from_be_bytes([bytes[0], bytes[1]]));
				}
				Message::accept(label, signal::DELAY_REPORT, &[])
			}
			other => Message { label, kind: Kind::GeneralReject, signal: other, params: Vec::new() },
		};
		self.avdtp_send(at, handle, &reply);
	}

	// THE PEER ACCEPTED WHAT THIS HOST ASKED: the next step of setting a stream up to a sink.
	fn avdtp_accepted(&mut self, at: usize, handle: u16, message: Message) {
		match message.signal {
			signal::DISCOVER => {
				let sink = avdtp::endpoints(&message.params).into_iter().find(|endpoint| endpoint.role == Role::Sink && endpoint.media == 0 && !endpoint.in_use);
				let Some(sink) = sink else {
					print(b"BluetoothService: the device has no free audio sink\n");
					return;
				};
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.theirs = sink.seid;
					a2dp.state = State::Capabilities;
				}
				self.avdtp_command(at, handle, signal::GET_CAPABILITIES, &[sink.seid << 2]);
			}
			signal::GET_CAPABILITIES | signal::GET_ALL_CAPABILITIES => {
				let caps = avdtp::capabilities(&message.params).unwrap_or_default();
				let Some(config) = caps.sbc.and_then(|sbc| sbc.choose(&Sbc::OFFERED)) else {
					print(b"BluetoothService: the device's sink shares no SBC configuration with this host\n");
					return;
				};
				let Some(a2dp) = self.a2dp(at, handle) else { return };
				a2dp.config = Some(config);
				a2dp.ours = OUR_SOURCE;
				a2dp.offered_delay_reporting = caps.delay_reporting;
				a2dp.state = State::Configuring;
				let mut params = alloc::vec![a2dp.theirs << 2, OUR_SOURCE << 2];
				params.extend_from_slice(&avdtp::capability_list(&Sbc::of(&config), caps.delay_reporting));
				self.avdtp_command(at, handle, signal::SET_CONFIGURATION, &params);
			}
			signal::SET_CONFIGURATION => {
				let Some(theirs) = self.a2dp(at, handle).map(|a2dp| a2dp.theirs) else { return };
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.state = State::Opening;
				}
				self.avdtp_command(at, handle, signal::OPEN, &[theirs << 2]);
			}
			signal::OPEN => {
				// THE MEDIA CHANNEL, the second on the PSM.
				let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
				if let Some((_, signal)) = classic.channels.open(psm::AVDTP, false) {
					self.send_signal(at, handle, &signal);
				}
			}
			signal::START => {
				let Some(a2dp) = self.a2dp(at, handle) else { return };
				a2dp.state = State::Streaming;
				self.a2dp_flush(at, handle);
			}
			signal::SUSPEND => {
				if let Some(a2dp) = self.a2dp(at, handle) {
					a2dp.state = State::Open;
				}
			}
			_ => {}
		}
	}

	// ------------------------------------------------------------------ the media

	// A PHONE'S MEDIA PACKET: decoded, held for the route endpoint at most two hundred milliseconds - the oldest dropped -
	// and a capture waiting on it answered.
	fn avdtp_media(&mut self, at: usize, handle: u16, packet: &[u8]) {
		let Some((_, _, count, mut frames)) = avdtp::read_media(packet) else { return };
		let Some(a2dp) = self.a2dp(at, handle) else { return };
		if a2dp.outgoing || a2dp.state != State::Streaming {
			return;
		}
		let Some(config) = a2dp.config else { return };
		let decoder = a2dp.decoder.get_or_insert_with(|| Box::new(sbc::Decoder::new()));
		let mut pcm = [0i16; 16 * 8 * 2];
		for _ in 0..count {
			let Ok((read, samples)) = decoder.decode(frames, &mut pcm) else { break };
			if read != config {
				break;
			}
			let length = read.frame_length();
			let values = &pcm[..samples * read.mode.channels()];
			a2dp.route.extend(values.iter().copied());
			frames = &frames[length.min(frames.len())..];
		}
		let bound = config.frequency as usize * config.mode.channels() * ROUTE_HOLD_MS / 1000;
		while a2dp.route.len() > bound {
			a2dp.route.pop_front();
			a2dp.dropped += 1;
		}
		let endpoint = a2dp.endpoint;
		if let Some(id) = endpoint {
			self.audio_capture_ready(id);
		}
	}

	// PCM AUDIOSERVICE PLAYED ON AN OUTPUT: SBC frames from it, gathered into media packets, sent while the stream
	// streams - and the stream started by the first of them.
	pub(crate) fn a2dp_play(&mut self, at: usize, handle: u16, samples: &[i16]) {
		let Some(a2dp) = self.a2dp(at, handle) else { return };
		let Some(config) = a2dp.config else { return };
		if a2dp.encoder.is_none() {
			a2dp.encoder = sbc::Encoder::new(config).map(Box::new);
		}
		a2dp.pcm.extend_from_slice(samples);
		let per_frame = config.samples() * config.mode.channels();
		let mut frame = [0u8; sbc::MAX_FRAME_BYTES];
		while a2dp.pcm.len() >= per_frame {
			let Some(encoder) = a2dp.encoder.as_mut() else { break };
			let length = encoder.encode(&a2dp.pcm[..per_frame], &mut frame);
			a2dp.pcm.drain(..per_frame);
			if a2dp.frames.len() < STARTING_FRAMES * length {
				a2dp.frames.extend_from_slice(&frame[..length]);
				a2dp.frame_count += 1;
			}
		}
		match a2dp.state {
			State::Open => {
				a2dp.state = State::Starting;
				let theirs = a2dp.theirs;
				self.avdtp_command(at, handle, signal::START, &[theirs << 2]);
			}
			State::Streaming => self.a2dp_flush(at, handle),
			_ => {}
		}
	}

	// THE FRAMES WAITING, in packets of as many whole frames as the media channel's MTU takes.
	fn a2dp_flush(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link(handle) else { return };
		let Some(classic) = link.classic.as_ref() else { return };
		let Some(media) = classic.a2dp.media else { return };
		let Some(channel) = classic.channels.get(media) else { return };
		let (remote, mtu) = (channel.remote_cid, usize::from(channel.peer_mtu));
		loop {
			let Some(a2dp) = self.a2dp(at, handle) else { return };
			let Some(config) = a2dp.config else { return };
			let length = config.frame_length();
			if a2dp.frame_count == 0 {
				return;
			}
			let count = avdtp::frames_per_packet(mtu, length).min(usize::from(a2dp.frame_count));
			let payload: Vec<u8> = a2dp.frames.drain(..count * length).collect();
			a2dp.frame_count -= count as u8;
			let packet = avdtp::media_packet(a2dp.sequence, a2dp.timestamp, 1, count as u8, &payload);
			a2dp.sequence = a2dp.sequence.wrapping_add(1);
			a2dp.timestamp = a2dp.timestamp.wrapping_add((count * config.samples()) as u32);
			self.controllers[at].l2cap(handle, remote, &packet);
		}
	}

	// THE END OF WHAT AUDIOSERVICE PLAYS: the stream is suspended, so the sink stops expecting packets.
	pub(crate) fn a2dp_stop(&mut self, at: usize, handle: u16) {
		let Some(a2dp) = self.a2dp(at, handle) else { return };
		if matches!(a2dp.state, State::Streaming | State::Starting) {
			a2dp.state = State::Suspending;
			a2dp.pcm.clear();
			a2dp.frames.clear();
			a2dp.frame_count = 0;
			let theirs = a2dp.theirs;
			self.avdtp_command(at, handle, signal::SUSPEND, &[theirs << 2]);
		}
	}

	// ------------------------------------------------------------------ the remote control

	fn avctp_send(&mut self, at: usize, handle: u16, frame: &Frame) {
		let Some(cid) = self.a2dp(at, handle).and_then(|a2dp| a2dp.avctp) else { return };
		self.send_on(at, handle, cid, &frame.encode());
	}

	// WHETHER THE PEER LEVELS ITSELF: its supported events asked for, and the volume registered for if it has it.
	fn avrcp_ask_volume(&mut self, at: usize, handle: u16) {
		let Some(label) = self.a2dp(at, handle).map(A2dp::next_avrcp_label) else { return };
		self.avctp_send(at, handle, &Frame::vendor(label, ctype::STATUS, pdu::GET_CAPABILITIES, &[0x03]));
	}

	fn avrcp_register_volume(&mut self, at: usize, handle: u16) {
		let Some(label) = self.a2dp(at, handle).map(A2dp::next_avrcp_label) else { return };
		self.avctp_send(at, handle, &Frame::vendor(label, ctype::NOTIFY, pdu::REGISTER_NOTIFICATION, &[avrcp::EVENT_VOLUME_CHANGED, 0, 0, 0, 0]));
	}

	// A BUTTON, PRESSED AND LET GO.
	fn avrcp_press(&mut self, at: usize, handle: u16, operation: Operation) {
		for pressed in [true, false] {
			let Some(label) = self.a2dp(at, handle).map(A2dp::next_avrcp_label) else { return };
			self.avctp_send(at, handle, &Frame::pass_through(label, operation, pressed));
		}
	}

	fn avctp_frame(&mut self, at: usize, handle: u16, payload: &[u8]) {
		let Some(frame) = Frame::decode(payload) else {
			// ANOTHER PROFILE'S FRAME, or one too short: a response with the profile marked invalid, where it can be read.
			if payload.len() >= 3 && payload[0] & 2 == 0 {
				let mut answer = payload[..3].to_vec();
				answer[0] |= 0x03;
				if let Some(cid) = self.a2dp(at, handle).and_then(|a2dp| a2dp.avctp) {
					self.send_on(at, handle, cid, &answer);
				}
			}
			return;
		};
		if frame.response {
			self.avrcp_response(at, handle, &frame);
		} else {
			self.avrcp_command(at, handle, &frame);
		}
	}

	// THE PEER'S ANSWER: its events, its volume - in an interim or a change - and a level accepted.
	fn avrcp_response(&mut self, at: usize, handle: u16, frame: &Frame) {
		let Some((pdu_id, params)) = frame.pdu() else { return };
		match pdu_id {
			pdu::GET_CAPABILITIES => {
				// [capability id, count, events...]
				let events = params.get(2..).unwrap_or(&[]);
				if frame.ctype == ctype::STABLE && events.contains(&avrcp::EVENT_VOLUME_CHANGED) {
					self.avrcp_register_volume(at, handle);
				} else {
					if let Some(a2dp) = self.a2dp(at, handle) {
						a2dp.volume = Volume::None;
					}
					self.a2dp_offer(at, handle);
				}
			}
			pdu::REGISTER_NOTIFICATION if params.first() == Some(&avrcp::EVENT_VOLUME_CHANGED) => {
				let level = params.get(1).map_or(100, |volume| avrcp::from_absolute(*volume));
				let Some(a2dp) = self.a2dp(at, handle) else { return };
				a2dp.volume = Volume::Device(level);
				let endpoint = a2dp.endpoint;
				match frame.ctype {
					ctype::INTERIM => self.a2dp_offer(at, handle),
					ctype::CHANGED => {
						// THE DEVICE CHANGED ITS OWN LEVEL: AudioService is told, and the notification asked for again.
						if let Some(id) = endpoint {
							self.audio.volume_changed(id, level);
						}
						self.avrcp_register_volume(at, handle);
					}
					_ => {
						if let Some(a2dp) = self.a2dp(at, handle) {
							a2dp.volume = Volume::None;
						}
						self.a2dp_offer(at, handle);
					}
				}
			}
			_ => {}
		}
	}

	// A COMMAND FROM THE PEER, this host the target: a button answered NOT IMPLEMENTED, the events this host reports,
	// and a phone's absolute volume for the route it streams.
	fn avrcp_command(&mut self, at: usize, handle: u16, frame: &Frame) {
		if frame.opcode == avrcp::opcode::PASS_THROUGH {
			// NO MEDIA SESSION: nothing to play, pause or skip, and the button is not acknowledged as done.
			self.avctp_send(at, handle, &frame.answer(ctype::NOT_IMPLEMENTED, &frame.operands));
			if frame.operation().is_some_and(|(_, pressed)| pressed) {
				print(b"BluetoothService: a remote-control button was answered not implemented - there is no media session\n");
			}
			return;
		}
		let Some((pdu_id, params)) = frame.pdu() else {
			self.avctp_send(at, handle, &frame.answer(ctype::NOT_IMPLEMENTED, &frame.operands));
			return;
		};
		let route_volume = self.a2dp(at, handle).map_or(100, |a2dp| a2dp.route_volume);
		let answer = |pdu_id: u8, ctype: u8, parameters: &[u8]| {
			let mut answer = Frame::vendor(frame.label, ctype, pdu_id, parameters);
			answer.response = true;
			answer
		};
		let reply = match pdu_id {
			pdu::GET_CAPABILITIES => answer(pdu::GET_CAPABILITIES, ctype::STABLE, &[0x03, 1, avrcp::EVENT_VOLUME_CHANGED]),
			pdu::REGISTER_NOTIFICATION if params.first() == Some(&avrcp::EVENT_VOLUME_CHANGED) => answer(pdu::REGISTER_NOTIFICATION, ctype::INTERIM, &[avrcp::EVENT_VOLUME_CHANGED, avrcp::to_absolute(route_volume)]),
			pdu::SET_ABSOLUTE_VOLUME if !params.is_empty() => {
				let level = avrcp::from_absolute(params[0]);
				let endpoint = self.a2dp(at, handle).and_then(|a2dp| {
					a2dp.route_volume = level;
					a2dp.endpoint
				});
				if let Some(id) = endpoint {
					self.audio.volume_changed(id, level);
				}
				answer(pdu::SET_ABSOLUTE_VOLUME, ctype::ACCEPTED, &[avrcp::to_absolute(level)])
			}
			_ => answer(pdu_id, ctype::NOT_IMPLEMENTED, &[]),
		};
		self.avctp_send(at, handle, &reply);
	}

	// THE OPERATOR'S MEDIA COMMAND toward a peer: over the remote control's channel, opened first if it is not.
	pub(crate) fn media_command(&mut self, at: usize, peer: &Peer, operation: Operation) -> Result<(), Error> {
		let Some(handle) = self.controllers[at].link_to(peer).filter(|link| link.encrypted).map(|link| link.handle) else { return Err(Error::Closed) };
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return Err(Error::Unsupported) };
		if classic.a2dp.avctp.is_some() {
			self.avrcp_press(at, handle, operation);
			return Ok(());
		}
		classic.a2dp.waiting_command = Some(operation);
		match classic.channels.open(psm::AVCTP, false) {
			Some((_, signal)) => {
				self.send_signal(at, handle, &signal);
				Ok(())
			}
			None => Err(Error::Exhausted),
		}
	}

	// AUDIOSERVICE SET A LEVEL on an output whose device levels itself: sent as the absolute volume.
	pub(crate) fn a2dp_set_volume(&mut self, at: usize, handle: u16, level: u8) -> Result<(), Error> {
		let Some(a2dp) = self.a2dp(at, handle) else { return Err(Error::NotFound) };
		if !matches!(a2dp.volume, Volume::Device(_)) {
			return Err(Error::Unsupported);
		}
		a2dp.volume = Volume::Device(level);
		let label = a2dp.next_avrcp_label();
		self.avctp_send(at, handle, &Frame::vendor(label, ctype::CONTROL, pdu::SET_ABSOLUTE_VOLUME, &[avrcp::to_absolute(level)]));
		Ok(())
	}

	// ONE PERIOD OF A PHONE'S AUDIO for the route endpoint, if a whole one is held.
	pub(crate) fn a2dp_take_period(&mut self, at: usize, handle: u16, samples: usize) -> Option<Vec<u8>> {
		let a2dp = self.a2dp(at, handle)?;
		if a2dp.route.len() < samples {
			return None;
		}
		let mut out = Vec::with_capacity(samples * 2);
		for sample in a2dp.route.drain(..samples) {
			out.extend_from_slice(&sample.to_le_bytes());
		}
		Some(out)
	}
}

// A pacer for an output stream at `rate`, started now.
pub(crate) fn pacer(rate: u32) -> TimerPacer {
	TimerPacer::new(rate, clock_ns())
}

// WHEN AN OUTPUT'S PERIOD MAY BE ACKNOWLEDGED: once everything accepted before it is due, less the lead - in clock
// ticks, the clock this service waits on.
pub(crate) fn ack_tick(pacer: &TimerPacer) -> u64 {
	let at_ns = pacer.when(0).saturating_sub(LEAD_NS);
	let now_ns = clock_ns();
	let now = clock();
	if at_ns <= now_ns { now } else { now + (at_ns - now_ns).div_ceil(10_000_000) }
}
