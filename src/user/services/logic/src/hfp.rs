//! THE HANDS-FREE PROFILE'S AUDIO GATEWAY, AND THE HEADSET PROFILE'S: the AT command set a headset speaks over RFCOMM,
//! answered from this system's side - the service level connection, the indicators, codec negotiation, gains, the HF
//! indicators a headset reports its battery through, and the call relay - as a pure state machine over lines.
//!
//! WHAT THIS GATEWAY IS. A voice gateway with no telephone network behind it: its calls are the ones a voice session
//! declares (AudioService's call relay), and with none declared it reports no call AND NO NETWORK SERVICE, and answers
//! every call command - answer, hang up, redial - with ERROR. When a session declares a call, the indicators follow
//! it and the headset's commands are relayed back to that session.
//!
//! THE CODEC. CVSD at 8 kHz is the controller's air coding and every headset has it; mSBC at 16 kHz is the host's,
//! negotiated with `+BCS` when both sides list it in `AT+BAC`.

use alloc::string::String;
use alloc::vec::Vec;

/// The codec identifiers HFP negotiates.
pub const CODEC_CVSD: u8 = 1;
pub const CODEC_MSBC: u8 = 2;

/// THIS GATEWAY'S FEATURES, as `+BRSF` states them: the ability to reject a call (bit 5), enhanced call status (6),
/// codec negotiation (9) and HF indicators (10). No three-way calling, no voice recognition, no in-band ring tone.
pub const AG_FEATURES: u32 = (1 << 5) | (1 << 6) | (1 << 9) | (1 << 10);
/// The gateway's features as its SDP record states them: wideband speech, bit 5.
pub const SDP_FEATURES: u16 = 1 << 5;
/// The HF feature bit saying it does codec negotiation, and the one for HF indicators.
pub const HF_CODEC_NEGOTIATION: u32 = 1 << 7;
pub const HF_INDICATORS: u32 = 1 << 8;

/// The HF indicator HFP assigns to the battery level.
pub const HF_INDICATOR_BATTERY: u32 = 2;

/// The call state the gateway reports, as the call relay declares it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Call {
	#[default]
	None,
	Incoming,
	Outgoing,
	Active,
	Held,
}

/// What a headset asked of a call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
	Answer,
	HangUp,
	Reject,
	Redial,
}

/// What the service does about a line, besides sending the answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
	/// The service level connection is up: the headset is a voice device.
	Connected,
	/// A call command, to relay to the session that declared the call.
	Command(Command),
	/// The headset's speaker or microphone gain, 0 to 15.
	SpeakerGain(u8),
	MicrophoneGain(u8),
	/// The headset's battery, 0 to 100.
	Battery(u8),
	/// The codec both sides settled on, and the audio link may be set up with it.
	Codec(u8),
	/// The headset asked for the audio link (`AT+BCC`, or a button on a headset-profile device).
	AudioRequested,
}

/// What to do: a line to send - complete with its `\r\n` framing - or an event.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
	Send(Vec<u8>),
	Event(Event),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
	// Waiting for `AT+BRSF` - or, from a headset-profile device, anything at all.
	Features,
	Codecs,
	IndicatorList,
	IndicatorValues,
	Reporting,
	Connected,
}

/// The indicators, in the order `+CIND=?` lists them.
const INDICATORS: &str = r#"("service",(0,1)),("call",(0,1)),("callsetup",(0-3)),("callheld",(0-2)),("signal",(0-5)),("roam",(0,1)),("battchg",(0-5))"#;
const SERVICE: usize = 1;
const CALL: usize = 2;
const CALLSETUP: usize = 3;
const CALLHELD: usize = 4;

/// ONE HEADSET'S GATEWAY.
#[derive(Clone, Debug)]
pub struct Gateway {
	stage: Stage,
	/// The device speaks only the headset profile: no service level connection, a button for everything.
	headset_profile: bool,
	hf_features: u32,
	hf_codecs: [bool; 3],
	reporting: bool,
	call: Call,
	codec: Option<u8>,
	partial: Vec<u8>,
	/// A voice session holds the link open: a headset's request for audio is taken only then.
	audio_allowed: bool,
}

fn line(text: &str) -> Out {
	let mut bytes = Vec::with_capacity(text.len() + 4);
	bytes.extend_from_slice(b"\r\n");
	bytes.extend_from_slice(text.as_bytes());
	bytes.extend_from_slice(b"\r\n");
	Out::Send(bytes)
}

fn ok() -> Out {
	line("OK")
}

fn error() -> Out {
	line("ERROR")
}

fn number(text: &[u8]) -> Option<u32> {
	let text = core::str::from_utf8(text).ok()?.trim();
	text.parse().ok()
}

impl Gateway {
	/// A gateway for a hands-free device, or - `headset_profile` - for one that speaks only the headset profile.
	pub fn new(headset_profile: bool) -> Gateway {
		Gateway { stage: if headset_profile { Stage::Connected } else { Stage::Features }, headset_profile, hf_features: 0, hf_codecs: [false; 3], reporting: false, call: Call::None, codec: None, partial: Vec::new(), audio_allowed: false }
	}

	/// Whether the service level connection is up.
	pub fn connected(&self) -> bool {
		self.stage == Stage::Connected
	}

	/// Whether the codec is negotiated before the voice device is known: a hands-free device that does it.
	pub fn negotiates(&self) -> bool {
		!self.headset_profile && self.hf_features & HF_CODEC_NEGOTIATION != 0
	}

	/// Whether a voice session holds the link open, so a headset may ask for audio.
	pub fn set_audio_allowed(&mut self, allowed: bool) {
		self.audio_allowed = allowed;
	}

	/// The codec the audio link uses: mSBC where both negotiated it, CVSD otherwise.
	pub fn codec(&self) -> u8 {
		self.codec.unwrap_or(CODEC_CVSD)
	}

	fn indicators(&self) -> [u8; 8] {
		let declared = self.call != Call::None;
		let mut values = [0u8; 8];
		// NO NETWORK SERVICE while no session declares a call: there is no telephone behind this gateway.
		values[SERVICE] = u8::from(declared);
		values[CALL] = u8::from(matches!(self.call, Call::Active | Call::Held));
		values[CALLSETUP] = match self.call {
			Call::Incoming => 1,
			Call::Outgoing => 2,
			_ => 0,
		};
		values[CALLHELD] = if self.call == Call::Held { 2 } else { 0 };
		values[5] = 0;
		values[6] = 0;
		values[7] = 5;
		values
	}

	/// BYTES FROM THE HEADSET'S RFCOMM CHANNEL: every complete command line in them answered, a partial one kept.
	pub fn receive(&mut self, bytes: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		self.partial.extend_from_slice(bytes);
		if self.partial.len() > 512 {
			// A LINE THAT NEVER ENDS is not one this gateway reads.
			self.partial.clear();
			out.push(error());
			return out;
		}
		while let Some(end) = self.partial.iter().position(|&byte| byte == b'\r' || byte == b'\n') {
			let command: Vec<u8> = self.partial.drain(..=end).collect();
			let command = &command[..command.len() - 1];
			if command.is_empty() {
				continue;
			}
			out.extend(self.command(command));
		}
		out
	}

	fn command(&mut self, raw: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		let text = String::from_utf8_lossy(raw);
		let upper = text.trim().to_ascii_uppercase();
		let after = |prefix: &str| -> Option<String> { upper.strip_prefix(prefix).map(String::from) };
		if let Some(value) = after("AT+BRSF=") {
			self.hf_features = number(value.as_bytes()).unwrap_or(0);
			out.push(line(&alloc::format!("+BRSF: {AG_FEATURES}")));
			out.push(ok());
			self.stage = if self.hf_features & HF_CODEC_NEGOTIATION != 0 { Stage::Codecs } else { Stage::IndicatorList };
		} else if let Some(value) = after("AT+BAC=") {
			self.hf_codecs = [false; 3];
			for codec in value.split(',').filter_map(|part| part.trim().parse::<usize>().ok()) {
				if codec < 3 {
					self.hf_codecs[codec] = true;
				}
			}
			out.push(ok());
			if self.stage == Stage::Codecs {
				self.stage = Stage::IndicatorList;
			}
		} else if upper == "AT+CIND=?" {
			out.push(line(&alloc::format!("+CIND: {INDICATORS}")));
			out.push(ok());
			self.stage = Stage::IndicatorValues;
		} else if upper == "AT+CIND?" {
			let values = self.indicators();
			out.push(line(&alloc::format!("+CIND: {},{},{},{},{},{},{}", values[1], values[2], values[3], values[4], values[5], values[6], values[7])));
			out.push(ok());
			self.stage = Stage::Reporting;
		} else if let Some(value) = after("AT+CMER=") {
			// Mode 3, indicator reporting on: the fourth field.
			self.reporting = value.split(',').nth(3).is_some_and(|field| field.trim() == "1");
			out.push(ok());
			if self.stage == Stage::Reporting {
				self.stage = Stage::Connected;
				out.push(Out::Event(Event::Connected));
				// THE CODEC, chosen by the gateway at once where both negotiate it: mSBC where both have it.
				if self.hf_features & HF_CODEC_NEGOTIATION != 0 {
					let codec = if self.hf_codecs[usize::from(CODEC_MSBC)] { CODEC_MSBC } else { CODEC_CVSD };
					out.push(line(&alloc::format!("+BCS: {codec}")));
				}
			}
		} else if let Some(value) = after("AT+BCS=") {
			match number(value.as_bytes()).filter(|codec| *codec == 1 || *codec == 2) {
				Some(codec) => {
					self.codec = Some(codec as u8);
					out.push(ok());
					out.push(Out::Event(Event::Codec(codec as u8)));
				}
				None => out.push(error()),
			}
		} else if upper == "AT+BCC" {
			// A LINK A HEADSET ASKS FOR WITH NO SESSION OPEN is refused: the link follows the session.
			if self.audio_allowed {
				out.push(ok());
				out.push(Out::Event(Event::AudioRequested));
			} else {
				out.push(error());
			}
		} else if upper == "AT+BIND=?" {
			out.push(line(&alloc::format!("+BIND: ({HF_INDICATOR_BATTERY})")));
			out.push(ok());
		} else if upper == "AT+BIND?" {
			out.push(line(&alloc::format!("+BIND: {HF_INDICATOR_BATTERY},1")));
			out.push(ok());
		} else if upper.starts_with("AT+BIND=") {
			out.push(ok());
		} else if let Some(value) = after("AT+BIEV=") {
			let mut fields = value.split(',');
			let indicator = fields.next().and_then(|field| field.trim().parse::<u32>().ok());
			let level = fields.next().and_then(|field| field.trim().parse::<u32>().ok());
			match (indicator, level) {
				(Some(HF_INDICATOR_BATTERY), Some(level)) if level <= 100 => {
					out.push(ok());
					out.push(Out::Event(Event::Battery(level as u8)));
				}
				_ => out.push(error()),
			}
		} else if let Some(value) = after("AT+VGS=") {
			match number(value.as_bytes()).filter(|gain| *gain <= 15) {
				Some(gain) => {
					out.push(ok());
					out.push(Out::Event(Event::SpeakerGain(gain as u8)));
				}
				None => out.push(error()),
			}
		} else if let Some(value) = after("AT+VGM=") {
			match number(value.as_bytes()).filter(|gain| *gain <= 15) {
				Some(gain) => {
					out.push(ok());
					out.push(Out::Event(Event::MicrophoneGain(gain as u8)));
				}
				None => out.push(error()),
			}
		} else if upper == "ATA" {
			out.extend(self.call_command(Command::Answer, self.call == Call::Incoming));
		} else if upper == "AT+CHUP" {
			let command = if self.call == Call::Incoming { Command::Reject } else { Command::HangUp };
			out.extend(self.call_command(command, self.call != Call::None));
		} else if upper == "AT+BLDN" {
			out.extend(self.call_command(Command::Redial, self.call != Call::None));
		} else if upper.starts_with("AT+CKPD") && self.headset_profile {
			// THE HEADSET PROFILE'S ONE BUTTON: it answers a ringing call, ends an active one, and asks for audio.
			out.push(ok());
			match self.call {
				Call::Incoming => out.push(Out::Event(Event::Command(Command::Answer))),
				Call::Active | Call::Outgoing | Call::Held => out.push(Out::Event(Event::Command(Command::HangUp))),
				Call::None => out.push(Out::Event(Event::AudioRequested)),
			}
		} else if upper == "AT+CLCC" {
			// The current calls: none listed - the declared call has no number.
			out.push(ok());
		} else if upper.starts_with("AT+CLIP=") || upper.starts_with("AT+CCWA=") || upper.starts_with("AT+CMEE=") || upper.starts_with("AT+NREC=") || upper.starts_with("AT+XAPL=") || upper.starts_with("AT+IPHONEACCEV=") {
			out.push(ok());
		} else if upper == "AT+COPS?" {
			out.push(line("+COPS: 0"));
			out.push(ok());
		} else {
			out.push(error());
		}
		out
	}

	// A CALL COMMAND: relayed where a session declares a call it applies to, ERROR otherwise.
	fn call_command(&mut self, command: Command, applies: bool) -> Vec<Out> {
		if applies { alloc::vec![ok(), Out::Event(Event::Command(command))] } else { alloc::vec![error()] }
	}

	/// THE CALL A SESSION DECLARES, reported: each indicator that changed, as an unsolicited `+CIEV` where reporting is
	/// on, and RING while one is incoming.
	pub fn set_call(&mut self, call: Call) -> Vec<Out> {
		let before = self.indicators();
		self.call = call;
		let after = self.indicators();
		let mut out = Vec::new();
		if self.stage == Stage::Connected && self.reporting {
			for index in 1..8 {
				if before[index] != after[index] {
					out.push(line(&alloc::format!("+CIEV: {index},{}", after[index])));
				}
			}
		}
		if call == Call::Incoming && (self.stage == Stage::Connected) {
			out.push(line("RING"));
		}
		out
	}

	/// The level AudioService set, as the speaker gain the headset applies itself.
	pub fn set_speaker_gain(&mut self, gain: u8) -> Vec<Out> {
		alloc::vec![line(&alloc::format!("+VGS: {}", gain.min(15)))]
	}
}

/// A level, 0 to 100, as a gain, 0 to 15, and back - rounded so a gain survives the trip.
pub fn gain_of(level: u8) -> u8 {
	((u32::from(level.min(100)) * 15 + 50) / 100) as u8
}

pub fn level_of(gain: u8) -> u8 {
	((u32::from(gain.min(15)) * 100 + 7) / 15) as u8
}

// ------------------------------------------------------------------ mSBC's framing on the SCO link

/// THE H2 HEADER before each mSBC frame on the voice link: a sync word and a two-bit sequence, sent twice inverted.
pub const H2_SEQUENCE: [u8; 4] = [0x08, 0x38, 0xc8, 0xf8];
/// One mSBC frame on the link: the H2 header, the 57-byte frame, and a padding byte - 60 bytes.
pub const MSBC_PACKET: usize = 60;

/// Frame `frame` (57 bytes) for the link as sequence number `sequence`.
pub fn h2_frame(sequence: u8, frame: &[u8], out: &mut [u8; MSBC_PACKET]) {
	out[0] = 0x01;
	out[1] = H2_SEQUENCE[usize::from(sequence & 3)];
	let length = frame.len().min(57);
	out[2..2 + length].copy_from_slice(&frame[..length]);
	out[59] = 0;
}

/// The mSBC frame in one 60-byte packet, if its H2 header is one of the four.
pub fn h2_payload(packet: &[u8]) -> Option<&[u8]> {
	if packet.len() < MSBC_PACKET || packet[0] != 0x01 || !H2_SEQUENCE.contains(&packet[1]) || packet[2] != crate::sbc::MSBC_SYNC {
		return None;
	}
	Some(&packet[2..59])
}

#[cfg(test)]
mod tests;
