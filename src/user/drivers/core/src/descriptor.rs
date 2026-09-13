// The USB descriptor walk, as pure decisions: what a configuration transfer has to contain before a
// walker reads a byte of it, tested on the host through the crate's seam.
//
// THE DEBT THAT LIVES HERE (DRV-012). A configuration descriptor is a variable-length list of
// variable-length records, and the walker trusted three things about it that nothing had checked:
//
//   THAT THE TRANSFER ARRIVED. The driver asks for `wTotalLength` bytes and the device may return
//     fewer; the page is reused between transfers, so the bytes past a short answer are the PREVIOUS
//     descriptor's, and the walk reads them as records.
//   THAT A RECORD IS AS LONG AS THE FIELD BEING READ. The interface walk reads the class at offset
//     five, the subclass at six and the protocol at seven of a record whose own `bLength` may be two.
//     Those three bytes are then whatever follows the record - the next descriptor's header, or the
//     tail of the previous transfer.
//   AND THAT THE ANSWER IS THE DESCRIPTOR THAT WAS ASKED FOR. A device that answers a configuration
//     request with a string descriptor is answered by a walk over a string.
//
// Reading past a record that is too short is a DIFFERENT fault from a record whose declared length
// runs past the transfer, and only the second was ever noticed - so both are named here.

// Why a descriptor transfer or one of its records cannot be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorFault {
	// The device returned fewer bytes than the descriptor it claims to carry.
	ShortTransfer { declared: u16, received: u32 },
	// The descriptor is not the type that was asked for.
	Type { expected: u8, got: u8 },
	// A record whose `bLength` is below the two bytes every record has.
	Malformed,
	// A record whose declared length runs past the end of what was received.
	Overrun,
	// A field read from a record that is too short to hold it.
	ShortRecord { length: u8, needed: usize },
}

// The bytes of a descriptor transfer that may be walked, from what was asked for and what arrived.
//
// THE DECLARED TOTAL IS THE DEVICE'S OWN CLAIM and the received count is the transport's observation.
// A device that claims more than it sent is refused rather than having its claim believed over the
// bytes that are actually there.
pub fn check_transfer(declared: u16, received: u32) -> Result<u16, DescriptorFault> {
	if (declared as u32) > received {
		return Err(DescriptorFault::ShortTransfer { declared, received });
	}
	Ok(declared)
}

// The descriptor is the one that was asked for.
pub fn check_type(expected: u8, got: u8) -> Result<(), DescriptorFault> {
	if expected == got { Ok(()) } else { Err(DescriptorFault::Type { expected, got }) }
}

// One record of the walk: its own bytes and nothing past them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record<'a> {
	pub kind: u8,
	bytes: &'a [u8],
}

impl<'a> Record<'a> {
	pub fn len(&self) -> usize {
		self.bytes.len()
	}

	pub fn is_empty(&self) -> bool {
		self.bytes.is_empty()
	}

	// A fixed field of this record, or a refusal naming what was missing.
	//
	// INSIDE THE RECORD AND NOT INSIDE THE BUFFER. A record whose `bLength` is two is a legal record,
	// and the class byte the interface walk wants is not in it - so the answer is a refusal rather
	// than the byte that happens to follow.
	pub fn field(&self, offset: usize) -> Result<u8, DescriptorFault> {
		self.bytes.get(offset).copied().ok_or(DescriptorFault::ShortRecord { length: self.bytes.len() as u8, needed: offset + 1 })
	}

	// Two fixed bytes as a little-endian value, for the fields that are one.
	pub fn field16(&self, offset: usize) -> Result<u16, DescriptorFault> {
		Ok(self.field(offset)? as u16 | (self.field(offset + 1)? as u16) << 8)
	}
}

// Walk the records of a descriptor transfer.
//
// IT STOPS ON THE FIRST MALFORMED RECORD rather than skipping it: the length field is what says where
// the next record begins, so a record that cannot be believed takes the rest of the walk with it.
pub struct Walk<'a> {
	bytes: &'a [u8],
	offset: usize,
	fault: Option<DescriptorFault>,
}

impl<'a> Walk<'a> {
	pub fn new(bytes: &'a [u8]) -> Self {
		Self { bytes, offset: 0, fault: None }
	}

	// The refusal that ended the walk, when one did.
	pub fn fault(&self) -> Option<DescriptorFault> {
		self.fault
	}
}

impl<'a> Iterator for Walk<'a> {
	type Item = Record<'a>;

	fn next(&mut self) -> Option<Record<'a>> {
		if self.offset + 2 > self.bytes.len() {
			return None;
		}
		let length = self.bytes[self.offset] as usize;
		let kind = self.bytes[self.offset + 1];
		if length < 2 {
			self.fault = Some(DescriptorFault::Malformed);
			return None;
		}
		let end = self.offset.checked_add(length)?;
		if end > self.bytes.len() {
			self.fault = Some(DescriptorFault::Overrun);
			return None;
		}
		let record = Record { kind, bytes: &self.bytes[self.offset..end] };
		self.offset = end;
		Some(record)
	}
}

#[cfg(test)]
mod tests;
