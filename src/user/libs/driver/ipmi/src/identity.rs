//! WHO THE BMC IS: Get Device ID (App 0x01) and Get Device GUID (App 0x08), and the name the system gives it.
//!
//! THE BMC'S IDENTITY IS ITS GUID. IPMI makes Get Device GUID optional, and a BMC that answers it with an error, or with
//! sixteen zero bytes, has none: it is named by its manufacturer, product and device ID TOGETHER WITH ITS BINDING - the
//! PCI address or the firmware path of the device its interface is bound to - so two BMCs without a GUID are never
//! taken for one. The name is `bmc:` and the GUID's 32 hex digits, or `bmc:` and that fallback, within the
//! administrative selector's 64 bytes; a longer fallback leaves the BMC without an administrable name, which is said
//! rather than truncated into someone else's.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const GET_DEVICE_ID: u8 = 0x01;
pub const GET_DEVICE_GUID: u8 = 0x08;

/// The administrative selector's bound.
pub const SELECTOR_MAX: usize = 64;
/// Every BMC's name begins with this.
pub const PREFIX: &str = "bmc:";

/// Get Device ID's answer.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeviceId {
	pub device_id: u8,
	pub device_revision: u8,
	/// Whether the device provides device SDRs.
	pub provides_sdrs: bool,
	/// Whether the firmware is in normal operation (bit 7 of the firmware revision clear).
	pub available: bool,
	pub firmware_major: u8,
	/// The firmware's minor revision, as the BCD byte it is sent as.
	pub firmware_minor: u8,
	/// The IPMI version: major, minor.
	pub ipmi: (u8, u8),
	/// Additional device support: bit 0 sensor device, 1 SDR repository, 2 SEL, 3 FRU inventory, 4 IPMB event
	/// receiver, 5 IPMB event generator, 6 bridge, 7 chassis.
	pub support: u8,
	/// The IANA enterprise number, twenty bits.
	pub manufacturer: u32,
	pub product: u16,
}

pub mod support {
	pub const SENSOR: u8 = 1 << 0;
	pub const SDR_REPOSITORY: u8 = 1 << 1;
	pub const SEL: u8 = 1 << 2;
	pub const FRU: u8 = 1 << 3;
	pub const CHASSIS: u8 = 1 << 7;
}

/// Decode Get Device ID's data (after the completion code): eleven bytes, the auxiliary revision optional.
pub fn device_id(data: &[u8]) -> Option<DeviceId> {
	if data.len() < 11 {
		return None;
	}
	Some(DeviceId { device_id: data[0], device_revision: data[1] & 0x0F, provides_sdrs: data[1] & 0x80 != 0, available: data[2] & 0x80 == 0, firmware_major: data[2] & 0x7F, firmware_minor: data[3], ipmi: (data[4] & 0x0F, data[4] >> 4), support: data[5], manufacturer: u32::from_le_bytes([data[6], data[7], data[8], 0]) & 0x000F_FFFF, product: u16::from_le_bytes([data[9], data[10]]) })
}

/// Decode Get Device GUID's data: sixteen bytes, of which all zero is no GUID at all.
pub fn guid(data: &[u8]) -> Option<[u8; 16]> {
	let bytes: [u8; 16] = data.get(..16)?.try_into().ok()?;
	(bytes != [0; 16]).then_some(bytes)
}

/// A BMC's identity: its GUID, or the fallback of its device identity and its binding.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Identity {
	Guid([u8; 16]),
	/// Manufacturer, product, device ID - and the binding it was reached through.
	Fallback {
		manufacturer: u32,
		product: u16,
		device_id: u8,
		binding: String,
	},
}

impl Identity {
	/// The identity from what the BMC answered: its GUID when it gave one, or the fallback.
	pub fn of(device: &DeviceId, guid: Option<[u8; 16]>, binding: &str) -> Identity {
		match guid {
			Some(guid) => Identity::Guid(guid),
			None => Identity::Fallback { manufacturer: device.manufacturer, product: device.product, device_id: device.device_id, binding: String::from(binding) },
		}
	}

	/// The name: `bmc:` and 32 hex digits, or `bmc:` and the fallback - whatever its length.
	pub fn name(&self) -> String {
		match self {
			Identity::Guid(guid) => {
				let mut out = String::from(PREFIX);
				for byte in guid {
					out.push_str(&format!("{byte:02x}"));
				}
				out
			}
			Identity::Fallback { manufacturer, product, device_id, binding } => format!("{PREFIX}{manufacturer:05x}-{product:04x}-{device_id:02x}@{binding}"),
		}
	}

	/// THE ADMINISTRATIVE SELECTOR: the name, when it fits the selector's 64 bytes. None for a fallback past it.
	pub fn selector(&self) -> Option<Vec<u8>> {
		let name = self.name();
		(name.len() <= SELECTOR_MAX).then(|| name.into_bytes())
	}

	/// Whether two bindings reach ONE BMC: only BMCs that answer a GUID can form a pair.
	pub fn same_bmc(&self, other: &Identity) -> bool {
		matches!((self, other), (Identity::Guid(a), Identity::Guid(b)) if a == b)
	}
}
