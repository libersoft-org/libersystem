// THE USB PRINTER CLASS, AS PURE DECISIONS (USB Device Class Definition for Printing Devices 1.1): which
// interface setting to drive, the three class requests, and what the port status byte and the device ID
// may say before anything above believes them.
//
// WHICH SETTING. A printer interface is class 7, subclass 1, and its protocol says how it talks: 1 is
// unidirectional (a bulk OUT pipe), 2 is bidirectional (and a bulk IN beside it), 3 is IEEE 1284.4 - a
// packet protocol of its own over the same pipes - and 4 is IPP over USB. This transport moves an
// already-rendered document one way and reads the port status, so it drives 2 where a device offers it,
// 1 otherwise, and never 3 or 4: carrying bytes into a 1284.4 or IPP channel as if it were a plain print
// stream would be a stream the printer reads as garbage.
//
// WHAT IS BELIEVED. The port status byte has three defined bits and five reserved ones, and a device ID
// is length-prefixed: its first two bytes are its whole length, big-endian, themselves included. What
// arrived is handed on as the printer gave it; the length is checked here only so the transport can say
// how much of the page it read was the ID.

use crate::usb_function::{Configuration, Endpoint, Refused, Setting};

pub const CLASS_PRINTER: u8 = 0x07;
pub const SUBCLASS_PRINTER: u8 = 0x01;
pub const PROTOCOL_UNIDIRECTIONAL: u8 = 0x01;
pub const PROTOCOL_BIDIRECTIONAL: u8 = 0x02;

/// The class requests (section 4.2): GET_DEVICE_ID and GET_PORT_STATUS are IN, SOFT_RESET has no data.
pub const REQ_GET_DEVICE_ID: u8 = 0;
pub const REQ_GET_PORT_STATUS: u8 = 1;
pub const REQ_SOFT_RESET: u8 = 2;
pub const RT_CLASS_INTERFACE_IN: u8 = 0xa1;
pub const RT_CLASS_INTERFACE_OUT: u8 = 0x21;

/// The port status bits the class defines; the other five are reserved.
pub const STATUS_PAPER_EMPTY: u8 = 1 << 5;
pub const STATUS_SELECTED: u8 = 1 << 4;
pub const STATUS_NOT_ERROR: u8 = 1 << 3;
pub const STATUS_DEFINED: u8 = STATUS_PAPER_EMPTY | STATUS_SELECTED | STATUS_NOT_ERROR;

/// The longest device ID a transport reads: what the backend contract carries.
pub const MAX_DEVICE_ID: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	/// No setting of the printer class with a protocol this transport speaks.
	NoPrinterInterface,
	/// A printer setting without the bulk OUT pipe every protocol needs, or a bidirectional one without its IN.
	NoBulkPipes,
	Malformed(Refused),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub interface: u8,
	pub alternate: u8,
	pub protocol: u8,
	pub bulk_out: Endpoint,
	/// Present for the bidirectional protocol, which is the one that has it.
	pub bulk_in: Option<Endpoint>,
}

impl Binding {
	/// GET_DEVICE_ID's `wIndex`: the interface in the high byte and the alternate setting in the low one.
	pub fn device_id_index(&self) -> u16 {
		(self.interface as u16) << 8 | self.alternate as u16
	}
}

fn usable(setting: &Setting, protocol: u8) -> Option<(Endpoint, Option<Endpoint>)> {
	if setting.protocol != protocol {
		return None;
	}
	let out = setting.first(Endpoint::is_bulk_out)?;
	match protocol {
		PROTOCOL_BIDIRECTIONAL => Some((out, Some(setting.first(Endpoint::is_bulk_in)?))),
		_ => Some((out, None)),
	}
}

/// The setting this transport drives in one configuration: bidirectional where there is one, else
/// unidirectional.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let printers = || parsed.settings.iter().filter(|setting| setting.is(CLASS_PRINTER, SUBCLASS_PRINTER));
	if printers().all(|setting| setting.protocol != PROTOCOL_BIDIRECTIONAL && setting.protocol != PROTOCOL_UNIDIRECTIONAL) {
		return Err(NotBindable::NoPrinterInterface);
	}
	for protocol in [PROTOCOL_BIDIRECTIONAL, PROTOCOL_UNIDIRECTIONAL] {
		for setting in printers() {
			if let Some((bulk_out, bulk_in)) = usable(setting, protocol) {
				return Ok(Binding { config_value: parsed.value, interface: setting.interface, alternate: setting.alternate, protocol, bulk_out, bulk_in });
			}
		}
	}
	Err(NotBindable::NoBulkPipes)
}

/// How many of `received` bytes are the device ID: its declared length, when that is at least the two
/// length bytes and no more than arrived. `None` for an ID whose length field is not one.
pub fn device_id_length(bytes: &[u8]) -> Option<usize> {
	let declared = u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]) as usize;
	(2..=bytes.len().min(MAX_DEVICE_ID)).contains(&declared).then_some(declared)
}

#[cfg(test)]
mod tests;
