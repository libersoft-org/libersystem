//! A face this tool BUILDS, so the oracle can be proved to refuse before it is trusted to approve.
//!
//! A VALIDATOR EXERCISED ONLY AGAINST A VALID INPUT IS NOT EXERCISED, and this one has no input at
//! all until a licensed face is staged. Without a face it could pass over an empty set for months
//! while its comparison quietly stopped comparing. So it builds one, agrees with it, and is then
//! required to DISAGREE with the same face under each field made wrong in turn.

pub fn u16(out: &mut Vec<u8>, value: u16) {
	out.extend_from_slice(&value.to_be_bytes());
}

pub fn u32(out: &mut Vec<u8>, value: u32) {
	out.extend_from_slice(&value.to_be_bytes());
}

fn head() -> Vec<u8> {
	let mut out = Vec::new();
	u16(&mut out, 1);
	u16(&mut out, 0);
	u32(&mut out, 0);
	u32(&mut out, 0);
	u32(&mut out, 0x5F0F_3CF5);
	u16(&mut out, 0);
	u16(&mut out, 1000);
	out.extend_from_slice(&[0u8; 16]);
	u16(&mut out, 0);
	u16(&mut out, 0);
	u16(&mut out, 1000);
	u16(&mut out, 1000);
	u16(&mut out, 0);
	u16(&mut out, 8);
	u16(&mut out, 2);
	u16(&mut out, 0);
	u16(&mut out, 0);
	out
}

fn hhea() -> Vec<u8> {
	let mut out = Vec::new();
	u16(&mut out, 1);
	u16(&mut out, 0);
	u16(&mut out, 800);
	u16(&mut out, (-200i16) as u16);
	u16(&mut out, 0);
	out.extend_from_slice(&[0u8; 24]);
	u16(&mut out, 2);
	out
}

fn maxp() -> Vec<u8> {
	let mut out = Vec::new();
	u32(&mut out, 0x0001_0000);
	u16(&mut out, 2);
	out.extend_from_slice(&[0u8; 26]);
	out
}

fn hmtx() -> Vec<u8> {
	let mut out = Vec::new();
	for _ in 0..2 {
		u16(&mut out, 500);
		u16(&mut out, 0);
	}
	out
}

/// A `name` table with the four names the oracle reads, in Windows English.
fn name(family: &str, style: &str) -> Vec<u8> {
	let records: [(u16, &str); 4] = [(1, family), (2, style), (16, family), (17, style)];
	let mut storage = Vec::new();
	let mut entries = Vec::new();
	for (id, text) in records {
		let mut bytes = Vec::new();
		for unit in text.encode_utf16() {
			bytes.extend_from_slice(&unit.to_be_bytes());
		}
		entries.push((id, storage.len(), bytes.len()));
		storage.extend_from_slice(&bytes);
	}
	let mut out = Vec::new();
	u16(&mut out, 0);
	u16(&mut out, entries.len() as u16);
	u16(&mut out, (6 + entries.len() * 12) as u16);
	for (id, offset, length) in &entries {
		u16(&mut out, 3); // Windows
		u16(&mut out, 1);
		u16(&mut out, 0x0409); // English
		u16(&mut out, *id);
		u16(&mut out, *length as u16);
		u16(&mut out, *offset as u16);
	}
	out.extend_from_slice(&storage);
	out
}

/// An `OS/2` version 4 table stating the weight, the width and the slope.
fn os2(weight: u16, width: u16, selection: u16) -> Vec<u8> {
	let mut out = Vec::new();
	u16(&mut out, 4);
	u16(&mut out, 500);
	u16(&mut out, weight);
	u16(&mut out, width);
	u16(&mut out, 0);
	out.extend_from_slice(&[0u8; 16]);
	out.extend_from_slice(&[0u8; 6]);
	out.extend_from_slice(&[0u8; 10]);
	out.extend_from_slice(&[0u8; 16]);
	out.extend_from_slice(b"TEST");
	u16(&mut out, selection);
	u16(&mut out, 32);
	u16(&mut out, 0xFFFF);
	u16(&mut out, 800);
	u16(&mut out, (-200i16) as u16);
	u16(&mut out, 100);
	u16(&mut out, 900);
	u16(&mut out, 250);
	out.extend_from_slice(&[0u8; 8]);
	u16(&mut out, 500);
	u16(&mut out, 700);
	u16(&mut out, 0);
	u16(&mut out, 32);
	u16(&mut out, 0);
	out
}

/// An `fvar` with the axes given, as `(tag, minimum, default, maximum)` in whole design units.
fn fvar(axes: &[([u8; 4], i32, i32, i32)]) -> Vec<u8> {
	let mut out = Vec::new();
	u16(&mut out, 1);
	u16(&mut out, 0);
	u16(&mut out, 16);
	u16(&mut out, 2);
	u16(&mut out, axes.len() as u16);
	u16(&mut out, 20);
	u16(&mut out, 0);
	u16(&mut out, 4 + axes.len() as u16 * 4);
	for (tag, minimum, default, maximum) in axes {
		out.extend_from_slice(tag);
		u32(&mut out, (*minimum << 16) as u32);
		u32(&mut out, (*default << 16) as u32);
		u32(&mut out, (*maximum << 16) as u32);
		u16(&mut out, 0);
		u16(&mut out, 256);
	}
	out
}

/// A minimal `glyf` and `loca`: one empty glyph and one square.
fn outlines() -> (Vec<u8>, Vec<u8>) {
	let mut glyf = Vec::new();
	u16(&mut glyf, 1);
	u16(&mut glyf, 0);
	u16(&mut glyf, 0);
	u16(&mut glyf, 1000);
	u16(&mut glyf, 1000);
	u16(&mut glyf, 3);
	u16(&mut glyf, 0);
	// Four on-curve points, each a short positive delta on both axes.
	for _ in 0..4 {
		glyf.push(0x01 | 0x02 | 0x10 | 0x04 | 0x20);
	}
	glyf.extend_from_slice(&[100u8, 100, 100, 100]);
	glyf.extend_from_slice(&[100u8, 100, 100, 100]);
	while glyf.len() % 2 != 0 {
		glyf.push(0);
	}
	let mut loca = Vec::new();
	u16(&mut loca, 0);
	u16(&mut loca, 0);
	u16(&mut loca, (glyf.len() / 2) as u16);
	(glyf, loca)
}

/// Assemble a font out of its tables, which must be in tag order.
fn font(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
	let mut out = Vec::new();
	u32(&mut out, 0x0001_0000);
	u16(&mut out, tables.len() as u16);
	u16(&mut out, 0);
	u16(&mut out, 0);
	u16(&mut out, 0);
	let mut offset = 12 + tables.len() * 16;
	let mut body = Vec::new();
	for (tag, bytes) in tables {
		out.extend_from_slice(tag);
		u32(&mut out, 0);
		u32(&mut out, offset as u32);
		u32(&mut out, bytes.len() as u32);
		body.extend_from_slice(bytes);
		while body.len() % 4 != 0 {
			body.push(0);
		}
		offset = 12 + tables.len() * 16 + body.len();
	}
	out.extend_from_slice(&body);
	out
}

/// A face stating the family, style, weight, width, slope and axes given.
pub fn face(family: &str, style: &str, weight: u16, width: u16, selection: u16, axes: &[([u8; 4], i32, i32, i32)]) -> Vec<u8> {
	let (glyf, loca) = outlines();
	let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
		(*b"OS/2", os2(weight, width, selection)),
		(*b"glyf", glyf),
		(*b"head", head()),
		(*b"hhea", hhea()),
		(*b"hmtx", hmtx()),
		(*b"loca", loca),
		(*b"maxp", maxp()),
		(*b"name", name(family, style)),
	];
	if !axes.is_empty() {
		tables.push((*b"fvar", fvar(axes)));
	}
	tables.sort_by_key(|(tag, _)| *tag);
	font(&tables)
}
