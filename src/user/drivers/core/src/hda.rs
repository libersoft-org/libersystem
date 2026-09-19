// THE HIGH DEFINITION AUDIO DECISIONS, WITH NO CONTROLLER BEHIND THEM.
//
// Most of an HDA driver is not register access. It is a verb packed into a word, two rings whose
// pointers the controller and the driver each own half of, a walk over a graph a codec describes
// about itself, and a format word whose fields are not the numbers they name. Every one of those is
// a function from numbers to an answer, and every one is somewhere this driver would be wrong
// silently: a verb built with the wrong payload width is ANSWERED rather than refused, by a
// different node, with a different command.
//
// The specification is Intel's High Definition Audio 1.0a.

// Controller registers, at the mapped BAR base.
pub const REG_GCAP: u64 = 0x00; // 16-bit: how many streams of each kind
pub const REG_GCTL: u64 = 0x08; // global control
pub const REG_STATESTS: u64 = 0x0E; // 16-bit: which codec addresses answered the reset
pub const REG_INTCTL: u64 = 0x20;
pub const REG_CORBLBASE: u64 = 0x40;
pub const REG_CORBUBASE: u64 = 0x44;
pub const REG_CORBWP: u64 = 0x48; // 16-bit, the driver's half
pub const REG_CORBRP: u64 = 0x4A; // 16-bit, the controller's half
pub const REG_CORBCTL: u64 = 0x4C; // 8-bit
pub const REG_CORBSIZE: u64 = 0x4E; // 8-bit
pub const REG_RIRBLBASE: u64 = 0x50;
pub const REG_RIRBUBASE: u64 = 0x54;
pub const REG_RIRBWP: u64 = 0x58; // 16-bit, the controller's half
pub const REG_RINTCNT: u64 = 0x5A; // 16-bit
pub const REG_RIRBCTL: u64 = 0x5C; // 8-bit
// THE RESPONSE RING'S STATUS, AND THE REGISTER A POLLING DRIVER FORGETS. Bit 0 is raised when the
// controller has written `RINTCNT` responses and bit 2 when the ring overran; both are write-one-to-
// clear, and until they ARE cleared the controller stops fetching commands. A driver that polls and
// never touches this register gets exactly `RINTCNT` responses and then silence - which looks like a
// codec that stopped answering, not like a register nobody wrote.
pub const REG_RIRBSTS: u64 = 0x5D; // 8-bit
pub const RIRBSTS_RESPONSE: u8 = 1 << 0;
pub const RIRBSTS_OVERRUN: u8 = 1 << 2;
pub const REG_RIRBSIZE: u64 = 0x5E; // 8-bit
// Stream descriptors start here, 0x20 bytes apart.
pub const REG_STREAM_BASE: u64 = 0x80;
pub const REG_STREAM_STRIDE: u64 = 0x20;

// Stream descriptor registers, relative to a descriptor's own base.
pub const SD_CTL: u64 = 0x00; // 24 bits of control, written as bytes
pub const SD_STS: u64 = 0x03; // 8-bit status
pub const SD_LPIB: u64 = 0x04; // link position in buffer
pub const SD_CBL: u64 = 0x08; // cyclic buffer length
pub const SD_LVI: u64 = 0x0C; // 16-bit: last valid index
pub const SD_FMT: u64 = 0x12; // 16-bit format
pub const SD_BDLPL: u64 = 0x18;
pub const SD_BDLPU: u64 = 0x1C;

// `GCTL` bit 0 leaves reset; `CORBCTL`/`RIRBCTL` bit 1 runs the ring.
pub const GCTL_RESET: u32 = 1 << 0;
pub const RING_RUN: u8 = 1 << 1;
// `SDnCTL` bit 1 runs the stream; bit 0 resets it.
pub const SD_RUN: u8 = 1 << 1;
pub const SD_RESET: u8 = 1 << 0;

// One CORB entry is a 32-bit verb; one RIRB entry is a 64-bit pair.
pub const CORB_ENTRY_LEN: usize = 4;
pub const RIRB_ENTRY_LEN: usize = 8;
// A buffer descriptor is an address, a length and a flags word: sixteen bytes.
pub const BDL_ENTRY_LEN: usize = 16;

// Codec verbs this driver sends.
pub const VERB_GET_PARAMETER: u32 = 0xF00;
pub const VERB_GET_CONNECTION_LIST: u32 = 0xF02;
pub const VERB_SET_STREAM_FORMAT: u32 = 0x200;
pub const VERB_SET_STREAM_CHANNEL: u32 = 0x706;
pub const VERB_SET_AMP_GAIN: u32 = 0x300;
pub const VERB_SET_PIN_CONTROL: u32 = 0x707;
pub const VERB_SET_POWER_STATE: u32 = 0x705;

// Parameter ids read with `VERB_GET_PARAMETER`.
pub const PARAM_VENDOR_ID: u32 = 0x00;
pub const PARAM_NODE_COUNT: u32 = 0x04;
pub const PARAM_FUNCTION_TYPE: u32 = 0x05;
pub const PARAM_WIDGET_CAPS: u32 = 0x09;
pub const PARAM_PIN_CAPS: u32 = 0x0C;
pub const PARAM_CONNECTION_COUNT: u32 = 0x0E;

/// The function group type that carries audio. A codec may also report a modem group, which this
/// driver walks past rather than into.
pub const FUNCTION_AUDIO: u32 = 0x01;

/// Widget types, from bits 23:20 of the widget capabilities.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Widget {
	AudioOutput,
	AudioInput,
	AudioMixer,
	AudioSelector,
	PinComplex,
	Other(u32),
}

/// What a widget's capability word says it is.
pub fn widget_kind(caps: u32) -> Widget {
	match (caps >> 20) & 0x0F {
		0 => Widget::AudioOutput,
		1 => Widget::AudioInput,
		2 => Widget::AudioMixer,
		3 => Widget::AudioSelector,
		4 => Widget::PinComplex,
		other => Widget::Other(other),
	}
}

/// Pack one codec command.
///
/// THE PAYLOAD WIDTH IS PART OF THE VERB AND NOT OF THE VALUE. A verb whose identifier fits in four
/// bits carries a sixteen-bit payload; every other verb carries eight. Building a `SET STREAM
/// FORMAT` - a four-bit verb - as though it were an eight-bit one shifts the command into the
/// payload and the payload into the node, and the codec ANSWERS it: a different node gets a
/// different command and nothing anywhere reports a problem.
pub fn verb(codec: u8, node: u8, command: u32, payload: u32) -> u32 {
	let wide = command <= 0xF;
	let (command_bits, payload_bits) = if wide { (command & 0xF, payload & 0xFFFF) } else { (command & 0xFFF, payload & 0xFF) };
	let command_shift = if wide { 16 } else { 8 };
	((codec as u32 & 0x0F) << 28) | ((node as u32) << 20) | (command_bits << command_shift) | payload_bits
}

/// The same, for the four-bit verbs whose identifier is given in its 12-bit written form.
///
/// `SET STREAM FORMAT` is written `0x200` in the specification's tables, which is the four-bit verb
/// `2` shifted into a twelve-bit field. Handing that to `verb` directly would build a twelve-bit
/// verb with an eight-bit payload, which is the exact mistake above.
pub fn verb_wide(codec: u8, node: u8, written_command: u32, payload: u32) -> u32 {
	verb(codec, node, written_command >> 8, payload)
}

/// Which codec addresses answered the controller's reset, from `STATESTS`.
pub fn codecs_present(statests: u16) -> impl Iterator<Item = u8> {
	(0..15u8).filter(move |address| statests & (1 << address) != 0)
}

/// The node range a `NODE COUNT` parameter answer describes: a starting node and how many.
///
/// THE START IS IN THE TOP HALF AND THE COUNT IN THE BOTTOM, and reading them the other way round
/// walks from node 1 for as many nodes as the codec's first node happens to be numbered.
pub fn node_range(parameter: u32) -> (u8, u8) {
	(((parameter >> 16) & 0xFF) as u8, (parameter & 0xFF) as u8)
}

/// Why a codec cannot be driven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unusable {
	/// It answered its identity verb with nothing, so it is not there.
	NoCodec,
	/// It reports no audio function group.
	NoAudioGroup,
	/// No pin complex reaches an audio output converter.
	NoRoute,
	/// It describes more nodes than this driver's bounded walk will follow.
	TooManyNodes,
}

/// Whether a vendor id answer names a codec at all. A verb sent to an address nothing answers reads
/// back as all ones or as zero, and both are the absence rather than a vendor.
pub fn codec_answered(vendor_id: u32) -> bool {
	vendor_id != 0 && vendor_id != 0xFFFF_FFFF
}

/// The largest node count this driver's walk will follow. A codec reporting more is refused rather
/// than walked part way.
pub const MAX_NODES: u8 = 64;

/// How many codecs on one link this driver records. `STATESTS` has fifteen bits, so that is the
/// most a link can announce; the bound here is what the driver keeps, and a link with more than
/// this many is one whose extra codecs are not looked at rather than one that is refused.
pub const MAX_CODECS: usize = 15;

/// Whether a pin complex is capable of output, from its pin capabilities.
pub fn pin_can_output(pin_caps: u32) -> bool {
	pin_caps & (1 << 4) != 0
}

/// Whether a pin complex is capable of INPUT, which is the adjacent bit and not the same one.
///
/// FOUR AND FIVE, AND THEY ARE NOT INTERCHANGEABLE. A driver that looked for output capability when
/// it wanted a microphone routes the speaker jack into the capture converter: the stream runs, the
/// controller reports no error, and every period comes back silent - which reads as a codec with no
/// microphone rather than as a route built backwards.
pub fn pin_can_input(pin_caps: u32) -> bool {
	pin_caps & (1 << 5) != 0
}

/// `SD_STS` bit 2: a buffer descriptor with its interrupt-on-completion flag finished.
///
/// STICKY AND WRITE-ONE-TO-CLEAR, like every other change bit here. A reader that only reads sees
/// the first period's completion for ever and never notices the second.
pub const SD_BUFFER_COMPLETE: u8 = 1 << 2;

/// A buffer descriptor's flag word: raise the completion status when this entry finishes.
pub const BDL_INTERRUPT_ON_COMPLETION: u32 = 1 << 0;

/// The stream format word.
///
/// EVERY FIELD IS AN INDEX RATHER THAN THE NUMBER IT NAMES. The channel count is zero-based, the
/// sample rate is a base plus two multiplier fields rather than a frequency, and the bit depth is a
/// three-bit code. A driver that wrote 48000 into this register configures nothing that resembles
/// 48 kHz.
pub fn format(rate_hz: u32, bits: u8, channels: u8) -> Option<u16> {
	let base: u16 = match rate_hz {
		// The 44.1 kHz family sets bit 14; the 48 kHz family clears it.
		44100 => 1 << 14,
		48000 => 0,
		96000 => 1 << 11,
		192000 => 2 << 11,
		_ => return None,
	};
	let depth: u16 = match bits {
		8 => 0,
		16 => 1,
		20 => 2,
		24 => 3,
		32 => 4,
		_ => return None,
	};
	if channels == 0 || channels > 16 {
		return None;
	}
	Some(base | (depth << 4) | (channels as u16 - 1))
}

/// How many bytes one second of that format carries, which is what a buffer length is chosen from.
pub fn bytes_per_second(rate_hz: u32, bits: u8, channels: u8) -> u64 {
	(rate_hz as u64) * ((bits as u64).div_ceil(8)) * channels as u64
}

/// Where a ring's next entry goes, given the pointer the hardware reports and how many entries the
/// ring holds.
///
/// THE WRITE POINTER IS THE LAST ENTRY WRITTEN AND NOT THE NEXT SLOT, which is the opposite of every
/// other ring in this tree. Treating it as the next slot leaves one entry unwritten and one read
/// twice, on every command.
pub fn ring_next(pointer: u16, entries: u16) -> u16 {
	if entries == 0 { 0 } else { (pointer + 1) % entries }
}

/// Whether the response ring has anything new: its write pointer has moved away from where this
/// driver last read.
pub fn ring_has(write_pointer: u16, read: u16) -> bool {
	write_pointer != read
}

/// The `CORBSIZE`/`RIRBSIZE` code for a ring of this many entries, and `None` for a size no
/// controller expresses.
pub fn ring_size_code(entries: u16) -> Option<u8> {
	match entries {
		2 => Some(0),
		16 => Some(1),
		256 => Some(2),
		_ => None,
	}
}

#[cfg(test)]
mod tests;
