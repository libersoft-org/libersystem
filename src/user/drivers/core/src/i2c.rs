// VIRTIO-I2C REQUESTS (virtio 1.2, device 34): how one I2C message, or one SMBus transaction composed from
// them, becomes the requests the device's one queue takes.
//
// A REQUEST is three parts: an out header (the target's seven-bit address shifted left by one, and flags), the
// message's buffer - device-readable for a write, device-writable for a read, absent for a zero-length message
// - and a one-byte in header the device writes its status into. The device reports only success or failure,
// so every failure is `interrupted` to a consumer.
//
// A WRITE THEN A READ IS TWO REQUESTS in one chain of submissions, the first flagged FAIL_NEXT: the device
// performs them as ONE transfer with a repeated start, and fails the second when the first fails.

use crate::smbus::{Composed, Shape};

// The out header's flags.
pub const FLAG_FAIL_NEXT: u32 = 1 << 0;
pub const FLAG_READ: u32 = 1 << 1;

// The one feature: zero-length requests, which a quick command is.
pub const FEATURE_ZERO_LENGTH_REQUEST: u64 = 1 << 0;

// The in header's statuses.
pub const STATUS_OK: u8 = 0;

// The longest message this driver sends: `hid-i2c`'s `MAX_TRANSFER`.
pub const MAX_TRANSFER: usize = 2048;

// ONE REQUEST: its out header and what its buffer is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
	pub header: [u8; 8],
	pub buffer: Buffer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Buffer {
	// A zero-length message: no buffer descriptor at all.
	None,
	// The device reads this many bytes, the message's.
	Write(usize),
	// The device writes this many.
	Read(usize),
}

// The out header for `address` with `flags`.
pub fn header(address: u8, flags: u32) -> [u8; 8] {
	let mut out = [0u8; 8];
	out[0..2].copy_from_slice(&((address as u16) << 1).to_le_bytes());
	out[4..8].copy_from_slice(&flags.to_le_bytes());
	out
}

// THE REQUESTS ONE TRANSFER TAKES: one or two, the first of two flagged FAIL_NEXT.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
	pub requests: [Option<Request>; 2],
}

impl Plan {
	pub fn count(&self) -> usize {
		self.requests.iter().flatten().count()
	}
}

// A PLAIN I2C MESSAGE, or a write then a read: `write` bytes and then `read` bytes, either of which may be
// zero - but not both, which is a quick command and is asked for as one.
pub fn plain(address: u8, write: usize, read: usize) -> Option<Plan> {
	if write > MAX_TRANSFER || read > MAX_TRANSFER || (write == 0 && read == 0) {
		return None;
	}
	Some(match (write, read) {
		(len, 0) => Plan { requests: [Some(Request { header: header(address, 0), buffer: Buffer::Write(len) }), None] },
		(0, len) => Plan { requests: [Some(Request { header: header(address, FLAG_READ), buffer: Buffer::Read(len) }), None] },
		(out, into) => Plan {
			requests: [
				Some(Request { header: header(address, FLAG_FAIL_NEXT), buffer: Buffer::Write(out) }),
				Some(Request { header: header(address, FLAG_READ), buffer: Buffer::Read(into) }),
			],
		},
	})
}

// A COMPOSED SMBUS TRANSACTION as requests. A quick command is one zero-length request, read or write.
pub fn smbus(address: u8, composed: &Composed) -> Plan {
	match composed.shape {
		Shape::Quick { read } => Plan { requests: [Some(Request { header: header(address, if read { FLAG_READ } else { 0 }), buffer: Buffer::None }), None] },
		Shape::Write => Plan { requests: [Some(Request { header: header(address, 0), buffer: Buffer::Write(composed.write().len()) }), None] },
		Shape::Read => Plan { requests: [Some(Request { header: header(address, FLAG_READ), buffer: Buffer::Read(composed.read_len) }), None] },
		Shape::WriteRead => Plan {
			requests: [
				Some(Request { header: header(address, FLAG_FAIL_NEXT), buffer: Buffer::Write(composed.write().len()) }),
				Some(Request { header: header(address, FLAG_READ), buffer: Buffer::Read(composed.read_len) }),
			],
		},
	}
}

#[cfg(test)]
mod tests;
