// THE FIXTURE'S HANDS-FREE UNIT: the headset's side of HFP - the service level connection it sets up with the host's
// gateway over RFCOMM, the commands it presses, and the voice link the host sets up to it.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS PROTOCOLS ARE ITS OWN: the AT exchange here is the hands-free unit's,
// written from the profile apart from the host's `service_logic::hfp` gateway, and what it hears on the voice link it
// judges with `bt_sbc` - so a call the headset reports as good is one two implementations agree on.
//
//   the SLC         `AT+BRSF` with codec negotiation and HF indicators, `AT+BAC=1,2`, the indicators listed and read,
//                   reporting turned on - then mSBC confirmed when the gateway chooses it, the battery reported at 80
//                   and the speaker gain at 10
//   its buttons     ATA, AT+CHUP and AT+BCC on the gate's word, each answer said
//   the voice link  a synchronous link the host sets up is answered with the headset's microphone - a tone written into
//                   one subband of mSBC, a packet every 7.5 ms - and what the host sends is heard, every frame read

use super::*;
use crate::bt_sbc;

// The host's gateway's RFCOMM channel, as its record names it.
pub(super) const GATEWAY_CHANNEL: u8 = 1;
// What the headset says it does: codec negotiation (bit 7) and HF indicators (bit 8).
const HF_FEATURES: u32 = (1 << 7) | (1 << 8);
// The subband the headset's microphone tone is written into, and its scale factor.
const MICROPHONE_SUBBAND: usize = 5;
const MICROPHONE_FACTOR: u32 = 11;
// The voice links' handles: one per device, past the ACL ones.
const SCO_HANDLE_BASE: u16 = 0x0100;
// mSBC on the link: a 60-byte packet, the H2 header's sequence words.
const MSBC_PACKET: usize = 60;
const H2_SEQUENCE: [u8; 4] = [0x08, 0x38, 0xc8, 0xf8];

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Slc {
	#[default]
	Down,
	Features,
	Codecs,
	IndicatorList,
	IndicatorValues,
	Reporting,
	Up,
	Indicators,
	Battery,
	Gain,
	Ready,
}

#[derive(Default)]
pub(super) struct Hf {
	pub slc: Slc,
	// The gate asked for the SLC: it starts once the gateway's channel is open.
	pub connecting: bool,
	pub partial: Vec<u8>,
	pub msbc: bool,
	// The command whose answer the gate reads, by its text.
	pub asked: Option<&'static str>,
	// The voice link: its handle, when it came up, the packets sent, what was heard.
	pub sco: Option<u16>,
	pub since: u64,
	pub sent: u64,
	pub phase: u32,
	pub sequence: u8,
	pub assembling: Vec<u8>,
	pub heard: u32,
	pub bad: u32,
	pub energy: [u64; 8],
}

impl World {
	fn hf(&mut self, at: usize) -> Option<&mut Hf> {
		self.devices[at].link.as_mut().map(|link| &mut link.hf)
	}

	// THE HEADSET CONNECTS TO THE HOST'S GATEWAY: an RFCOMM channel to it - which the host holds until it has secured
	// the link - the SLC begun once it is open; or the host paged first, and the channel opened once it takes the page.
	pub(super) fn hf_connect(&mut self, at: usize) -> Result<Vec<Out>, &'static str> {
		if self.devices[at].link.is_none() {
			self.devices[at].hf_wanted = true;
			return Ok(self.act(FIRST_DEVICE + at as u8, 1, 0)?.1);
		}
		if let Some(hf) = self.hf(at) {
			hf.slc = Slc::Down;
			hf.connecting = true;
		}
		self.open_channel(at, 0x0003, Purpose::Rfcomm(GATEWAY_CHANNEL)).ok_or("no link")
	}

	// THE HOST TOOK THE PAGE: a connection the gate asked for before there was a link goes now.
	pub(super) fn hf_linked(&mut self, at: usize) -> Vec<Out> {
		if !core::mem::take(&mut self.devices[at].hf_wanted) {
			return Vec::new();
		}
		self.hf_connect(at).unwrap_or_default()
	}

	// ONE AT COMMAND to the gateway.
	fn at(&mut self, at: usize, command: &str) -> Vec<Out> {
		let mut bytes = command.as_bytes().to_vec();
		bytes.push(b'\r');
		self.rfcomm_write(at, GATEWAY_CHANNEL, &bytes)
	}

	// THE CHANNEL IS UP: the features first.
	pub(super) fn hf_opened(&mut self, at: usize) -> Vec<Out> {
		let Some(hf) = self.hf(at) else { return Vec::new() };
		if !core::mem::take(&mut hf.connecting) {
			return Vec::new();
		}
		hf.slc = Slc::Features;
		self.at(at, &format!("AT+BRSF={HF_FEATURES}"))
	}

	// WHAT THE GATEWAY SENT: each line read, the SLC's next step taken on its OK.
	pub(super) fn hf_receive(&mut self, at: usize, bytes: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(hf) = self.hf(at) else { return out };
		hf.partial.extend_from_slice(bytes);
		let mut lines = Vec::new();
		while let Some(end) = hf.partial.iter().position(|&byte| byte == b'\n') {
			let line: Vec<u8> = hf.partial.drain(..=end).collect();
			let text = String::from(String::from_utf8_lossy(&line).trim());
			if !text.is_empty() {
				lines.push(text);
			}
		}
		for line in lines {
			out.extend(self.hf_line(at, &line));
		}
		out
	}

	fn hf_line(&mut self, at: usize, line: &str) -> Vec<Out> {
		let name = self.short(at);
		let Some(hf) = self.hf(at) else { return Vec::new() };
		if let Some(command) = hf.asked.take() {
			if line == "OK" || line == "ERROR" {
				self.say(format!("{name}'s {command} was answered {line}"));
				return Vec::new();
			}
			hf.asked = Some(command);
		}
		if let Some(value) = line.strip_prefix("+BCS:") {
			let codec = String::from(value.trim());
			hf.msbc = codec == "2";
			return self.at(at, &format!("AT+BCS={codec}"));
		}
		if line.starts_with("+CIEV:") || line == "RING" {
			self.say(format!("{name} saw {line}"));
			return Vec::new();
		}
		if let Some(gain) = line.strip_prefix("+VGS:") {
			self.say(format!("{name}'s speaker gain was set to {}", gain.trim()));
			return Vec::new();
		}
		if line != "OK" {
			return Vec::new();
		}
		// THE SLC, A STEP FOR EACH OK. Reporting on is the SLC up; the codec's confirmation comes next where the
		// gateway chose one, and its OK - or the CMER's, with none - is what the HF indicators follow.
		let (next, command) = match hf.slc {
			Slc::Features => (Slc::Codecs, "AT+BAC=1,2"),
			Slc::Codecs => (Slc::IndicatorList, "AT+CIND=?"),
			Slc::IndicatorList => (Slc::IndicatorValues, "AT+CIND?"),
			Slc::IndicatorValues => (Slc::Reporting, "AT+CMER=3,0,0,1"),
			Slc::Reporting => (Slc::Up, ""),
			Slc::Up => (Slc::Indicators, "AT+BIND=2"),
			Slc::Indicators => (Slc::Battery, "AT+BIEV=2,80"),
			Slc::Battery => (Slc::Gain, "AT+VGS=10"),
			Slc::Gain => {
				hf.slc = Slc::Ready;
				let codec = if hf.msbc { "mSBC" } else { "CVSD" };
				self.say(format!("{name}'s service level connection is up, codec {codec}, its battery 80"));
				return Vec::new();
			}
			Slc::Down | Slc::Ready => return Vec::new(),
		};
		hf.slc = next;
		if command.is_empty() {
			return Vec::new();
		}
		self.at(at, command)
	}

	// A BUTTON: its command, and its answer said when it comes.
	pub(super) fn hf_press(&mut self, at: usize, command: &'static str) -> Result<Vec<Out>, &'static str> {
		let hf = self.hf(at).ok_or("no link")?;
		if hf.slc != Slc::Ready {
			return Err("the headset has no service level connection");
		}
		hf.asked = Some(command);
		Ok(self.at(at, command))
	}

	// ------------------------------------------------------------------ the voice link

	// THE HOST SETS A SYNCHRONOUS LINK UP over the headset's ACL link: the controller's completion, and the microphone
	// starts.
	pub(super) fn sco_setup(&mut self, acl: u16, transparent: bool) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(at) = self.by_handle(acl) else { return out };
		let now = self.now;
		let sco = SCO_HANDLE_BASE + at as u16;
		let address = self.devices[at].spec.address;
		let name = self.short(at);
		if let Some(hf) = self.hf(at) {
			*hf = Hf { slc: hf.slc, msbc: hf.msbc, partial: core::mem::take(&mut hf.partial), asked: hf.asked, sco: Some(sco), since: now, ..Hf::default() };
		}
		let mut body = alloc::vec![0];
		body.extend_from_slice(&sco.to_le_bytes());
		body.extend_from_slice(&wire(&address));
		// eSCO, an interval of six slots, a window of two, 60-byte packets each way, the air mode the setting asked.
		body.extend_from_slice(&[0x02, 6, 2, 60, 0, 60, 0, if transparent { 0x03 } else { 0x02 }]);
		out.push(event(0x2c, &body));
		self.say(format!("{name}'s voice link is up: {}", if transparent { "transparent, mSBC" } else { "CVSD" }));
		out
	}

	pub(super) fn owns_sco(&self, handle: u16) -> Option<usize> {
		self.devices.iter().position(|device| device.link.as_ref().is_some_and(|link| link.hf.sco == Some(handle)))
	}

	// THE HOST TOOK THE LINK DOWN: what was heard said.
	pub(super) fn sco_down(&mut self, handle: u16) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(at) = self.owns_sco(handle) else { return out };
		let name = self.short(at);
		let Some(hf) = self.hf(at) else { return out };
		hf.sco = None;
		let loudest = (0..8).max_by_key(|&subband| hf.energy[subband]).unwrap_or(0);
		let crc = if hf.bad == 0 { String::from("every CRC good") } else { format!("{} bad", hf.bad) };
		let line = format!("{name} heard {} mSBC frames from the host, {crc}, the loudest subband {loudest}; its voice link is down", hf.heard);
		self.say(line);
		let h = handle.to_le_bytes();
		out.push(event(0x05, &[0, h[0], h[1], 0x16]));
		out
	}

	// ONE VOICE PACKET FROM THE HOST: mSBC frames reassembled from 60-byte packets and judged.
	pub fn sco_in(&mut self, bytes: &[u8]) {
		if bytes.len() < 3 {
			return;
		}
		let handle = u16::from_le_bytes([bytes[0], bytes[1]]) & 0x0fff;
		let Some(at) = self.owns_sco(handle) else { return };
		let Some(hf) = self.hf(at) else { return };
		hf.assembling.extend_from_slice(&bytes[3..]);
		while hf.assembling.len() >= MSBC_PACKET {
			let packet: Vec<u8> = hf.assembling.drain(..MSBC_PACKET).collect();
			if packet[0] != 0x01 || !H2_SEQUENCE.contains(&packet[1]) {
				hf.bad += 1;
				continue;
			}
			match bt_sbc::hear(&packet[2..59]) {
				Some(heard) if heard.crc_good => {
					hf.heard += 1;
					for (total, energy) in hf.energy.iter_mut().zip(heard.energy.iter()) {
						*total = total.saturating_add(*energy);
					}
				}
				_ => hf.bad += 1,
			}
		}
	}

	// THE MICROPHONE, ON THE LINK'S CLOCK: the packets due since it came up - one every 7.5 ms, four for every three
	// ticks of 10 ms - each an mSBC frame of a tone.
	pub(super) fn sco_tick(&mut self, now: u64) -> Vec<Out> {
		let mut out = Vec::new();
		for at in 0..self.devices.len() {
			let Some(hf) = self.hf(at) else { continue };
			let Some(sco) = hf.sco else { continue };
			let due = now.saturating_sub(hf.since) * 4 / 3;
			while hf.sent < due {
				let mut frame = [0u8; 64];
				let length = bt_sbc::msbc_tone_frame(MICROPHONE_SUBBAND, MICROPHONE_FACTOR, &mut hf.phase, &mut frame);
				let mut packet = alloc::vec![0u8; 3 + MSBC_PACKET];
				packet[..2].copy_from_slice(&sco.to_le_bytes());
				packet[2] = MSBC_PACKET as u8;
				packet[3] = 0x01;
				packet[4] = H2_SEQUENCE[usize::from(hf.sequence & 3)];
				packet[5..5 + length].copy_from_slice(&frame[..length]);
				hf.sequence = hf.sequence.wrapping_add(1);
				hf.sent += 1;
				out.push(Out::Sco(packet));
			}
		}
		out
	}

	pub fn voice_active(&self) -> bool {
		self.devices.iter().any(|device| device.link.as_ref().is_some_and(|link| link.hf.sco.is_some()))
	}
}
