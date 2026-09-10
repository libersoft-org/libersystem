// The boot's DMA mode: what the machine translates with, stated by whoever built the machine, and
// carried to the kernel's admission decision by exactly one producer per entry path.
//
// WHY THIS IS A FIELD AND NOT A DISCOVERY. The kernel can see whether a `virtio-iommu` is on its bus
// and whether it came up; it cannot see whether it was SUPPOSED to be there. A controller that is
// absent looks the same on a machine that was built without one and on a machine whose attacker
// removed one, and a policy that reads "no controller" as "untranslated DMA is fine here" has made
// the degraded profile reachable by taking a device away. So the mode is stated up front, by the
// party that assembled the machine - a signed release field on a public boot, the harness on every
// test, development, gate and device-tree boot - and admission compares the machine against the
// statement rather than inferring the statement from the machine.
//
// THREE CARRIERS, ONE FORMAT. The harness writes the same eight bytes on every input it has: an
// x86_64 `fw_cfg` file, a file on the ESP the runner assembles for the two UEFI ports, and a
// property under this product's own node in the device tree a direct boot receives. A signed
// manifest carries the mode in its own tagged header field (`manifest.rs`) and never this record:
// the record's provenance byte can say only `harness`, because a carrier that lives on replaceable
// media is exactly the place a value must not be able to claim it was authenticated.
//
// SHARED ON PURPOSE. The loader, the kernel and the host tools all read this module - a producer
// and a consumer that each keep their own copy of a frozen format is how a frozen format drifts.

// The record, byte by byte. `LSDM`, the format version, the mode, the provenance, a reserved zero.
pub const RECORD_MAGIC: [u8; 4] = *b"LSDM";
pub const RECORD_VERSION: u8 = 1;
pub const RECORD_LEN: usize = 8;

// The mode codes, as `BootInfo` and the signed manifest carry them. Zero is deliberately not a mode:
// it is what an uninitialised field reads as, and an uninitialised field must decode as ABSENT.
pub const MODE_ABSENT: u32 = 0;
pub const MODE_ENFORCING_REQUIRED: u32 = 1;
pub const MODE_NO_IOMMU: u32 = 2;

// Where a value came from, as `BootInfo` carries it. `signed` is written only by a loader for a
// value it verified against a signature it checked; `harness` is asserted by the named harness,
// or relayed by a loader from the harness input it validated.
pub const PROVENANCE_ABSENT: u32 = 0;
pub const PROVENANCE_SIGNED: u32 = 1;
pub const PROVENANCE_HARNESS: u32 = 2;

// The one `fw_cfg` file the x86_64 harness writes and the x86_64 loader and kernel read.
pub const FW_CFG_FILE: &[u8] = b"opt/org.libersystem.dma-mode";
// The one ESP file the non-x86 runner stages beside the loader, in the firmware API's separator.
pub const ESP_FILE: &str = "EFI\\BOOT\\LSDM";
// The device-tree carrier: a node found by its `compatible`, and the byte-string property on it.
pub const FDT_NODE_NAME: &[u8] = b"libersystem";
pub const FDT_COMPATIBLE: &[u8] = b"libersystem,boot-policy";
pub const FDT_PROPERTY: &[u8] = b"libersystem,dma-mode";

// The two modes a boot can run under. There is no third: absence is the record not being there,
// and it is refused rather than represented.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
	// Every bus-mastering device is translated, and a policy that needs translation gets it. A
	// controller that is absent or did not come up REFUSES every DMA-capable claim on this mode.
	EnforcingRequired,
	// The explicit degraded profile: no controller, and the boot says so. `iommu-required` refuses;
	// `trusted-untranslated` enters the audited degraded inventory; `none` masters nothing.
	NoIommu,
}

impl Mode {
	pub const fn code(self) -> u32 {
		match self {
			Mode::EnforcingRequired => MODE_ENFORCING_REQUIRED,
			Mode::NoIommu => MODE_NO_IOMMU,
		}
	}

	pub const fn from_code(code: u32) -> Option<Mode> {
		match code {
			MODE_ENFORCING_REQUIRED => Some(Mode::EnforcingRequired),
			MODE_NO_IOMMU => Some(Mode::NoIommu),
			_ => None,
		}
	}

	pub const fn name(self) -> &'static str {
		match self {
			Mode::EnforcingRequired => "enforcing-required",
			Mode::NoIommu => "no-iommu",
		}
	}
}

// Why an eight-byte record was refused. Distinguished so an operator can tell a harness that never
// wrote the record from one that wrote it wrongly - and NEVER interpreted: a malformed record's
// mode byte is not read as a mode, which is what stops a corrupt carrier from being a downgrade.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Malformed {
	// Not eight bytes. The length carried is what was found, for the message.
	Length(usize),
	Magic,
	Version(u8),
	Mode(u8),
	// `signed` (1) on a harness carrier is the one value this variant exists for: a replaceable
	// medium claiming authentication. Refused, never believed.
	Provenance(u8),
	Reserved(u8),
}

// The record the harness writes: `LSDM`, version 1, the mode, provenance `harness`, reserved zero.
pub const fn encode_harness(mode: Mode) -> [u8; RECORD_LEN] {
	[RECORD_MAGIC[0], RECORD_MAGIC[1], RECORD_MAGIC[2], RECORD_MAGIC[3], RECORD_VERSION, mode.code() as u8, PROVENANCE_HARNESS as u8, 0]
}

// Read a harness record. Only a well-formed record with `harness` provenance decodes; everything
// else is `Malformed` and says which byte.
pub fn decode_harness(bytes: &[u8]) -> Result<Mode, Malformed> {
	if bytes.len() != RECORD_LEN {
		return Err(Malformed::Length(bytes.len()));
	}
	if bytes[..4] != RECORD_MAGIC {
		return Err(Malformed::Magic);
	}
	if bytes[4] != RECORD_VERSION {
		return Err(Malformed::Version(bytes[4]));
	}
	let mode = match bytes[5] {
		1 => Mode::EnforcingRequired,
		2 => Mode::NoIommu,
		other => return Err(Malformed::Mode(other)),
	};
	if bytes[6] != PROVENANCE_HARNESS as u8 {
		return Err(Malformed::Provenance(bytes[6]));
	}
	if bytes[7] != 0 {
		return Err(Malformed::Reserved(bytes[7]));
	}
	Ok(mode)
}

// What a path's harness input looked like when it was read. `Absent` and `Malformed` reach the same
// decision - refusal - and differ only in what is reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Carrier {
	Absent,
	Malformed(Malformed),
	Valid(Mode),
}

impl Carrier {
	pub fn from_bytes(bytes: Option<&[u8]>) -> Carrier {
		match bytes {
			None => Carrier::Absent,
			Some(bytes) => match decode_harness(bytes) {
				Ok(mode) => Carrier::Valid(mode),
				Err(reason) => Carrier::Malformed(reason),
			},
		}
	}
}

// The mode admission runs under and where it came from - the loader's hand-off, and the value the
// kernel adopts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handoff {
	Signed(Mode),
	Harness(Mode),
}

impl Handoff {
	pub const fn mode(self) -> Mode {
		match self {
			Handoff::Signed(mode) | Handoff::Harness(mode) => mode,
		}
	}

	pub const fn provenance(self) -> u32 {
		match self {
			Handoff::Signed(_) => PROVENANCE_SIGNED,
			Handoff::Harness(_) => PROVENANCE_HARNESS,
		}
	}

	pub const fn provenance_name(self) -> &'static str {
		match self {
			Handoff::Signed(_) => "signed",
			Handoff::Harness(_) => "harness",
		}
	}

	// The two `BootInfo` words, and back. `from_words` is the kernel's validation of what a loader
	// wrote: a pair that does not name a mode and a provenance this module defines is ABSENT, and
	// absent refuses.
	pub const fn words(self) -> (u32, u32) {
		(self.mode().code(), self.provenance())
	}

	pub const fn from_words(mode: u32, provenance: u32) -> Option<Handoff> {
		let mode = match Mode::from_code(mode) {
			Some(mode) => mode,
			None => return None,
		};
		match provenance {
			PROVENANCE_SIGNED => Some(Handoff::Signed(mode)),
			PROVENANCE_HARNESS => Some(Handoff::Harness(mode)),
			_ => None,
		}
	}
}

// Why the boot-wide latch refused. Each names a different mistake by a different party.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LatchRefusal {
	// No manifest was verified at all, so there is no signed set to latch over.
	NothingSelected,
	// The selected set does not agree on whether it carries a mode.
	MixedPresence,
	// Every manifest carries a mode and they are not the same one.
	DifferingValues,
	// Every manifest carries a mode AND a harness carrier is present - two producers, even when
	// they happen to agree.
	SecondProducer,
	// No manifest carries a mode and the path's harness carrier is not there.
	CarrierAbsent,
	// No manifest carries a mode and the carrier is there and is not this record.
	CarrierMalformed(Malformed),
}

impl LatchRefusal {
	pub const fn message(self) -> &'static str {
		match self {
			LatchRefusal::NothingSelected => "no signed manifest was selected, so there is no DMA-mode set to latch",
			LatchRefusal::MixedPresence => "the selected manifests do not agree on whether they carry a DMA mode",
			LatchRefusal::DifferingValues => "the selected manifests carry different DMA modes",
			LatchRefusal::SecondProducer => "the selected manifests carry a signed DMA mode AND a harness carrier is present - two producers",
			LatchRefusal::CarrierAbsent => "the selected manifests carry no DMA mode and this path's harness carrier is absent",
			LatchRefusal::CarrierMalformed(_) => "the selected manifests carry no DMA mode and this path's harness carrier is malformed",
		}
	}
}

// THE BOOT-WIDE EQUALITY LATCH over every manifest a boot verified.
//
// Recorded as each manifest is verified, resolved once before anything is handed to admission - so
// no first-manifest precedence can hide a later contradiction. The set must agree on the presence
// tag; when every tag is 1 the values must agree too and no harness carrier may exist; when every
// tag is 0 exactly the path's valid carrier supplies the mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Latch {
	count: usize,
	// Whether the manifests seen so far carry a mode: `None` until the first one is recorded.
	present: Option<bool>,
	// The agreed mode code, where present.
	mode: Option<u32>,
	// The first contradiction seen. Kept rather than acted on, so the verification that found it
	// finishes and the refusal is reported once, at resolution.
	contradiction: Option<LatchRefusal>,
}

impl Latch {
	pub const fn new() -> Latch {
		Latch { count: 0, present: None, mode: None, contradiction: None }
	}

	// One verified manifest's DMA field: `None` for tag 0, `Some(code)` for tag 1. The codec has
	// already refused a tag-1 value outside the two modes, so `code` is one of them.
	pub fn record(&mut self, dma_mode: Option<u32>) {
		self.count += 1;
		match (self.present, dma_mode) {
			(None, None) => self.present = Some(false),
			(None, Some(code)) => {
				self.present = Some(true);
				self.mode = Some(code);
			}
			(Some(false), None) => {}
			(Some(true), Some(code)) => {
				if self.mode != Some(code) && self.contradiction.is_none() {
					self.contradiction = Some(LatchRefusal::DifferingValues);
				}
			}
			(Some(true), None) | (Some(false), Some(_)) => {
				if self.contradiction.is_none() {
					self.contradiction = Some(LatchRefusal::MixedPresence);
				}
			}
		}
	}

	pub const fn manifests(&self) -> usize {
		self.count
	}

	// The mode this boot runs under, or why it must not.
	pub fn resolve(&self, carrier: Carrier) -> Result<Handoff, LatchRefusal> {
		if let Some(contradiction) = self.contradiction {
			return Err(contradiction);
		}
		match self.present {
			None => Err(LatchRefusal::NothingSelected),
			Some(true) => {
				// A harness carrier beside a signed set is a second producer whether or not it
				// agrees, and whether or not it is well formed.
				if carrier != Carrier::Absent {
					return Err(LatchRefusal::SecondProducer);
				}
				// The codec refused anything but the two modes, so this cannot fail; stated rather
				// than unwrapped, because a latch that panicked in a loader would halt with no line.
				match self.mode.and_then(Mode::from_code) {
					Some(mode) => Ok(Handoff::Signed(mode)),
					None => Err(LatchRefusal::MixedPresence),
				}
			}
			Some(false) => match carrier {
				Carrier::Valid(mode) => Ok(Handoff::Harness(mode)),
				Carrier::Absent => Err(LatchRefusal::CarrierAbsent),
				Carrier::Malformed(reason) => Err(LatchRefusal::CarrierMalformed(reason)),
			},
		}
	}
}

impl Default for Latch {
	fn default() -> Latch {
		Latch::new()
	}
}

// Why the x86_64 kernel refused the loader's hand-off against the `fw_cfg` input it can still read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RelayRefusal {
	// The `BootInfo` words name no mode or no provenance this module defines.
	NoHandoff,
	// `harness` provenance and the input the loader relayed is not there any more.
	InputAbsent,
	// `harness` provenance and the input decodes to something other than the hand-off.
	InputMalformed(Malformed),
	InputDisagrees,
	// `signed` provenance and a harness input is present beside it: an independent producer the
	// loader should have refused, refused here as well.
	IndependentInput,
}

impl RelayRefusal {
	pub const fn message(self) -> &'static str {
		match self {
			RelayRefusal::NoHandoff => "the loader handed over no valid DMA mode",
			RelayRefusal::InputAbsent => "the hand-off says harness provenance and the fw_cfg record it relayed is absent at kernel entry",
			RelayRefusal::InputMalformed(_) => "the hand-off says harness provenance and the fw_cfg record it relayed is malformed",
			RelayRefusal::InputDisagrees => "the hand-off says harness provenance and the fw_cfg record names a different mode",
			RelayRefusal::IndependentInput => "the hand-off says signed provenance and a fw_cfg record is present beside it - an independent producer",
		}
	}
}

// THE x86_64 KERNEL'S RELAY CHECK. The loader relayed the harness input; reading a `fw_cfg` file does
// not consume it, so the kernel revalidates all eight bytes and requires them to name the hand-off.
// With `signed` provenance the input must be absent.
pub fn relay_check(mode: u32, provenance: u32, input: Carrier) -> Result<Handoff, RelayRefusal> {
	let Some(handoff) = Handoff::from_words(mode, provenance) else {
		return Err(RelayRefusal::NoHandoff);
	};
	match handoff {
		Handoff::Signed(_) => {
			if input != Carrier::Absent {
				return Err(RelayRefusal::IndependentInput);
			}
			Ok(handoff)
		}
		Handoff::Harness(expected) => match input {
			Carrier::Absent => Err(RelayRefusal::InputAbsent),
			Carrier::Malformed(reason) => Err(RelayRefusal::InputMalformed(reason)),
			Carrier::Valid(found) if found == expected => Ok(handoff),
			Carrier::Valid(_) => Err(RelayRefusal::InputDisagrees),
		},
	}
}

#[cfg(test)]
mod tests;
