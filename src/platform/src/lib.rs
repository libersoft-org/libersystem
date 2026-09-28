// WHERE A FIRMWARE DESCRIPTION OF A PLATFORM DEVICE GOES in the device table, decided by pure functions.
//
// A device the firmware describes can be described more than once: the `TPM2` table and the namespace's
// `MSFT0101` node are one TPM, SPCR and the console UART's node are one UART, the `HPET` table and
// `PNP0103` are one timer. Rows are published in a fixed order - what the kernel declares, then the static
// tables, then the device tree or the namespace - and every later description is PLACED against the rows
// already there:
//
//   - one carrying the same IDENTITY is that row, never a new one (a walk reporting it again);
//   - one whose ranges overlap a row's, where every overlap STARTS AT THE SAME BASE and one range contains
//     the other, is MERGED into it: its identity and match ids join the row's, the row keeps the first
//     description's resources, and a live claim is never changed;
//   - any other overlap is REFUSED, and says which row it overlapped and how;
//   - anything else is a row of its own.
//
// And a range over something no platform row may own - RAM, a PCI function's BAR, a range the kernel
// holds - is refused before it is placed at all (`over`).

#![no_std]

use abi::{Connection, MatchId, MmioResource, PlatformPart, PortResource, WiredLine};

pub mod policy;
pub mod report;

#[cfg(test)]
mod firmware_tests;
#[cfg(test)]
mod tests;

// A DESCRIPTION, before it has a row: the platform part it would publish and the port ranges beside it
// (a row's ports live in `DeviceInfo::ports`, which PCI rows use too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Description {
	pub part: PlatformPart,
	pub ports: [PortResource; abi::MAX_PORT_RESOURCES],
	pub port_count: u8,
}

impl Description {
	// A description from `source` in `state`, named `identity`; None for an identity past the bound.
	pub fn new(source: u8, state: u8, identity: &[u8]) -> Option<Self> {
		if identity.is_empty() || identity.len() > abi::PLATFORM_NAME_LEN || identity.contains(&0) {
			return None;
		}
		let mut part = PlatformPart { kind: abi::ROW_KIND_PLATFORM, source, state, ..PlatformPart::default() };
		part.identity[..identity.len()].copy_from_slice(identity);
		Some(Description { part, ports: [PortResource::default(); abi::MAX_PORT_RESOURCES], port_count: 0 })
	}

	pub fn identity(&self) -> &[u8] {
		self.part.identity()
	}

	// Each of these answers false when the description is full, which the caller reports: a description
	// past a bound is refused whole rather than published with part of what it owns.
	pub fn add_match(&mut self, kind: u8, text: &[u8]) -> bool {
		let at = self.part.match_count as usize;
		let Some(id) = MatchId::new(kind, text) else { return false };
		if at >= abi::MAX_MATCH_IDS {
			return false;
		}
		self.part.match_ids[at] = id;
		self.part.match_count += 1;
		true
	}

	pub fn add_mmio(&mut self, base: u64, len: u64) -> bool {
		let at = self.part.mmio_count as usize;
		if at >= abi::MAX_PLATFORM_MMIO || len == 0 || base.checked_add(len).is_none() {
			return false;
		}
		self.part.mmio[at] = MmioResource { base, len };
		self.part.mmio_count += 1;
		true
	}

	pub fn add_port(&mut self, base: u16, len: u16) -> bool {
		let at = self.port_count as usize;
		if at >= abi::MAX_PORT_RESOURCES || len == 0 || u32::from(base) + u32::from(len) > 0x1_0000 {
			return false;
		}
		self.ports[at] = PortResource { base, len, source: abi::PORT_SOURCE_PLATFORM, index: at as u8, _pad: [0; 2] };
		self.port_count += 1;
		true
	}

	pub fn add_line(&mut self, line: WiredLine) -> bool {
		let at = self.part.line_count as usize;
		if at >= abi::MAX_PLATFORM_LINES {
			return false;
		}
		self.part.lines[at] = line;
		self.part.line_count += 1;
		true
	}

	pub fn add_connection(&mut self, connection: Connection) -> bool {
		let at = self.part.connection_count as usize;
		if at >= abi::MAX_PLATFORM_CONNECTIONS {
			return false;
		}
		self.part.connections[at] = connection;
		self.part.connection_count += 1;
		true
	}

	pub fn ports(&self) -> &[PortResource] {
		&self.ports[..(self.port_count as usize).min(abi::MAX_PORT_RESOURCES)]
	}
}

// Which kind of range an overlap was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlap {
	Mmio,
	Ports,
}

// Where a description goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
	New,
	Same(usize),
	Merge(usize),
	Refuse { row: usize, what: Overlap },
}

// A ROW AS PLACEMENT READS IT: its platform part and its port ranges. A PCI row is not a platform row and
// is passed as None, so indices stay the device table's.
pub type RowView<'a> = Option<(&'a PlatformPart, &'a [PortResource])>;

fn overlaps(a_base: u64, a_len: u64, b_base: u64, b_len: u64) -> bool {
	a_base < b_base.saturating_add(b_len) && b_base < a_base.saturating_add(a_len)
}

// A pair of overlapping ranges MERGES only when both start at the same base and one contains the other -
// which two ranges starting at one base always do, so the base is the whole test: the shorter is the
// longer's head. Two ranges at different bases that overlap are two descriptions disagreeing about where
// a device is, and neither may be believed over the other.
fn mergeable(a_base: u64, b_base: u64) -> bool {
	a_base == b_base
}

fn carries_identity(part: &PlatformPart, identity: &[u8]) -> bool {
	part.identity() == identity || part.match_ids().iter().any(|id| id.kind == abi::MATCH_ID_IDENTITY && id.text() == identity)
}

// Where `description` goes among `rows`.
pub fn place(rows: &[RowView<'_>], description: &Description) -> Placement {
	let identity = description.identity();
	if let Some(row) = rows.iter().position(|row| row.is_some_and(|(part, _)| carries_identity(part, identity))) {
		return Placement::Same(row);
	}
	let mut merge_into: Option<usize> = None;
	for (row, view) in rows.iter().enumerate() {
		let Some((part, ports)) = view else { continue };
		for mine in description.part.mmio() {
			for theirs in part.mmio() {
				if !overlaps(mine.base, mine.len, theirs.base, theirs.len) {
					continue;
				}
				if !mergeable(mine.base, theirs.base) || merge_into.is_some_and(|other| other != row) {
					return Placement::Refuse { row, what: Overlap::Mmio };
				}
				merge_into = Some(row);
			}
		}
		for mine in description.ports() {
			for theirs in ports.iter() {
				if !overlaps(u64::from(mine.base), u64::from(mine.len), u64::from(theirs.base), u64::from(theirs.len)) {
					continue;
				}
				if !mergeable(u64::from(mine.base), u64::from(theirs.base)) || merge_into.is_some_and(|other| other != row) {
					return Placement::Refuse { row, what: Overlap::Ports };
				}
				merge_into = Some(row);
			}
		}
	}
	merge_into.map_or(Placement::New, Placement::Merge)
}

// MERGE `description` into `into`: its identity joins as an `IDENTITY` match id and each of its match ids
// the row does not already carry joins too; the row's resources and state stay the first description's.
// False when they do not all fit, and then nothing was added.
pub fn merge(into: &mut PlatformPart, description: &Description) -> bool {
	let mut added = [MatchId::default(); abi::MAX_MATCH_IDS + 1];
	let mut count = 0usize;
	let Some(identity) = MatchId::new(abi::MATCH_ID_IDENTITY, description.identity()) else { return false };
	let mut wanted = [identity; abi::MAX_MATCH_IDS + 1];
	wanted[1..=description.part.match_ids().len()].copy_from_slice(description.part.match_ids());
	for id in &wanted[..=description.part.match_ids().len()] {
		if into.match_ids().iter().any(|held| held.kind == id.kind && held.text() == id.text()) || added[..count].iter().any(|held| held.kind == id.kind && held.text() == id.text()) {
			continue;
		}
		added[count] = *id;
		count += 1;
	}
	let at = into.match_count as usize;
	if at + count > abi::MAX_MATCH_IDS {
		return false;
	}
	into.match_ids[at..at + count].copy_from_slice(&added[..count]);
	into.match_count += count as u8;
	true
}

// WHETHER A RANGE LIES OVER SOMETHING NO PLATFORM ROW MAY OWN: any of `forbidden`, each (base, len) - the
// RAM the memory map reports, every PCI function's BAR, and every range a kernel-held row owns.
pub fn over(base: u64, len: u64, forbidden: &[(u64, u64)]) -> Option<usize> {
	forbidden.iter().position(|&(start, size)| overlaps(base, len, start, size))
}

// AN IDENTITY FROM ITS FORM AND BODY - `dt:` and a node path, `table:` a signature and instance, `kernel:`
// a name - into `out`; None past the bound.
pub fn identity(form: &[u8], body: &[u8], out: &mut [u8; abi::PLATFORM_NAME_LEN]) -> Option<usize> {
	let len = form.len().checked_add(body.len())?;
	if len > abi::PLATFORM_NAME_LEN || body.contains(&0) {
		return None;
	}
	out[..form.len()].copy_from_slice(form);
	out[form.len()..len].copy_from_slice(body);
	Some(len)
}

// `table:SIG#n`, the identity of a static table's `n`-th instance.
pub fn table_identity(signature: &[u8; 4], instance: u32, out: &mut [u8; abi::PLATFORM_NAME_LEN]) -> usize {
	let mut body = [0u8; 16];
	body[..4].copy_from_slice(signature);
	body[4] = b'#';
	let mut digits = [0u8; 10];
	let mut n = instance;
	let mut count = 0;
	loop {
		digits[count] = b'0' + (n % 10) as u8;
		count += 1;
		n /= 10;
		if n == 0 {
			break;
		}
	}
	for i in 0..count {
		body[5 + i] = digits[count - 1 - i];
	}
	identity(b"table:", &body[..5 + count], out).unwrap_or(0)
}
