// ONE CONFIGURATION DESCRIPTOR, READ ONCE INTO ITS INTERFACE SETTINGS: every alternate setting with its
// endpoints, and where the class-specific records around them are.
//
// WHY A SHARED WALK. Nine class modules bind by the same three facts - which interface setting carries the
// class, which endpoints it has, and which class-specific records sit between them - and each one walking
// the records itself is nine places for a record that is too short, or an endpoint that comes before any
// interface, to be read as something else. So the walk is done once, here, and a class binder reads a
// `Configuration`.
//
// WHAT IS REFUSED, AND REFUSES THE WHOLE CONFIGURATION: a first record that is not a configuration, a record
// whose declared length runs past the transfer or is under two, an interface or endpoint record shorter than
// its fixed part, an endpoint before any interface, an endpoint numbered zero, one address twice in one
// setting, and more settings or endpoints than a device can have. `bNumEndpoints` is NOT held against the
// endpoints that follow - Linux reads what is there and says so, and a device that miscounts its own
// endpoints is common enough that refusing it would refuse real hardware for a byte nothing uses.
//
// WHAT IS KEPT AS BYTES: the records between an interface descriptor and its first endpoint (a class's
// functional descriptors), and the records after each endpoint (a class-specific endpoint descriptor, like
// USB-MIDI's jack count). They are handed back as a `descriptor::Walk`, so a binder reads them with the same
// bounds-checked accessors as everything else.

use crate::descriptor::{self, DescriptorFault, Walk};
use alloc::vec::Vec;

pub const TRANSFER_CONTROL: u8 = 0;
pub const TRANSFER_ISOCHRONOUS: u8 = 1;
pub const TRANSFER_BULK: u8 = 2;
pub const TRANSFER_INTERRUPT: u8 = 3;
/// The interface association descriptor, which groups interfaces into one function.
pub const DT_INTERFACE_ASSOCIATION: u8 = 11;
/// A class-specific interface or endpoint record.
pub const DT_CS_INTERFACE: u8 = 0x24;
pub const DT_CS_ENDPOINT: u8 = 0x25;

/// Settings one configuration may describe: sixteen interfaces with a spare alternate each, which no real
/// function this driver binds comes near.
pub const MAX_SETTINGS: usize = 32;
/// Endpoints one setting may carry: fifteen numbers in each direction.
pub const MAX_ENDPOINTS: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
	/// The first record is not a configuration descriptor.
	NotConfiguration,
	/// A record is shorter than its type's fixed part, or out of place.
	Malformed,
	/// The record list itself could not be walked.
	Walk(DescriptorFault),
	/// An endpoint numbered zero, or one address twice in one setting.
	BadEndpoint,
	/// More settings or endpoints than any device has.
	TooMany,
}

/// One endpoint descriptor, and where the class-specific records after it are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
	pub address: u8,
	pub attributes: u8,
	/// `wMaxPacketSize` as the device wrote it: bits 10:0 are the size, 12:11 the high-bandwidth count.
	pub packet: u16,
	pub interval: u8,
	extra: (usize, usize),
}

impl Endpoint {
	pub fn is_in(&self) -> bool {
		self.address & 0x80 != 0
	}

	pub fn number(&self) -> u8 {
		self.address & 0x0f
	}

	pub fn transfer(&self) -> u8 {
		self.attributes & 0x03
	}

	/// The packet size alone, without the high-bandwidth multiplier bits.
	pub fn max_packet(&self) -> u16 {
		self.packet & 0x07ff
	}

	/// The xHCI device context index: twice the number, and one more for IN.
	pub fn dci(&self) -> u32 {
		self.number() as u32 * 2 + u32::from(self.is_in())
	}

	pub fn is_bulk_in(&self) -> bool {
		self.transfer() == TRANSFER_BULK && self.is_in()
	}

	pub fn is_bulk_out(&self) -> bool {
		self.transfer() == TRANSFER_BULK && !self.is_in()
	}

	pub fn is_interrupt_in(&self) -> bool {
		self.transfer() == TRANSFER_INTERRUPT && self.is_in()
	}
}

/// One interface alternate setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
	pub interface: u8,
	pub alternate: u8,
	pub class: u8,
	pub subclass: u8,
	pub protocol: u8,
	pub endpoints: Vec<Endpoint>,
	functional: (usize, usize),
}

impl Setting {
	pub fn first(&self, test: impl Fn(&Endpoint) -> bool) -> Option<Endpoint> {
		self.endpoints.iter().copied().find(|endpoint| test(endpoint))
	}

	pub fn is(&self, class: u8, subclass: u8) -> bool {
		self.class == class && self.subclass == subclass
	}
}

/// A configuration descriptor's settings, over the bytes they were read from.
pub struct Configuration<'a> {
	bytes: &'a [u8],
	pub value: u8,
	pub settings: Vec<Setting>,
}

impl<'a> Configuration<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<Configuration<'a>, Refused> {
		let mut walk = Walk::new(bytes);
		let head = walk.next().ok_or(Refused::NotConfiguration)?;
		if head.kind != descriptor::DT_CONFIG || head.len() < 9 {
			return Err(Refused::NotConfiguration);
		}
		let value = head.field(5).map_err(|_| Refused::Malformed)?;
		let mut settings: Vec<Setting> = Vec::new();
		let mut offset = head.len();
		for record in walk.by_ref() {
			let start = offset;
			offset += record.len();
			match record.kind {
				descriptor::DT_INTERFACE => {
					if record.len() < 9 {
						return Err(Refused::Malformed);
					}
					if settings.len() >= MAX_SETTINGS {
						return Err(Refused::TooMany);
					}
					let field = |at| record.field(at).map_err(|_| Refused::Malformed);
					settings.push(Setting { interface: field(2)?, alternate: field(3)?, class: field(5)?, subclass: field(6)?, protocol: field(7)?, endpoints: Vec::new(), functional: (offset, offset) });
				}
				descriptor::DT_ENDPOINT => {
					if record.len() < 7 {
						return Err(Refused::Malformed);
					}
					let setting = settings.last_mut().ok_or(Refused::Malformed)?;
					let address = record.field(2).map_err(|_| Refused::Malformed)?;
					if address & 0x0f == 0 || setting.endpoints.iter().any(|endpoint| endpoint.address == address) {
						return Err(Refused::BadEndpoint);
					}
					if setting.endpoints.len() >= MAX_ENDPOINTS {
						return Err(Refused::TooMany);
					}
					let attributes = record.field(3).map_err(|_| Refused::Malformed)?;
					let packet = record.field16(4).map_err(|_| Refused::Malformed)?;
					let interval = record.field(6).map_err(|_| Refused::Malformed)?;
					setting.endpoints.push(Endpoint { address, attributes, packet, interval, extra: (offset, offset) });
				}
				_ => {
					// EVERYTHING ELSE BELONGS TO WHAT IT FOLLOWS: to the last endpoint if the setting has one,
					// otherwise to the setting itself. Records before the first interface - an association,
					// an OTG descriptor - belong to nothing a binder reads, and are skipped.
					let Some(setting) = settings.last_mut() else { continue };
					match setting.endpoints.last_mut() {
						Some(endpoint) => {
							if endpoint.extra.1 == start {
								endpoint.extra.1 = offset;
							}
						}
						None => {
							if setting.functional.1 == start {
								setting.functional.1 = offset;
							}
						}
					}
				}
			}
		}
		if let Some(fault) = walk.fault() {
			return Err(Refused::Walk(fault));
		}
		Ok(Configuration { bytes, value, settings })
	}

	/// The class-specific records between a setting's interface descriptor and its first endpoint.
	pub fn functional(&self, setting: &Setting) -> Walk<'a> {
		Walk::new(&self.bytes[setting.functional.0..setting.functional.1])
	}

	/// The same records as bytes, for a parser that walks them itself.
	pub fn functional_bytes(&self, setting: &Setting) -> &'a [u8] {
		&self.bytes[setting.functional.0..setting.functional.1]
	}

	/// The class-specific records that follow one endpoint descriptor.
	pub fn endpoint_extra(&self, endpoint: &Endpoint) -> Walk<'a> {
		Walk::new(&self.bytes[endpoint.extra.0..endpoint.extra.1])
	}

	/// The first setting of this class and subclass with an endpoint set `usable` accepts - in descriptor
	/// order, which puts an interface's alternate zero first.
	pub fn find(&self, class: u8, subclass: u8, usable: impl Fn(&Setting) -> bool) -> Option<&Setting> {
		self.settings.iter().find(|setting| setting.is(class, subclass) && usable(setting))
	}
}

#[cfg(test)]
mod tests;
