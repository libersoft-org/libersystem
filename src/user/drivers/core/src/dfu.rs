// THE USB DEVICE FIRMWARE UPGRADE CLASS AS PURE DECISIONS (USB DFU 1.1): which interface is a DFU target and in
// which mode, what its functional descriptor allows, what a status answer says, and what an image's DFU suffix
// claims before a byte of the image is believed.
//
// TWO MODES, AND A DOWNLOAD CROSSES FROM ONE TO THE OTHER. A device in RUNTIME mode (protocol 1) is running its
// application and has to be detached and re-enumerated before it takes an image; a device in DFU MODE (protocol
// 2) takes the download. The device that comes back is a new attachment, so nothing prepared against the runtime
// device survives it - EXCEPT the one operation whose confirmed execution sent the detach: that one is carried
// across by the executor that sent it, and only to a device that comes back in DFU mode where the runtime device
// was, carrying its serial number if it had one (`same_device`). Anything else that arrives there is not it.
//
// THE SUFFIX IS THE ONLY METADATA THE CLASS HAS. DFU 1.1 files end in sixteen bytes naming the device, the vendor
// and the product the image is for, and a CRC over everything before it. Where an image carries one it is held
// against the target and checked, and a mismatch refuses the image; where it carries none there is nothing to
// validate, which is what "where available" means - and the image goes as the requester gave it.

use crate::usb_function::{Configuration, Refused};
use alloc::vec::Vec;

pub const CLASS_APPLICATION: u8 = 0xfe;
pub const SUBCLASS_DFU: u8 = 0x01;
pub const PROTOCOL_RUNTIME: u8 = 0x01;
pub const PROTOCOL_DFU_MODE: u8 = 0x02;
pub const DT_DFU_FUNCTIONAL: u8 = 0x21;

pub const REQ_DETACH: u8 = 0;
pub const REQ_DNLOAD: u8 = 1;
pub const REQ_UPLOAD: u8 = 2;
pub const REQ_GETSTATUS: u8 = 3;
pub const REQ_CLRSTATUS: u8 = 4;
pub const REQ_GETSTATE: u8 = 5;
pub const REQ_ABORT: u8 = 6;
pub const RT_CLASS_INTERFACE_OUT: u8 = 0x21;
pub const RT_CLASS_INTERFACE_IN: u8 = 0xa1;

pub const ATTR_CAN_DOWNLOAD: u8 = 1 << 0;
pub const ATTR_CAN_UPLOAD: u8 = 1 << 1;
pub const ATTR_MANIFESTATION_TOLERANT: u8 = 1 << 2;
pub const ATTR_WILL_DETACH: u8 = 1 << 3;

// The states a status answer names.
pub const STATE_APP_IDLE: u8 = 0;
pub const STATE_APP_DETACH: u8 = 1;
pub const STATE_IDLE: u8 = 2;
pub const STATE_DNLOAD_SYNC: u8 = 3;
pub const STATE_DNBUSY: u8 = 4;
pub const STATE_DNLOAD_IDLE: u8 = 5;
pub const STATE_MANIFEST_SYNC: u8 = 6;
pub const STATE_MANIFEST: u8 = 7;
pub const STATE_MANIFEST_WAIT_RESET: u8 = 8;
pub const STATE_UPLOAD_IDLE: u8 = 9;
pub const STATE_ERROR: u8 = 10;
pub const STATUS_OK: u8 = 0;

/// The smallest and largest block a transport sends, whatever the device declares.
pub const MIN_TRANSFER: u16 = 8;
pub const MAX_TRANSFER: u16 = 4096;
pub const SUFFIX: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	Runtime,
	Dfu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	NoDfuInterface,
	/// A DFU interface with no functional descriptor, or one too short to read.
	NoFunctionalDescriptor,
	/// A transfer size a transport cannot use.
	BadTransferSize,
	Malformed(Refused),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub interface: u8,
	pub alternate: u8,
	pub mode: Mode,
	pub attributes: u8,
	pub detach_timeout_ms: u16,
	pub transfer_size: u16,
	pub version: u16,
}

impl Binding {
	/// Whether the device takes a download in DFU mode - which a runtime device's descriptor says of the mode it
	/// detaches into.
	pub fn can_download(&self) -> bool {
		self.attributes & ATTR_CAN_DOWNLOAD != 0
	}

	/// Whether the device leaves the bus by itself after a detach, rather than waiting for the host to reset it.
	pub fn will_detach(&self) -> bool {
		self.attributes & ATTR_WILL_DETACH != 0
	}

	/// How long a detached runtime device is given to come back in DFU mode: its own detach timeout, bounded,
	/// and the five seconds a re-enumeration may take.
	pub fn return_window_ms(&self) -> u64 {
		u64::from(self.detach_timeout_ms).clamp(100, 5_000) + 5_000
	}
}

/// Where a device is, and what it calls itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
	pub port: u32,
	pub route: u32,
	pub serial: Option<&'a str>,
}

/// WHETHER THE DEVICE THAT CAME BACK IS THE ONE THAT DETACHED. The product id may change - a DFU-mode device is
/// often another product - so it proves nothing either way. What does: the same place on the bus, and, where the
/// runtime device had a serial number, the same serial number. A runtime device with none is followed by place
/// alone, which is weaker and is what such a device leaves; a device that comes back with a serial where there was
/// none, or another one, is not it.
pub fn same_device(detached: &Identity<'_>, returned: &Identity<'_>) -> bool {
	detached.port == returned.port && detached.route == returned.route && detached.serial == returned.serial
}

/// The DFU interface of one configuration.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let setting = parsed.settings.iter().find(|setting| setting.is(CLASS_APPLICATION, SUBCLASS_DFU) && matches!(setting.protocol, PROTOCOL_RUNTIME | PROTOCOL_DFU_MODE)).ok_or(NotBindable::NoDfuInterface)?;
	let functional = parsed.functional(setting).find(|record| record.kind == DT_DFU_FUNCTIONAL && record.len() >= 7).ok_or(NotBindable::NoFunctionalDescriptor)?;
	let attributes = functional.field(2).map_err(|_| NotBindable::NoFunctionalDescriptor)?;
	let detach_timeout_ms = functional.field16(3).map_err(|_| NotBindable::NoFunctionalDescriptor)?;
	let transfer_size = functional.field16(5).map_err(|_| NotBindable::NoFunctionalDescriptor)?;
	// DFU 1.0 descriptors stop before the version, which is then 1.0.
	let version = functional.field16(7).unwrap_or(0x0100);
	if transfer_size < MIN_TRANSFER {
		return Err(NotBindable::BadTransferSize);
	}
	let mode = if setting.protocol == PROTOCOL_DFU_MODE { Mode::Dfu } else { Mode::Runtime };
	Ok(Binding { config_value: parsed.value, interface: setting.interface, alternate: setting.alternate, mode, attributes, detach_timeout_ms, transfer_size: transfer_size.min(MAX_TRANSFER), version })
}

/// A GETSTATUS answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
	pub status: u8,
	pub poll_timeout_ms: u32,
	pub state: u8,
}

/// Six bytes, or nothing a transport believes.
pub fn status(bytes: &[u8]) -> Option<Status> {
	if bytes.len() < 6 || bytes[4] > STATE_ERROR {
		return None;
	}
	Some(Status { status: bytes[0], poll_timeout_ms: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], 0]), state: bytes[4] })
}

/// The CRC a DFU suffix carries: CRC-32 over everything before the CRC field, without the final inversion - as the
/// reference tools compute it.
pub fn suffix_crc(bytes: &[u8]) -> u32 {
	let mut crc: u32 = 0xffff_ffff;
	for &byte in bytes {
		crc ^= byte as u32;
		for _ in 0..8 {
			crc = if crc & 1 != 0 { crc >> 1 ^ 0xedb8_8320 } else { crc >> 1 };
		}
	}
	crc
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuffixRefused {
	/// The suffix names another vendor or product than the target.
	WrongDevice,
	/// Its CRC does not match what precedes it.
	Crc,
	/// It declares a length that is not a suffix's, or leaves no image.
	Malformed,
}

/// The image a payload carries, for a target with this vendor and product: the payload less its suffix when it
/// has a valid one for this target, the payload itself when it has none, and a refusal when it has one that does
/// not hold.
pub fn image(payload: &[u8], vendor: u16, product: u16) -> Result<&[u8], SuffixRefused> {
	if payload.len() < SUFFIX || &payload[payload.len() - 8..payload.len() - 5] != b"UFD" {
		return Ok(payload);
	}
	let suffix = &payload[payload.len() - SUFFIX..];
	if suffix[11] as usize != SUFFIX || payload.len() == SUFFIX {
		return Err(SuffixRefused::Malformed);
	}
	let said_product = u16::from_le_bytes([suffix[2], suffix[3]]);
	let said_vendor = u16::from_le_bytes([suffix[4], suffix[5]]);
	// 0xffff names any product, or any vendor.
	if (said_vendor != 0xffff && said_vendor != vendor) || (said_product != 0xffff && said_product != product) {
		return Err(SuffixRefused::WrongDevice);
	}
	let crc = u32::from_le_bytes([suffix[12], suffix[13], suffix[14], suffix[15]]);
	if suffix_crc(&payload[..payload.len() - 4]) != crc {
		return Err(SuffixRefused::Crc);
	}
	Ok(&payload[..payload.len() - SUFFIX])
}

/// A suffix for `image`, for a test to build a payload with.
pub fn with_suffix(image: &[u8], vendor: u16, product: u16) -> Vec<u8> {
	let mut out = image.to_vec();
	out.extend_from_slice(&[0xff, 0xff]);
	out.extend_from_slice(&product.to_le_bytes());
	out.extend_from_slice(&vendor.to_le_bytes());
	out.extend_from_slice(&[0x00, 0x01]);
	out.extend_from_slice(b"UFD");
	out.push(SUFFIX as u8);
	let crc = suffix_crc(&out);
	out.extend_from_slice(&crc.to_le_bytes());
	out
}

#[cfg(test)]
mod tests;
