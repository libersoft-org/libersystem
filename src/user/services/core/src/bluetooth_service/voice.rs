// CALLS: THE HANDS-FREE PROFILE'S AUDIO GATEWAY OVER RFCOMM, AND THE VOICE LINK OVER SCO.
//
// THIS SYSTEM IS WHAT A HEADSET CONNECTS TO. The headset opens RFCOMM to the gateway's channel - admitted only for a
// peer trusted for voice - and sets the service level connection up (`service_logic::hfp::Gateway`); once it is up,
// and its codec settled, the headset is offered to AudioService as a VOICE endpoint: 16 kHz mono where both sides
// negotiated mSBC, 8 kHz where the link carries CVSD.
//
// THE LINK FOLLOWS THE SESSION. AudioService opens the voice endpoint's channel only while a voice session is open, and
// that is what sets the synchronous link up; closing it takes the link down. A headset that asks for audio with no
// session open is refused, and a synchronous link a headset tries to open itself always is: this gateway sets it up.
//
// ONE OUT FOR ONE IN. The controller delivers a voice packet each interval and the gateway sends one back for each,
// from what AudioService played - silence when it played nothing - so the link's own clock paces both directions;
// AudioService's periods are acknowledged as the packets carry them away. mSBC's framing - its H2 header - is the
// host's, and a whole 60-byte frame is the unit counted; CVSD's coding is the controller's, the host carrying 8 kHz
// linear PCM a packet at a time.

use super::*;
use proto::system::{AudioEndpoint, AudioEndpointKind, BtCallCommand, BtCallState};
use service_logic::audio_routing::OneForOne;
use service_logic::bt_policy;
use service_logic::hci_bredr;
use service_logic::hfp::{self, Call, Command, Event as Hfp, Gateway, Out as HfpOut};
use service_logic::sbc;

// What the playback queue may hold before AudioService's next period is held: two periods of ten milliseconds.
const PLAY_HOLD_MS: usize = 20;
// What the microphone's queue holds for AudioService, at most: a hundred milliseconds, the oldest dropped.
const CAPTURE_HOLD_MS: usize = 100;
// The voice setting: CVSD's 16-bit linear input, and transparent data for mSBC.
const VOICE_CVSD: u16 = 0x0060;
const VOICE_TRANSPARENT: u16 = 0x0063;
// EV3 alone, the EDR types barred: what both of the codecs' settings run on.
const PACKET_TYPES: u16 = 0x0008 | 0x03c0;
// The samples one mSBC frame carries: 7.5 ms at 16 kHz.
const MSBC_SAMPLES: usize = 120;

pub(crate) struct VoiceLink {
	pub channel: u8,
	pub gateway: Gateway,
	pub endpoint: Option<u32>,
	// The synchronous link's handle, and whether it is being set up.
	pub sco: Option<u16>,
	pub setting_up: bool,
	pub clock: OneForOne,
	pub play: VecDeque<i16>,
	pub capture: VecDeque<i16>,
	pub encoder: Option<Box<sbc::Encoder>>,
	pub decoder: Option<Box<sbc::Decoder>>,
	pub sequence: u8,
	// Bytes of mSBC frames that arrived split across the controller's packets.
	pub assembling: Vec<u8>,
	pub battery: Option<u8>,
	// A playback period's acknowledgment waits for the queue to drain.
	pub ack_waiting: bool,
}

impl VoiceLink {
	pub fn new(channel: u8) -> VoiceLink {
		VoiceLink { channel, gateway: Gateway::new(channel == bt_policy::HSP_CHANNEL), endpoint: None, sco: None, setting_up: false, clock: OneForOne::default(), play: VecDeque::new(), capture: VecDeque::new(), encoder: None, decoder: None, sequence: 0, assembling: Vec::new(), battery: None, ack_waiting: false }
	}

	fn msbc(&self) -> bool {
		self.gateway.codec() == hfp::CODEC_MSBC
	}

	fn rate(&self) -> u32 {
		if self.msbc() { 16_000 } else { 8_000 }
	}

	fn play_hold(&self) -> usize {
		self.rate() as usize * PLAY_HOLD_MS / 1000
	}
}

// ONE mSBC FRAME OF WHAT WAS PLAYED - silence where a whole one is not queued - in its H2 header.
fn msbc_packet(voice: &mut VoiceLink, out: &mut Vec<u8>) {
	let mut samples = [0i16; MSBC_SAMPLES];
	if voice.play.len() >= MSBC_SAMPLES {
		for (slot, sample) in samples.iter_mut().zip(voice.play.drain(..MSBC_SAMPLES)) {
			*slot = sample;
		}
	} else {
		voice.clock.silent = voice.clock.silent.saturating_add(1);
	}
	let Some(encoder) = voice.encoder.as_mut() else { return };
	let mut frame = [0u8; sbc::MAX_FRAME_BYTES];
	let length = encoder.encode(&samples, &mut frame);
	let mut packet = [0u8; hfp::MSBC_PACKET];
	hfp::h2_frame(voice.sequence, &frame[..length], &mut packet);
	voice.sequence = voice.sequence.wrapping_add(1);
	out.extend_from_slice(&packet);
}

// ONE CVSD PACKET'S WORTH of what was played, as 16-bit linear samples: as many as the packet that came in carried.
fn cvsd_packet(voice: &mut VoiceLink, samples: usize, out: &mut Vec<u8>) {
	let have = voice.play.len() >= samples;
	if !have {
		voice.clock.silent = voice.clock.silent.saturating_add(1);
	}
	for _ in 0..samples {
		let sample = if have { voice.play.pop_front().unwrap_or(0) } else { 0 };
		out.extend_from_slice(&sample.to_le_bytes());
	}
}

// Whether the assembly starts at an H2 header: what a lost piece of a frame is resynchronised to.
fn at_header(bytes: &[u8]) -> bool {
	bytes.len() >= 2 && bytes[0] == 0x01 && hfp::H2_SEQUENCE.contains(&bytes[1])
}

impl Stack {
	fn voice(&mut self, at: usize, handle: u16) -> Option<&mut VoiceLink> {
		self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.voice.as_mut())
	}

	// A HEADSET OPENED THE GATEWAY'S CHANNEL: a gateway for it.
	pub(crate) fn voice_opened(&mut self, at: usize, handle: u16, server_channel: u8) {
		if let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut())
			&& classic.voice.is_none()
		{
			classic.voice = Some(VoiceLink::new(server_channel));
			// HSP has no HFP service-level or codec negotiation: its admitted RFCOMM
			// connection is already a CVSD voice endpoint.
			if server_channel == bt_policy::HSP_CHANNEL {
				self.voice_offer(at, handle);
			}
		}
	}

	// BYTES ON THE GATEWAY'S CHANNEL: each line answered, and what the headset asked for done.
	pub(crate) fn voice_data(&mut self, at: usize, handle: u16, bytes: &[u8]) {
		let Some(voice) = self.voice(at, handle) else { return };
		let outs = voice.gateway.receive(bytes);
		self.run_gateway(at, handle, outs);
	}

	fn run_gateway(&mut self, at: usize, handle: u16, outs: Vec<HfpOut>) {
		for out in outs {
			match out {
				HfpOut::Send(bytes) => {
					let Some(channel) = self.voice(at, handle).map(|voice| voice.channel) else { return };
					self.rfcomm_send(at, handle, channel, &bytes);
				}
				HfpOut::Event(event) => self.gateway_event(at, handle, event),
			}
		}
	}

	fn gateway_event(&mut self, at: usize, handle: u16, event: Hfp) {
		match event {
			// UP: a headset without codec negotiation is a CVSD voice device at once; one with it, once its codec is
			// confirmed.
			Hfp::Connected => {
				if !self.voice(at, handle).is_some_and(|voice| voice.gateway.negotiates()) {
					self.voice_offer(at, handle);
				}
			}
			Hfp::Codec(_) => self.voice_offer(at, handle),
			Hfp::Command(command) => {
				let command = match command {
					Command::Answer => BtCallCommand::Answer,
					Command::HangUp => BtCallCommand::HangUp,
					Command::Reject => BtCallCommand::Reject,
					Command::Redial => BtCallCommand::Redial,
				};
				self.audio.command(command);
			}
			Hfp::SpeakerGain(gain) => {
				if let Some(id) = self.voice(at, handle).and_then(|voice| voice.endpoint) {
					self.audio.volume_changed(id, hfp::level_of(gain));
				}
			}
			Hfp::MicrophoneGain(gain) => {
				if let Some(id) = self.voice(at, handle).and_then(|voice| voice.endpoint) {
					self.audio.microphone_volume_changed(id, hfp::level_of(gain));
				}
			}
			Hfp::Battery(level) => {
				if let Some(voice) = self.voice(at, handle) {
					voice.battery = Some(level);
				}
			}
			// THE HEADSET ASKS FOR AUDIO: the gateway took it only while a session holds the endpoint open.
			Hfp::AudioRequested => self.voice_up(at, handle),
		}
	}

	// THE VOICE ENDPOINT, offered to AudioService once the codec is known: mono at the codec's rate, the headset's gain
	// its own.
	fn voice_offer(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link(handle) else { return };
		let peer = link.peer;
		let Some(voice) = link.classic.as_ref().and_then(|classic| classic.voice.as_ref()) else { return };
		if voice.endpoint.is_some() {
			return;
		}
		let rate = voice.rate();
		let microphone_volume = hfp::level_of(voice.gateway.microphone_gain());
		let name = self.controllers[at].bredr_state.name_of(&peer).unwrap_or_default();
		// THE LATENCY: the link's interval, a packet each way, and the queue's two periods.
		let endpoint = AudioEndpoint { id: 0, peer: peer_to_wire(&peer), name, kind: AudioEndpointKind::Voice, rate, channels: 1, latency_us: 7_500 + (PLAY_HOLD_MS as u32) * 1_000, hardware_volume: true, volume: hfp::level_of(voice.gateway.speaker_gain()) };
		let Some(id) = self.audio.offer(endpoint, at, handle) else { return };
		self.audio.microphone_volume_changed(id, microphone_volume);
		if let Some(voice) = self.voice(at, handle) {
			voice.endpoint = Some(id);
		}
		print(b"BluetoothService: a headset's voice is offered to AudioService\n");
	}

	// THE SESSION OPENED THE ENDPOINT: the synchronous link, set up with the codec's setting.
	pub(crate) fn voice_up(&mut self, at: usize, handle: u16) {
		let Some(voice) = self.voice(at, handle) else { return };
		voice.gateway.set_audio_allowed(true);
		if voice.sco.is_some() || voice.setting_up {
			return;
		}
		voice.setting_up = true;
		let (latency, setting) = if voice.msbc() { (13, VOICE_TRANSPARENT) } else { (12, VOICE_CVSD) };
		self.controllers[at].command(hci_bredr::opcode::SETUP_SYNCHRONOUS_CONNECTION, &hci_bredr::setup_synchronous(handle, latency, setting, 2, PACKET_TYPES));
	}

	// THE SESSION CLOSED IT: the synchronous link goes down, and the headset may not ask for one.
	pub(crate) fn voice_down(&mut self, at: usize, handle: u16) {
		let Some(voice) = self.voice(at, handle) else { return };
		voice.gateway.set_audio_allowed(false);
		voice.play.clear();
		voice.capture.clear();
		voice.ack_waiting = false;
		let Some(sco) = voice.sco.take() else { return };
		self.controllers[at].disconnect(sco, REASON_USER);
		self.voice_transport(at, None);
	}

	// THE TRANSPORT'S VOICE SETTING: one channel of 16-bit samples, wideband for mSBC - or none.
	fn voice_transport(&mut self, at: usize, msbc: Option<bool>) -> bool {
		let transport = self.controllers[at].transport;
		let mut client = hci_transport::Client::new(ChannelTransport { chan: transport });
		let answer = match msbc {
			Some(wideband) => client.voice(&1, &16, &wideband),
			None => client.voice(&0, &0, &false),
		};
		matches!(answer, Some(Ok(_)))
	}

	// THE SYNCHRONOUS LINK IS UP - or refused: the transport told the setting, and the codec's coders made.
	pub(crate) fn voice_connected(&mut self, at: usize, status: u8, sco: u16, address: &[u8; 6]) {
		let peer = classic::bredr(address);
		let Some(handle) = self.controllers[at].link_to(&peer).map(|link| link.handle) else {
			if status == 0 {
				self.controllers[at].disconnect(sco, REASON_USER);
			}
			return;
		};
		let owned = self.audio.pcm.iter().any(|pcm| pcm.at == at && pcm.handle == handle && pcm.kind == AudioEndpointKind::Voice);
		let Some(voice) = self.voice(at, handle) else {
			if status == 0 {
				self.controllers[at].disconnect(sco, REASON_USER);
			}
			return;
		};
		voice.setting_up = false;
		if status != 0 {
			// NO LINK, NO AUDIO: what was queued is let go, and a period held for it acknowledged.
			voice.play.clear();
			let release = core::mem::take(&mut voice.ack_waiting);
			let endpoint = voice.endpoint;
			print(b"BluetoothService: the headset's voice link was not set up\n");
			if let (true, Some(id)) = (release, endpoint) {
				self.voice_ack(id);
			}
			return;
		}
		// A subscriber may close while SETUP_SYNCHRONOUS_CONNECTION is in flight. Keep that
		// request pending until this completion (so reopening cannot queue a second setup), but
		// never enable a late successful link without a current owner. A replacement that already
		// reopened the same voice endpoint is allowed to use the completion.
		if !owned {
			voice.gateway.set_audio_allowed(false);
			self.controllers[at].disconnect(sco, REASON_USER);
			return;
		}
		voice.sco = Some(sco);
		voice.clock = OneForOne::default();
		voice.assembling.clear();
		let msbc = voice.msbc();
		if msbc {
			voice.encoder = sbc::Encoder::new(sbc::Config::MSBC).map(Box::new);
			voice.decoder = Some(Box::new(sbc::Decoder::new()));
		}
		if self.voice_transport(at, Some(msbc)) {
			print(if msbc { b"BluetoothService: a headset's voice link is up: transparent, mSBC\n".as_slice() } else { b"BluetoothService: a headset's voice link is up: CVSD\n".as_slice() });
		} else {
			print(b"BluetoothService: the transport carries no voice; the link is up and silent\n");
		}
	}

	// A SYNCHRONOUS LINK WENT DOWN: true when it was a voice link's.
	pub(crate) fn voice_disconnected(&mut self, at: usize, sco: u16) -> bool {
		let mut found = None;
		for link in self.controllers[at].links.iter_mut() {
			if let Some(voice) = link.classic.as_mut().and_then(|classic| classic.voice.as_mut())
				&& voice.sco == Some(sco)
			{
				voice.sco = None;
				found = Some(core::mem::take(&mut voice.ack_waiting).then_some(voice.endpoint).flatten());
				voice.play.clear();
				break;
			}
		}
		let Some(release) = found else { return false };
		self.voice_transport(at, None);
		if let Some(id) = release {
			self.voice_ack(id);
		}
		true
	}

	// ONE VOICE PACKET FROM THE CONTROLLER: the headset's microphone into the capture queue, and one unit back for each
	// that came in.
	pub(crate) fn on_sco(&mut self, at: usize, bytes: &[u8]) {
		if bytes.len() < 3 {
			return;
		}
		let sco = u16::from_le_bytes([bytes[0], bytes[1]]) & 0x0fff;
		let data = &bytes[3..(3 + usize::from(bytes[2])).min(bytes.len())];
		let Some(handle) = self.controllers[at].links.iter().find(|link| link.classic.as_ref().and_then(|classic| classic.voice.as_ref()).is_some_and(|voice| voice.sco == Some(sco))).map(|link| link.handle) else { return };
		let Some(voice) = self.voice(at, handle) else { return };
		let msbc = voice.msbc();
		if msbc {
			voice.assembling.extend_from_slice(data);
			while voice.assembling.len() >= 2 && !at_header(&voice.assembling) {
				voice.assembling.remove(0);
			}
			while voice.assembling.len() >= hfp::MSBC_PACKET {
				let packet: Vec<u8> = voice.assembling.drain(..hfp::MSBC_PACKET).collect();
				let mut pcm = [0i16; MSBC_SAMPLES];
				let decoded = hfp::h2_payload(&packet).and_then(|frame| voice.decoder.as_mut()?.decode(frame, &mut pcm).ok());
				// A LOST OR DAMAGED FRAME is silence in its place.
				if decoded.is_none() {
					pcm = [0; MSBC_SAMPLES];
				}
				voice.capture.extend(pcm.iter().copied());
				voice.clock.arrived();
			}
		} else {
			voice.capture.extend(data.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])));
			voice.clock.arrived();
		}
		let hold = voice.rate() as usize * CAPTURE_HOLD_MS / 1000;
		while voice.capture.len() > hold {
			voice.capture.pop_front();
		}
		// ONE OUT FOR EACH ONE IN: what AudioService played, or silence.
		let mut out = Vec::new();
		while voice.clock.send() {
			if msbc {
				msbc_packet(voice, &mut out);
			} else {
				cvsd_packet(voice, data.len() / 2, &mut out);
			}
		}
		let release = voice.ack_waiting && voice.play.len() < voice.play_hold();
		if release {
			voice.ack_waiting = false;
		}
		let endpoint = voice.endpoint;
		if !out.is_empty() {
			self.send_sco(at, sco, &out);
		}
		if let Some(id) = endpoint {
			if release {
				self.voice_ack(id);
			}
			self.audio_capture_ready(id);
		}
	}

	// Voice data to the controller, cut to the packets its transport carries.
	fn send_sco(&mut self, at: usize, sco: u16, data: &[u8]) {
		let controller = &self.controllers[at];
		let room = (controller.sco_bytes as usize).saturating_sub(3).min(255);
		if room == 0 {
			return;
		}
		for chunk in data.chunks(room) {
			let mut packet = Vec::with_capacity(3 + chunk.len());
			packet.extend_from_slice(&sco.to_le_bytes());
			packet.push(chunk.len() as u8);
			packet.extend_from_slice(chunk);
			send_packet(controller.transport, HciPacketKind::Sco, &packet);
		}
	}

	// A PERIOD AUDIOSERVICE PLAYED on the voice endpoint: queued for the packets - or dropped with no link to carry it -
	// and true when it may be acknowledged now, false when two periods wait already.
	pub(crate) fn voice_play(&mut self, at: usize, handle: u16, samples: &[i16]) -> bool {
		let Some(voice) = self.voice(at, handle) else { return true };
		if voice.sco.is_none() && !voice.setting_up {
			return true;
		}
		voice.play.extend(samples.iter().copied());
		let room = voice.play.len() < voice.play_hold();
		if !room {
			voice.ack_waiting = true;
		}
		room
	}

	pub(crate) fn voice_take_period(&mut self, at: usize, handle: u16, samples: usize) -> Option<Vec<u8>> {
		let voice = self.voice(at, handle)?;
		if voice.capture.len() < samples {
			return None;
		}
		let mut out = Vec::with_capacity(samples * 2);
		for sample in voice.capture.drain(..samples) {
			out.extend_from_slice(&sample.to_le_bytes());
		}
		Some(out)
	}

	// THE CALL AUDIOSERVICE RELAYS, to every headset's gateway.
	pub(crate) fn voice_call(&mut self, state: BtCallState) {
		let call = match state {
			BtCallState::None => Call::None,
			BtCallState::Incoming => Call::Incoming,
			BtCallState::Outgoing => Call::Outgoing,
			BtCallState::Active => Call::Active,
			BtCallState::Held => Call::Held,
		};
		let mut targets = Vec::new();
		for (at, controller) in self.controllers.iter().enumerate() {
			for link in &controller.links {
				if link.classic.as_ref().is_some_and(|classic| classic.voice.is_some()) {
					targets.push((at, link.handle));
				}
			}
		}
		for (at, handle) in targets {
			let Some(voice) = self.voice(at, handle) else { continue };
			let outs = voice.gateway.set_call(call);
			self.run_gateway(at, handle, outs);
		}
		self.le_call(state);
	}

	// A LEVEL FOR THE HEADSET'S SPEAKER, as its gain.
	pub(crate) fn voice_set_volume(&mut self, at: usize, handle: u16, level: u8) -> Result<(), Error> {
		let voice = self.voice(at, handle).ok_or(Error::NotFound)?;
		let outs = voice.gateway.set_speaker_gain(hfp::gain_of(level));
		self.run_gateway(at, handle, outs);
		Ok(())
	}

	pub(crate) fn voice_microphone_volume(&mut self, at: usize, handle: u16) -> Result<u8, Error> {
		let voice = self.voice(at, handle).ok_or(Error::NotFound)?;
		Ok(hfp::level_of(voice.gateway.microphone_gain()))
	}

	pub(crate) fn voice_set_microphone_volume(&mut self, at: usize, handle: u16, level: u8) -> Result<(), Error> {
		let classic = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).ok_or(Error::Closed)?;
		let voice = classic.voice.as_mut().ok_or(Error::Closed)?;
		let rfcomm = classic.rfcomm.as_mut().ok_or(Error::Closed)?;
		let outs = voice.gateway.queue_microphone_gain(hfp::gain_of(level), &mut rfcomm.session, voice.channel).map_err(|error| match error {
			hfp::GainRefusal::Closed => Error::Closed,
			hfp::GainRefusal::Full => Error::Again,
			hfp::GainRefusal::Invalid => Error::Invalid,
		})?;
		self.run_rfcomm(at, handle, outs);
		Ok(())
	}

	// THE GATEWAY'S CHANNEL CLOSED: the voice endpoint leaves, and the synchronous link with it.
	pub(crate) fn voice_gone(&mut self, at: usize, handle: u16) {
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		let Some(voice) = classic.voice.take() else { return };
		if let Some(sco) = voice.sco {
			self.controllers[at].disconnect(sco, REASON_USER);
			self.voice_transport(at, None);
		}
		if let Some(id) = voice.endpoint {
			self.audio.withdraw(id);
		}
	}

	// THE LINK IS GONE, and the controller took its synchronous link with it.
	pub(crate) fn voice_link_gone(&mut self, at: usize, link: &mut Link) {
		let Some(voice) = link.classic.as_mut().and_then(|classic| classic.voice.take()) else { return };
		if voice.sco.is_some() {
			self.voice_transport(at, None);
		}
		if let Some(id) = voice.endpoint {
			self.audio.withdraw(id);
		}
	}
}
