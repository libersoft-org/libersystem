use std::env;
use std::fs;
use std::path::Path;

const FIXED_SERIAL: u32 = 0x4c53_4f47;

fn main() {
	let path = env::args_os().nth(1).expect("usage: normalize-ogg <file>");
	let mut bytes = fs::read(&path).expect("read Ogg file");
	normalize(&mut bytes).expect("valid Ogg pages");
	fs::write(Path::new(&path), bytes).expect("write Ogg file");
}

fn normalize(bytes: &mut [u8]) -> Result<(), &'static str> {
	let mut cursor = 0usize;
	while cursor < bytes.len() {
		let header = bytes.get(cursor..cursor + 27).ok_or("truncated Ogg header")?;
		if &header[..4] != b"OggS" || header[4] != 0 {
			return Err("invalid Ogg capture or version");
		}
		let segments = usize::from(header[26]);
		let table_end = cursor.checked_add(27 + segments).ok_or("Ogg size overflow")?;
		let body_len = bytes.get(cursor + 27..table_end).ok_or("truncated Ogg lacing")?.iter().map(|value| usize::from(*value)).sum::<usize>();
		let page_end = table_end.checked_add(body_len).filter(|end| *end <= bytes.len()).ok_or("truncated Ogg body")?;
		bytes[cursor + 14..cursor + 18].copy_from_slice(&FIXED_SERIAL.to_le_bytes());
		bytes[cursor + 22..cursor + 26].fill(0);
		let checksum = ogg_crc(&bytes[cursor..page_end]);
		bytes[cursor + 22..cursor + 26].copy_from_slice(&checksum.to_le_bytes());
		cursor = page_end;
	}
	Ok(())
}

fn ogg_crc(bytes: &[u8]) -> u32 {
	let mut crc = 0u32;
	for byte in bytes {
		crc ^= u32::from(*byte) << 24;
		for _ in 0..8 {
			crc = if crc & 0x8000_0000 != 0 { crc << 1 ^ 0x04c1_1db7 } else { crc << 1 };
		}
	}
	crc
}

#[cfg(test)]
#[path = "normalize-ogg/tests.rs"]
mod tests;
