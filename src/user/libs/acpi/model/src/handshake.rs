//! THE HANDSHAKES, each before what it grants is used. Firmware gates its paths on what the OS says it supports, so
//! each capability set here is exactly what this system implements - never more.
//!
//!   - every PCI Express host bridge's `_OSC` requests native hot-plug, AER, PME, capability-structure and LTR
//!     control; the answer is reported to the kernel, which arms native hot-plug and AER only for what it grants - a
//!     bridge without `_OSC` grants nothing;
//!   - the platform-wide `\_SB._OSC` requests CPPC and CPPC v2, `_OST`, power resources and platform-coordinated
//!     `_LPI`;
//!   - each processor's `_OSC` - `_PDC` where there is none - declares exactly the forms the kernel's processor power
//!     management executes: system I/O and system memory registers and x86 MWAIT hints, no model-specific-register
//!     forms - so firmware never hands over a table the kernel refuses. It runs before `_PSS`, `_CST` or `_CPC` is
//!     read, which is when Intel firmware `Load`s its processor SSDTs.

use platform::report::osc;

/// The PCI host bridge `_OSC` UUID.
pub const PCI_HOST_BRIDGE: &str = "33db4d5b-1ff7-401c-9657-7441c03dd766";
/// The platform-wide `\_SB._OSC` UUID.
pub const PLATFORM_WIDE: &str = "0811b06e-4a27-44f9-8d60-3cbbc22e7b48";
/// The processor `_OSC` (and `_PDC`) UUID.
pub const PROCESSOR: &str = "4077a616-290c-47be-9ebd-d87058713953";

/// `_OSC` status dword (the first): a query, and the failures the firmware answers with.
pub mod status {
	pub const QUERY: u32 = 1 << 0;
	pub const FAILURE: u32 = 1 << 1;
	pub const UNRECOGNIZED_UUID: u32 = 1 << 2;
	pub const UNRECOGNIZED_REVISION: u32 = 1 << 3;
	pub const CAPABILITIES_MASKED: u32 = 1 << 4;
}

/// The PCI support dword: what this OS supports - extended configuration space, ASPM, clock power management,
/// segments, MSI.
pub const PCI_SUPPORT: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4);
/// The PCI control dword requested.
pub const PCI_CONTROL: u32 = osc::HOT_PLUG | osc::PME | osc::AER | osc::CAPABILITY | osc::LTR;

/// `_PPC` `_OST`, `_PR3` (power resources), hot-plug `_OST`, CPPC, CPPC v2, platform-coordinated `_LPI`.
pub const PLATFORM_SUPPORT: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 5) | (1 << 6) | (1 << 7);

/// THE PROCESSOR FORMS: C1 as a halt, SMP C1 and C2/C3 power-down, software coordination of P, C and T states, C1 and
/// C2/C3 through MWAIT hints, and `_PPC` notification. NOT the P-state and T-state fixed-hardware forms - those are
/// model-specific registers, which the kernel does not execute.
pub const PROCESSOR_SUPPORT: u32 = 0x0002 | 0x0008 | 0x0010 | 0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0200 | 0x1000;

/// The dwords a host bridge's `_OSC` is called with: status, support, control.
pub fn pci_request() -> [u32; 3] {
	[0, PCI_SUPPORT, PCI_CONTROL]
}

/// WHAT A HOST BRIDGE'S `_OSC` ANSWER GRANTS: the control dword it returned, masked to what was asked - nothing when
/// the firmware refused the UUID or the revision, failed, or did not answer the three dwords. `None` - no `_OSC` at
/// all - grants nothing either.
pub fn pci_granted(answer: Option<&[u32]>) -> u32 {
	let Some(answer) = answer else { return 0 };
	if answer.len() < 3 {
		return 0;
	}
	if answer[0] & (status::FAILURE | status::UNRECOGNIZED_UUID | status::UNRECOGNIZED_REVISION) != 0 {
		return 0;
	}
	answer[2] & PCI_CONTROL
}

/// The dwords `\_SB._OSC` is called with: status and support.
pub fn platform_request() -> [u32; 2] {
	[0, PLATFORM_SUPPORT]
}

/// What the platform-wide answer agreed to, masked to what was asked.
pub fn platform_granted(answer: Option<&[u32]>) -> u32 {
	match answer {
		Some(answer) if answer.len() >= 2 && answer[0] & (status::FAILURE | status::UNRECOGNIZED_UUID | status::UNRECOGNIZED_REVISION) == 0 => answer[1] & PLATFORM_SUPPORT,
		_ => 0,
	}
}

/// The dwords a processor's `_OSC` is called with: status and the forms.
pub fn processor_request() -> [u32; 2] {
	[0, PROCESSOR_SUPPORT]
}

/// The `_PDC` buffer where a processor has no `_OSC`: revision 1, one dword, the same forms.
pub fn pdc_buffer() -> [u8; 12] {
	let mut out = [0u8; 12];
	out[0..4].copy_from_slice(&1u32.to_le_bytes());
	out[4..8].copy_from_slice(&1u32.to_le_bytes());
	out[8..12].copy_from_slice(&PROCESSOR_SUPPORT.to_le_bytes());
	out
}

/// The control bits in words, for the service's line.
pub fn pci_control_text(granted: u32) -> alloc::string::String {
	let mut out = alloc::string::String::new();
	for (bit, name) in [(osc::HOT_PLUG, "hot-plug"), (osc::PME, "PME"), (osc::AER, "AER"), (osc::CAPABILITY, "capability-structure"), (osc::LTR, "LTR")] {
		if granted & bit != 0 {
			if !out.is_empty() {
				out.push_str(", ");
			}
			out.push_str(name);
		}
	}
	if out.is_empty() {
		out.push_str("nothing");
	}
	out
}
