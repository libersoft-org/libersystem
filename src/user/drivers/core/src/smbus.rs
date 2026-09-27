// SMBUS TRANSACTIONS COMPOSED FROM I2C MESSAGES, and the packet error code that protects them.
//
// A controller that moves I2C messages - write, read, and write-then-read with a repeated start - can carry
// most of SMBus: each SMBus transaction below is a fixed arrangement of those messages. What it cannot carry
// is the BLOCK READ WITH THE DEVICE'S COUNT, where the first byte the device sends decides how many follow: an
// I2C read's length is fixed when it is queued. A controller that declares plain I2C declares these, and not
// that one.
//
// THE PACKET ERROR CODE is SMBus's CRC-8 (x^8 + x^2 + x + 1, initial value zero, no reflection) over EVERY byte
// of the transaction as it appears on the bus - the address byte with its read/write bit included, and again
// after a repeated start. A write carries it as one more byte; a read receives it as one more byte and checks
// it, and a mismatch is its own failure rather than data.

// The longest block SMBus 2.0 carries.
pub const MAX_BLOCK: usize = 32;

// The longest write any transaction below composes: command, count, a whole block and the PEC byte.
pub const MAX_WRITE: usize = 1 + 1 + MAX_BLOCK + 1;

// One SMBus transaction, as a consumer asks for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transaction<'a> {
	// The address alone and the read/write bit: a zero-length message.
	Quick { read: bool },
	SendByte(u8),
	ReceiveByte,
	WriteByteData { command: u8, value: u8 },
	ReadByteData { command: u8 },
	WriteWordData { command: u8, value: u16 },
	ReadWordData { command: u8 },
	BlockWrite { command: u8, data: &'a [u8] },
	// A read of `len` bytes after a one-byte command, with no count from the device.
	I2cBlockRead { command: u8, len: usize },
}

// Why a transaction could not be composed or its answer could not be taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	// Nothing to protect, or a shape this composition does not carry.
	Unsupported,
	// A block longer than SMBus carries, or an empty one.
	TooLong,
	// The PEC the device sent is not the one its bytes make.
	Pec,
	// The controller answered with a different number of bytes than the transaction reads.
	Short,
}

// How the messages go on the bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
	// A zero-length message, read or write.
	Quick { read: bool },
	// One write message.
	Write,
	// One read message.
	Read,
	// A write, then a read after a repeated start, as one transfer.
	WriteRead,
}

// ONE COMPOSED TRANSACTION: its shape, what the write message carries - the PEC byte already appended when
// the transaction asked for one - and how many bytes the read message takes, the device's PEC byte included.
pub struct Composed {
	pub shape: Shape,
	write: [u8; MAX_WRITE],
	write_len: usize,
	pub read_len: usize,
	address: u8,
	pec: bool,
}

impl Composed {
	pub fn write(&self) -> &[u8] {
		&self.write[..self.write_len]
	}

	// THE ANSWER: the bytes the transaction returns once the controller read `read` - the PEC byte checked
	// and taken off when one was asked for. Empty for a transaction that reads nothing.
	pub fn finish<'r>(&self, read: &'r [u8]) -> Result<&'r [u8], Refusal> {
		if read.len() != self.read_len {
			return Err(Refusal::Short);
		}
		if self.read_len == 0 {
			return Ok(read);
		}
		if !self.pec {
			return Ok(read);
		}
		let (data, sent) = read.split_at(read.len() - 1);
		// The bytes as they crossed the bus: the write half with its address byte, if there was one, then the
		// address byte with the read bit, then what the device sent.
		let mut crc = 0u8;
		if self.shape == Shape::WriteRead {
			crc = crc8(crc, &[self.address << 1]);
			crc = crc8(crc, self.write());
		}
		crc = crc8(crc, &[self.address << 1 | 1]);
		crc = crc8(crc, data);
		if crc != sent[0] {
			return Err(Refusal::Pec);
		}
		Ok(data)
	}
}

// THE CRC-8 SMBUS USES: polynomial 0x07, most significant bit first, continuing from `crc`.
pub fn crc8(mut crc: u8, bytes: &[u8]) -> u8 {
	for &byte in bytes {
		crc ^= byte;
		for _ in 0..8 {
			crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
		}
	}
	crc
}

// COMPOSE `transaction` for the seven-bit `address`, with a PEC when `pec` asks for one.
pub fn compose(address: u8, transaction: Transaction<'_>, pec: bool) -> Result<Composed, Refusal> {
	let mut out = Composed { shape: Shape::Write, write: [0; MAX_WRITE], write_len: 0, read_len: 0, address, pec };
	let put = |bytes: &[u8], out: &mut Composed| {
		out.write[out.write_len..out.write_len + bytes.len()].copy_from_slice(bytes);
		out.write_len += bytes.len();
	};
	match transaction {
		Transaction::Quick { read } => {
			// A QUICK COMMAND CARRIES NO BYTE, so there is nothing for a PEC to protect.
			if pec {
				return Err(Refusal::Unsupported);
			}
			out.shape = Shape::Quick { read };
			return Ok(out);
		}
		Transaction::SendByte(value) => put(&[value], &mut out),
		Transaction::ReceiveByte => {
			out.shape = Shape::Read;
			out.read_len = 1;
		}
		Transaction::WriteByteData { command, value } => put(&[command, value], &mut out),
		Transaction::ReadByteData { command } => {
			put(&[command], &mut out);
			out.shape = Shape::WriteRead;
			out.read_len = 1;
		}
		Transaction::WriteWordData { command, value } => put(&[command, value as u8, (value >> 8) as u8], &mut out),
		Transaction::ReadWordData { command } => {
			put(&[command], &mut out);
			out.shape = Shape::WriteRead;
			out.read_len = 2;
		}
		Transaction::BlockWrite { command, data } => {
			if data.is_empty() || data.len() > MAX_BLOCK {
				return Err(Refusal::TooLong);
			}
			put(&[command, data.len() as u8], &mut out);
			put(data, &mut out);
		}
		Transaction::I2cBlockRead { command, len } => {
			if len == 0 || len > MAX_BLOCK {
				return Err(Refusal::TooLong);
			}
			put(&[command], &mut out);
			out.shape = Shape::WriteRead;
			out.read_len = len;
		}
	}
	if pec {
		if out.shape == Shape::Write {
			// A WRITE carries its PEC as the last byte, over the address byte and everything before it.
			let crc = crc8(crc8(0, &[address << 1]), out.write());
			put(&[crc], &mut out);
		} else {
			// A READ receives one more byte, which `finish` checks.
			out.read_len += 1;
		}
	}
	Ok(out)
}

#[cfg(test)]
mod tests;
