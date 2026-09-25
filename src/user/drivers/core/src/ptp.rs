// THE USB STILL IMAGE CLASS AS A TRANSPORT (USB Still Image Capture Device Definition 1.0, which carries PTP -
// ISO 15740 - and the MTP superset of it): which interface setting, the four class requests, and the device
// status the host reads after a cancel.
//
// A TRANSPORT PARSES NO CONTAINER. MediaImportService validates every container's length, type, code and
// transaction; what this side reads of the bytes it carries is exactly one field - the transaction ID of the
// last command, which the class's Cancel Request has to name - and it bounds every frame before copying it.

use crate::usb_function::{Configuration, Endpoint, Refused};
use alloc::vec::Vec;

pub const CLASS_STILL_IMAGE: u8 = 0x06;
pub const SUBCLASS_STILL_IMAGE: u8 = 0x01;
pub const PROTOCOL_PTP: u8 = 0x01;

/// The class requests (section 5.2): Cancel and Device Reset go OUT to the interface, the status comes IN.
pub const REQ_CANCEL: u8 = 0x64;
pub const REQ_DEVICE_RESET: u8 = 0x66;
pub const REQ_GET_DEVICE_STATUS: u8 = 0x67;
pub const RT_CLASS_INTERFACE_OUT: u8 = 0x21;
pub const RT_CLASS_INTERFACE_IN: u8 = 0xa1;
/// The cancellation code a Cancel Request's data carries.
pub const CANCEL_CODE: u16 = 0x4001;
/// Response codes a device status may carry.
pub const STATUS_OK: u16 = 0x2001;
pub const STATUS_DEVICE_BUSY: u16 = 0x2019;
pub const STATUS_TRANSACTION_CANCELLED: u16 = 0x201f;

/// The largest command container the contract carries, and the largest event.
pub const MAX_COMMAND: usize = 32;
pub const MAX_EVENT: usize = 32;
/// A device status is its length and code and at most a handful of stalled endpoints.
pub const MAX_STATUS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	NoStillImageInterface,
	/// A still image setting without its bulk pair and its event pipe.
	NoPipes,
	Malformed(Refused),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub interface: u8,
	pub alternate: u8,
	pub bulk_in: Endpoint,
	pub bulk_out: Endpoint,
	pub interrupt_in: Endpoint,
}

/// The still image setting in one configuration: a bulk pair and an interrupt IN for events.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	if !parsed.settings.iter().any(|setting| setting.is(CLASS_STILL_IMAGE, SUBCLASS_STILL_IMAGE) && setting.protocol == PROTOCOL_PTP) {
		return Err(NotBindable::NoStillImageInterface);
	}
	for setting in parsed.settings.iter().filter(|setting| setting.is(CLASS_STILL_IMAGE, SUBCLASS_STILL_IMAGE) && setting.protocol == PROTOCOL_PTP) {
		if let (Some(bulk_in), Some(bulk_out), Some(interrupt_in)) = (setting.first(Endpoint::is_bulk_in), setting.first(Endpoint::is_bulk_out), setting.first(Endpoint::is_interrupt_in)) {
			return Ok(Binding { config_value: parsed.value, interface: setting.interface, alternate: setting.alternate, bulk_in, bulk_out, interrupt_in });
		}
	}
	Err(NotBindable::NoPipes)
}

/// The transaction a command container names, which is the one a cancel has to name. Zero for a container
/// too short to carry one - the service refuses such a command before it is sent anyway.
pub fn transaction(container: &[u8]) -> u32 {
	match container.get(8..12) {
		Some(field) => u32::from_le_bytes([field[0], field[1], field[2], field[3]]),
		None => 0,
	}
}

/// A Cancel Request's six bytes: the cancellation code and the transaction.
pub fn cancel_request(transaction: u32) -> [u8; 6] {
	let mut out = [0u8; 6];
	out[..2].copy_from_slice(&CANCEL_CODE.to_le_bytes());
	out[2..].copy_from_slice(&transaction.to_le_bytes());
	out
}

/// What a Get Device Status answered: its code, and the endpoints it says are stalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceStatus {
	pub code: u16,
	pub stalled: Vec<u8>,
}

/// A device status, believed only as far as its own length and the transfer agree: a length under the four
/// bytes of length and code, past what arrived, or with a parameter cut in half is not a status.
pub fn device_status(bytes: &[u8]) -> Option<DeviceStatus> {
	let length = u16::from_le_bytes([*bytes.first()?, *bytes.get(1)?]) as usize;
	if length < 4 || length > bytes.len() || length > MAX_STATUS || (length - 4) % 4 != 0 {
		return None;
	}
	let code = u16::from_le_bytes([bytes[2], bytes[3]]);
	// Each parameter is an endpoint address in the low byte of a 32-bit field.
	let stalled = bytes[4..length].chunks_exact(4).map(|param| param[0]).collect();
	Some(DeviceStatus { code, stalled })
}

#[cfg(test)]
mod tests;
