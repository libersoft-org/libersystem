use super::*;

// The seven-bit address the tests compose for: 0x50, 0xA0 on the bus for a write and 0xA1 for a read.
const ADDRESS: u8 = 0x50;

#[test]
fn the_pec_is_smbus_s_crc_8() {
	// THE POLYNOMIAL THE SPECIFICATION NAMES, x^8 + x^2 + x + 1, from zero and unreflected - which the CRC
	// catalogue lists as CRC-8/SMBUS with the check value 0xF4 over "123456789".
	assert_eq!(crc8(0, b"123456789"), 0xF4);
	assert_eq!(crc8(0, &[]), 0);
	// Continuing a CRC is the same as computing it over the concatenation.
	assert_eq!(crc8(crc8(0, b"1234"), b"56789"), 0xF4);
	// And a message followed by its own CRC checks to zero, which is how a receiver can verify a PEC.
	let crc = crc8(0, &[0xA0, 0x10, 0x55]);
	assert_eq!(crc8(0, &[0xA0, 0x10, 0x55, crc]), 0);
}

#[test]
fn every_write_carries_its_pec_over_the_address_byte_and_what_it_writes() {
	for (transaction, bytes) in [
		(Transaction::SendByte(0x42), &[0x42][..]),
		(Transaction::WriteByteData { command: 0x10, value: 0x55 }, &[0x10, 0x55]),
		(Transaction::WriteWordData { command: 0x20, value: 0xBEEF }, &[0x20, 0xEF, 0xBE]),
	] {
		let plain = compose(ADDRESS, transaction, false).expect("composes");
		assert_eq!((plain.shape, plain.write(), plain.read_len), (Shape::Write, bytes, 0), "{transaction:?}");
		let protected = compose(ADDRESS, transaction, true).expect("composes with a PEC");
		let (data, pec) = protected.write().split_at(bytes.len());
		assert_eq!(data, bytes);
		assert_eq!(pec, &[crc8(crc8(0, &[ADDRESS << 1]), bytes)], "{transaction:?}: the PEC byte is the CRC of the address byte and the data");
		assert_eq!(protected.read_len, 0);
		assert_eq!(protected.finish(&[]), Ok(&[][..]), "a write answers nothing");
	}
	// A WORD GOES LOW BYTE FIRST, as SMBus sends it.
	assert_eq!(compose(ADDRESS, Transaction::WriteWordData { command: 1, value: 0x1234 }, false).expect("composes").write(), &[1, 0x34, 0x12]);
}

#[test]
fn a_block_write_carries_its_count_and_is_bounded_by_smbus() {
	let block = [7u8; MAX_BLOCK];
	let composed = compose(ADDRESS, Transaction::BlockWrite { command: 0x30, data: &block[..3] }, false).expect("composes");
	assert_eq!(composed.write(), &[0x30, 3, 7, 7, 7]);
	let whole = compose(ADDRESS, Transaction::BlockWrite { command: 0x30, data: &block }, true).expect("a whole block composes, with its PEC");
	assert_eq!(whole.write().len(), MAX_WRITE);
	assert_eq!(compose(ADDRESS, Transaction::BlockWrite { command: 0x30, data: &[0u8; MAX_BLOCK + 1] }, false).err(), Some(Refusal::TooLong));
	assert_eq!(compose(ADDRESS, Transaction::BlockWrite { command: 0x30, data: &[] }, false).err(), Some(Refusal::TooLong), "an empty block is not a block");
}

#[test]
fn a_read_after_its_command_is_one_transfer_with_a_repeated_start() {
	let byte = compose(ADDRESS, Transaction::ReadByteData { command: 0x10 }, false).expect("composes");
	assert_eq!((byte.shape, byte.write(), byte.read_len), (Shape::WriteRead, &[0x10][..], 1));
	assert_eq!(byte.finish(&[0x99]), Ok(&[0x99][..]));
	let word = compose(ADDRESS, Transaction::ReadWordData { command: 0x11 }, false).expect("composes");
	assert_eq!((word.shape, word.read_len), (Shape::WriteRead, 2));
	let block = compose(ADDRESS, Transaction::I2cBlockRead { command: 0x12, len: 5 }, false).expect("composes");
	assert_eq!((block.shape, block.write(), block.read_len), (Shape::WriteRead, &[0x12][..], 5));
	assert_eq!(compose(ADDRESS, Transaction::I2cBlockRead { command: 0x12, len: 0 }, false).err(), Some(Refusal::TooLong));
	assert_eq!(compose(ADDRESS, Transaction::I2cBlockRead { command: 0x12, len: MAX_BLOCK + 1 }, false).err(), Some(Refusal::TooLong));
	// AN ANSWER OF THE WRONG LENGTH is not taken.
	assert_eq!(byte.finish(&[1, 2]), Err(Refusal::Short));
}

#[test]
fn a_read_s_pec_covers_both_address_bytes_and_a_wrong_one_is_refused() {
	let composed = compose(ADDRESS, Transaction::ReadWordData { command: 0x21 }, true).expect("composes with a PEC");
	assert_eq!(composed.read_len, 3, "two data bytes and the device's PEC");
	// What crossed the bus: 0xA0, the command, 0xA1 after the repeated start, then the device's two bytes.
	let pec = crc8(0, &[ADDRESS << 1, 0x21, ADDRESS << 1 | 1, 0x34, 0x12]);
	assert_eq!(composed.finish(&[0x34, 0x12, pec]), Ok(&[0x34, 0x12][..]));
	assert_eq!(composed.finish(&[0x34, 0x12, pec ^ 1]), Err(Refusal::Pec), "a checksum that does not match is refused, not returned as data");
	// A RECEIVE BYTE has no write half: its PEC covers the read address byte and the data alone.
	let receive = compose(ADDRESS, Transaction::ReceiveByte, true).expect("composes with a PEC");
	assert_eq!((receive.shape, receive.read_len), (Shape::Read, 2));
	let pec = crc8(0, &[ADDRESS << 1 | 1, 0x5A]);
	assert_eq!(receive.finish(&[0x5A, pec]), Ok(&[0x5A][..]));
	assert_eq!(receive.finish(&[0x5A, 0]), Err(Refusal::Pec));
}

#[test]
fn a_quick_command_is_the_address_alone_and_carries_no_pec() {
	let write = compose(ADDRESS, Transaction::Quick { read: false }, false).expect("composes");
	assert_eq!((write.shape, write.write(), write.read_len), (Shape::Quick { read: false }, &[][..], 0));
	assert_eq!(compose(ADDRESS, Transaction::Quick { read: true }, false).expect("composes").shape, Shape::Quick { read: true });
	assert_eq!(compose(ADDRESS, Transaction::Quick { read: false }, true).err(), Some(Refusal::Unsupported), "there is no byte for a PEC to protect");
}
