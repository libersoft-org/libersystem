//! RESOURCE DESCRIPTORS: what a `_CRS` buffer says a device holds - memory ranges, ports, wired interrupts - and
//! the CONNECTIONS a device reaches through another: GPIO lines and serial-bus addresses. The bytes are firmware's
//! and are decoded bounded: a descriptor that runs past the buffer ends the list with an error.

use alloc::string::String;
use alloc::vec::Vec;

/// A decoded descriptor.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resource {
	/// A memory range - `Memory24`, `Memory32`, `Memory32Fixed`, or an address-space descriptor of type memory.
	Memory { base: u64, length: u64, writable: bool },
	/// A port range - `IO`, `FixedIO`, or an address-space descriptor of type I/O.
	Io { base: u64, length: u64 },
	/// A bus-number range from an address-space descriptor of type bus.
	BusNumbers { base: u64, length: u64 },
	/// A wired interrupt - `IRQ`, `IRQNoFlags` or `Interrupt` (extended): its line, edge or level, active high or
	/// low, and whether the device consumes it (not a producer's).
	Interrupt { line: u32, level: bool, active_low: bool, shared: bool, wake: bool },
	/// A GPIO connection: `GpioInt` (an interrupt) or `GpioIo`, its pins, the controller named by path, and for
	/// `GpioIo` whether it is restricted to input or output.
	Gpio { interrupt: bool, pins: Vec<u16>, controller: String, level: bool, active_low: bool, restriction: u8, raw: Vec<u8> },
	/// An I2C serial-bus connection: the device's address, the bus speed, and the controller named by path.
	I2c { address: u16, speed: u32, ten_bit: bool, controller: String, raw: Vec<u8> },
	/// Another serial bus (SPI, UART, CSI-2): named, not decoded further.
	SerialBus { kind: u8, controller: String, raw: Vec<u8> },
	/// A DMA channel.
	Dma { channel: u16 },
	/// A generic register (`Register`): an address space id and an address.
	Register { space: u8, address: u64, bit_width: u8 },
	/// Any other descriptor, by its tag, kept for display.
	Other { tag: u8 },
}

/// Why a resource list did not decode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceError {
	/// A descriptor runs past the end of the buffer.
	Truncated(usize),
	/// No end tag before the buffer ended.
	NoEnd,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
	Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// A resource-source string at `at` within a descriptor: bytes up to a null.
fn source_at(descriptor: &[u8], at: usize) -> String {
	descriptor.get(at..).map(|rest| rest.iter().take_while(|&&byte| byte != 0).map(|&byte| byte as char).collect()).unwrap_or_default()
}

/// DECODE a resource template.
pub fn decode(bytes: &[u8]) -> Result<Vec<Resource>, ResourceError> {
	let mut out = Vec::new();
	let mut at = 0usize;
	while at < bytes.len() {
		let tag = bytes[at];
		if tag & 0x80 == 0 {
			// A SMALL descriptor: the name in bits 6:3, the length in bits 2:0.
			let name = (tag >> 3) & 0x0F;
			let length = (tag & 0x07) as usize;
			let body = bytes.get(at + 1..at + 1 + length).ok_or(ResourceError::Truncated(at))?;
			match name {
				0x0F => return Ok(out),
				0x04 => {
					let mask = u16_at(body, 0).unwrap_or(0);
					let flags = body.get(2).copied().unwrap_or(0x01);
					for line in (0..16).filter(|bit| mask & (1 << bit) != 0) {
						out.push(Resource::Interrupt { line, level: flags & 0x01 == 0, active_low: flags & 0x08 != 0, shared: flags & 0x10 != 0, wake: flags & 0x20 != 0 });
					}
				}
				0x05 => {
					let mask = body.first().copied().unwrap_or(0);
					for channel in (0..8).filter(|bit| mask & (1 << bit) != 0) {
						out.push(Resource::Dma { channel });
					}
				}
				0x08 => {
					let minimum = u16_at(body, 1).unwrap_or(0) as u64;
					let length = body.get(6).copied().unwrap_or(0) as u64;
					out.push(Resource::Io { base: minimum, length });
				}
				0x09 => {
					let base = (u16_at(body, 0).unwrap_or(0) & 0x03FF) as u64;
					let length = body.get(2).copied().unwrap_or(0) as u64;
					out.push(Resource::Io { base, length });
				}
				0x0A => out.push(Resource::Dma { channel: u16_at(body, 0).unwrap_or(0) }),
				other => out.push(Resource::Other { tag: other }),
			}
			at += 1 + length;
		} else {
			let length = u16_at(bytes, at + 1).ok_or(ResourceError::Truncated(at))? as usize;
			let descriptor = bytes.get(at..at + 3 + length).ok_or(ResourceError::Truncated(at))?;
			let body = &descriptor[3..];
			match tag {
				0x81 => {
					let base = (u16_at(body, 1).unwrap_or(0) as u64) << 8;
					let length = (u16_at(body, 7).unwrap_or(0) as u64) << 8;
					out.push(Resource::Memory { base, length, writable: body.first().is_some_and(|info| info & 1 != 0) });
				}
				0x82 => out.push(Resource::Register { space: body.first().copied().unwrap_or(0), bit_width: body.get(1).copied().unwrap_or(0), address: u64_at(body, 4).unwrap_or(0) }),
				0x85 => out.push(Resource::Memory { base: u32_at(body, 1).unwrap_or(0) as u64, length: u32_at(body, 13).unwrap_or(0) as u64, writable: body.first().is_some_and(|info| info & 1 != 0) }),
				0x86 => out.push(Resource::Memory { base: u32_at(body, 1).unwrap_or(0) as u64, length: u32_at(body, 5).unwrap_or(0) as u64, writable: body.first().is_some_and(|info| info & 1 != 0) }),
				0x87 | 0x88 | 0x8A | 0x8B => {
					let kind = body.first().copied().unwrap_or(0);
					// After the type and the flags: granularity, minimum, maximum, translation, length - each the
					// descriptor's width; the extended form has a revision and a reserved byte first.
					let (minimum, length) = match tag {
						0x88 => (u16_at(body, 5).unwrap_or(0) as u64, u16_at(body, 11).unwrap_or(0) as u64),
						0x87 => (u32_at(body, 7).unwrap_or(0) as u64, u32_at(body, 19).unwrap_or(0) as u64),
						0x8A => (u64_at(body, 11).unwrap_or(0), u64_at(body, 35).unwrap_or(0)),
						_ => (u64_at(body, 13).unwrap_or(0), u64_at(body, 37).unwrap_or(0)),
					};
					// A PRODUCER'S range - a bridge's window - is not the device's own: general flags bit 0 set is a
					// consumer, clear a producer.
					let consumer = body.get(1).is_some_and(|flags| flags & 0x01 != 0);
					match kind {
						0 if consumer => out.push(Resource::Memory { base: minimum, length, writable: true }),
						1 if consumer => out.push(Resource::Io { base: minimum, length }),
						2 => out.push(Resource::BusNumbers { base: minimum, length }),
						_ => out.push(Resource::Other { tag }),
					}
				}
				0x89 => {
					let flags = body.first().copied().unwrap_or(0);
					let count = body.get(1).copied().unwrap_or(0) as usize;
					for index in 0..count {
						let Some(line) = u32_at(body, 2 + 4 * index) else { return Err(ResourceError::Truncated(at)) };
						// Bit 0 set: the device CONSUMES the interrupt; a producer's is not a device resource.
						if flags & 0x01 != 0 {
							out.push(Resource::Interrupt { line, level: flags & 0x02 == 0, active_low: flags & 0x04 != 0, shared: flags & 0x08 != 0, wake: flags & 0x10 != 0 });
						}
					}
				}
				0x8C => {
					// THE LAYOUT: the connection type at 4, the interrupt-or-I/O flags at 7, the pin table's offset at
					// 14 and the controller's name's at 17 - both from the descriptor's start.
					let interrupt = body.get(1).copied().unwrap_or(0) == 0;
					let flags = u16_at(body, 4).unwrap_or(0);
					let restriction = if interrupt { 0 } else { (flags & 0x03) as u8 };
					let pin_offset = u16_at(body, 11).unwrap_or(0) as usize;
					let source_offset = u16_at(body, 14).unwrap_or(0) as usize;
					let pins: Vec<u16> = (pin_offset..source_offset).step_by(2).filter_map(|offset| u16_at(descriptor, offset)).collect();
					let controller = source_at(descriptor, source_offset);
					// A GpioInt's mode is bit 0 (0 level, 1 edge) and its polarity bits 2:1 (1 active low).
					let (level, active_low) = if interrupt { (flags & 0x01 == 0, (flags >> 1) & 0x03 == 1) } else { (false, false) };
					out.push(Resource::Gpio { interrupt, pins, controller, level, active_low, restriction, raw: descriptor.to_vec() });
				}
				0x8E => {
					let kind = body.get(2).copied().unwrap_or(0);
					let type_data_length = u16_at(body, 7).unwrap_or(0) as usize;
					let controller = source_at(descriptor, 12 + type_data_length);
					if kind == 1 {
						let speed = u32_at(body, 9).unwrap_or(0);
						let address = u16_at(body, 13).unwrap_or(0);
						let ten_bit = u16_at(body, 4).is_some_and(|flags| flags & 0x01 != 0);
						out.push(Resource::I2c { address, speed, ten_bit, controller, raw: descriptor.to_vec() });
					} else {
						out.push(Resource::SerialBus { kind, controller, raw: descriptor.to_vec() });
					}
				}
				other => out.push(Resource::Other { tag: other }),
			}
			at += 3 + length;
		}
	}
	Err(ResourceError::NoEnd)
}

/// The pins of a GPIO descriptor in its raw bytes, for a GeneralPurposeIo field's connection.
pub fn gpio_pins(raw: &[u8]) -> Option<Vec<u16>> {
	match decode_one(raw)? {
		Resource::Gpio { pins, .. } => Some(pins),
		_ => None,
	}
}

/// One descriptor at the start of `raw` - a field connection's buffer, which carries its own end tag or none.
pub fn decode_one(raw: &[u8]) -> Option<Resource> {
	let mut bytes = raw.to_vec();
	if !(bytes.len() >= 2 && bytes[bytes.len() - 2] == 0x79) {
		bytes.extend_from_slice(&[0x79, 0x00]);
	}
	decode(&bytes).ok()?.into_iter().next()
}

/// A compressed EISA id - `_HID` as an integer - in its text form: `PNP0A08`.
pub fn eisa_id(value: u32) -> String {
	let swapped = value.swap_bytes();
	let letter = |shift: u32| (((swapped >> shift) & 0x1F) as u8 + b'@') as char;
	let mut text = String::new();
	text.push(letter(26));
	text.push(letter(21));
	text.push(letter(16));
	for shift in [12, 8, 4, 0] {
		text.push(core::char::from_digit((swapped >> shift) & 0xF, 16).unwrap_or('0').to_ascii_uppercase());
	}
	text
}

/// The inverse: `PNP0A08` as the integer ASL's `EISAID` makes of it.
pub fn eisa_value(text: &str) -> Option<u32> {
	let bytes = text.as_bytes();
	if bytes.len() != 7 || !bytes[..3].iter().all(|byte| byte.is_ascii_uppercase()) {
		return None;
	}
	let mut value: u32 = 0;
	for &letter in &bytes[..3] {
		value = (value << 5) | (letter - b'@') as u32;
	}
	for &digit in &bytes[3..] {
		value = (value << 4) | (digit as char).to_digit(16)?;
	}
	Some(value.swap_bytes())
}
