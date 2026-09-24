//! WHAT A BOND IS ON DISK, and what makes one unreadable rather than absent.
//!
//! A bond is a long-term key and the little that goes with it: which controller it belongs to, which
//! peer, what the link it came from was worth, and whether an operator has made that peer an input
//! source. It outlives the service, the boot and the machine being switched off, which is the whole
//! reason it is written down - a device that had to be paired again after every reboot would be a
//! device nobody pairs once.
//!
//! A CORRUPT RECORD IS UNAVAILABLE AND NEVER AN IMPLICIT NEW PEER. This is the rule the module is
//! shaped by. A store whose bytes have been damaged - a truncated write, a half-written file, a
//! version this build does not know - must answer "I do not have that bond", not "here is one" and
//! not "there is no such peer, pair it again without asking". The first is a device that stops
//! working until somebody looks; the second is a key applied to the wrong link; the third is a
//! pairing an operator did not authorise, prompted by a filesystem error.
//!
//! THE CHECKSUM IS A CORRUPTION CHECK AND NOT A MESSAGE AUTHENTICATION CODE, and saying so here is
//! the point. It catches a truncated write, a torn sector and a byte flipped in flight; it catches
//! nothing at all about somebody who can write this file on purpose, and the milestone states that
//! limit in its own words - there is no claim of protection from a trusted storage administrator.
//! A keyed construction would imply one, which is worse than having neither.

/// The record format this build writes and reads. A file whose records name another version is
/// unavailable rather than read: a record read under the wrong layout is a key applied to the wrong
/// link, which is the one outcome worse than having no key.
pub const VERSION: u32 = 1;

/// How many bonds this store holds.
pub const MAX_RECORDS: usize = 64;

/// The longest peer name kept, which is the same bound the wire carries.
pub const MAX_NAME: usize = 48;

/// One record's bytes on disk.
pub const RECORD_BYTES: usize = 89;

/// The most the whole file may be. A file larger than this is not this store's, whatever it
/// contains, and is refused rather than parsed for whatever fits.
pub const MAX_FILE_BYTES: usize = 64 * 1024;

/// A device address as a record holds it: the type byte and six bytes, most significant first.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Address {
	pub kind: u8,
	pub bytes: [u8; 6],
}

/// One bond.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
	pub local: Address,
	pub peer: Address,
	pub key: [u8; 16],
	pub security: u8,
	pub enabled: bool,
	pub name: [u8; MAX_NAME],
	pub name_len: u8,
}

impl Default for Record {
	fn default() -> Self {
		Record { local: Address::default(), peer: Address::default(), key: [0; 16], security: 0, enabled: false, name: [0; MAX_NAME], name_len: 0 }
	}
}

impl Record {
	/// The name, without the padding behind it.
	pub fn name(&self) -> &[u8] {
		&self.name[..(self.name_len as usize).min(MAX_NAME)]
	}
}

/// Why a record or a file could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
	/// The bytes are not a whole number of records.
	Ragged { len: usize },
	/// The file is larger than this store will hold.
	TooLarge { len: usize },
	/// More records than this store holds.
	TooMany { records: usize },
	/// A record naming a version this build does not know.
	Version { found: u32 },
	/// A record whose checksum does not match its bytes.
	Corrupt { at: usize },
	/// A name length past the field that holds it.
	Name { len: u8 },
	/// An address kind this store does not know. One and two are public and random-static, which
	/// are the two this profile speaks; anything else is a record written by something else.
	AddressKind { kind: u8 },
}

/// A corruption check over a record's own bytes. FNV-1a, which is a hash and not a MAC - see the
/// module's note.
fn checksum(bytes: &[u8]) -> u32 {
	let mut hash: u32 = 0x811c_9dc5;
	for byte in bytes {
		hash ^= *byte as u32;
		hash = hash.wrapping_mul(0x0100_0193);
	}
	hash
}

/// The two address kinds this profile speaks.
const KIND_PUBLIC: u8 = 1;
const KIND_RANDOM_STATIC: u8 = 2;

fn known_kind(kind: u8) -> bool {
	kind == KIND_PUBLIC || kind == KIND_RANDOM_STATIC
}

/// Write one record's bytes.
pub fn encode(record: &Record) -> [u8; RECORD_BYTES] {
	let mut out = [0u8; RECORD_BYTES];
	out[..4].copy_from_slice(&VERSION.to_le_bytes());
	out[4] = record.local.kind;
	out[5..11].copy_from_slice(&record.local.bytes);
	out[11] = record.peer.kind;
	out[12..18].copy_from_slice(&record.peer.bytes);
	out[18..34].copy_from_slice(&record.key);
	out[34] = record.security;
	out[35] = u8::from(record.enabled);
	out[36] = record.name_len;
	out[37..85].copy_from_slice(&record.name);
	let sum = checksum(&out[..85]);
	out[85..89].copy_from_slice(&sum.to_le_bytes());
	out
}

/// Read one record's bytes.
///
/// THE CHECKSUM IS CHECKED BEFORE ANY FIELD IS BELIEVED. A record whose bytes are damaged has a
/// damaged name length too, and validating the fields of bytes that are already known to be wrong
/// reports whichever field happened to be noticed rather than the fact that the record is corrupt.
pub fn decode(bytes: &[u8], at: usize) -> Result<Record, Fault> {
	if bytes.len() != RECORD_BYTES {
		return Err(Fault::Ragged { len: bytes.len() });
	}
	let stored = u32::from_le_bytes([bytes[85], bytes[86], bytes[87], bytes[88]]);
	if stored != checksum(&bytes[..85]) {
		return Err(Fault::Corrupt { at });
	}
	let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
	if version != VERSION {
		return Err(Fault::Version { found: version });
	}
	let mut record = Record::default();
	record.local.kind = bytes[4];
	record.local.bytes.copy_from_slice(&bytes[5..11]);
	record.peer.kind = bytes[11];
	record.peer.bytes.copy_from_slice(&bytes[12..18]);
	record.key.copy_from_slice(&bytes[18..34]);
	record.security = bytes[34];
	record.enabled = bytes[35] != 0;
	record.name_len = bytes[36];
	record.name.copy_from_slice(&bytes[37..85]);
	if !known_kind(record.local.kind) {
		return Err(Fault::AddressKind { kind: record.local.kind });
	}
	if !known_kind(record.peer.kind) {
		return Err(Fault::AddressKind { kind: record.peer.kind });
	}
	if record.name_len as usize > MAX_NAME {
		return Err(Fault::Name { len: record.name_len });
	}
	Ok(record)
}

/// What reading a whole file produced.
///
/// A FILE IS A SEQUENCE OF RECORDS AND ONE BAD RECORD IS NOT A BAD FILE. A store that refused the
/// whole file for one damaged record would lose every other bond on the machine to one torn sector -
/// so the good ones are kept, the bad ones are counted, and the count is reported so that something
/// can say so out loud rather than a peer quietly not being there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Loaded {
	pub records: [Record; MAX_RECORDS],
	pub len: usize,
	/// How many records were unreadable. Each of them is a bond that is UNAVAILABLE - the peer it
	/// belongs to will not connect - and never a peer that may be paired again without an operator
	/// saying so.
	pub unreadable: usize,
}

impl Loaded {
	pub fn entries(&self) -> &[Record] {
		&self.records[..self.len]
	}
}

/// Read a whole store.
pub fn load(bytes: &[u8]) -> Result<Loaded, Fault> {
	if bytes.len() > MAX_FILE_BYTES {
		return Err(Fault::TooLarge { len: bytes.len() });
	}
	if bytes.len() % RECORD_BYTES != 0 {
		return Err(Fault::Ragged { len: bytes.len() });
	}
	let count = bytes.len() / RECORD_BYTES;
	if count > MAX_RECORDS {
		return Err(Fault::TooMany { records: count });
	}
	let mut loaded = Loaded { records: [Record::default(); MAX_RECORDS], len: 0, unreadable: 0 };
	for at in 0..count {
		match decode(&bytes[at * RECORD_BYTES..(at + 1) * RECORD_BYTES], at) {
			Ok(record) => {
				loaded.records[loaded.len] = record;
				loaded.len += 1;
			}
			Err(_) => loaded.unreadable += 1,
		}
	}
	Ok(loaded)
}

/// The store as a table, with the rules a request is answered under.
#[derive(Clone, Copy)]
pub struct Store {
	loaded: Loaded,
}

/// Why a request was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// No record for that controller and peer.
	NotFound,
	/// Every slot holds a bond.
	Full,
	/// A record whose controller is not the one asking. ONE MACHINE'S KEY IS NOT ANOTHER'S, and a
	/// store answering by peer address alone would hand a second controller the first one's link.
	WrongController,
	/// A record this store cannot write: an address kind it does not know, or a name past the field.
	Malformed(Fault),
}

impl Store {
	pub const fn new(loaded: Loaded) -> Store {
		Store { loaded }
	}

	pub const fn empty() -> Store {
		Store { loaded: Loaded { records: [Record { local: Address { kind: 0, bytes: [0; 6] }, peer: Address { kind: 0, bytes: [0; 6] }, key: [0; 16], security: 0, enabled: false, name: [0; MAX_NAME], name_len: 0 }; MAX_RECORDS], len: 0, unreadable: 0 } }
	}

	pub const fn unreadable(&self) -> usize {
		self.loaded.unreadable
	}

	pub fn entries(&self) -> &[Record] {
		self.loaded.entries()
	}

	/// The record for this controller and peer.
	pub fn lookup(&self, local: &Address, peer: &Address) -> Result<&Record, Refusal> {
		self.loaded.entries().iter().find(|record| record.local == *local && record.peer == *peer).ok_or(Refusal::NotFound)
	}

	/// Every record for this controller.
	pub fn for_controller<'a>(&'a self, local: &'a Address) -> impl Iterator<Item = &'a Record> + 'a {
		self.loaded.entries().iter().filter(move |record| record.local == *local)
	}

	/// Store or replace one.
	///
	/// REPLACE IS THE ORDINARY CASE: pairing a peer that is already bonded produces a new key, and a
	/// store that appended would hold two records for one peer - of which the lookup would find
	/// whichever came first, which is the old one.
	pub fn put(&mut self, record: &Record) -> Result<(), Refusal> {
		if !known_kind(record.local.kind) {
			return Err(Refusal::Malformed(Fault::AddressKind { kind: record.local.kind }));
		}
		if !known_kind(record.peer.kind) {
			return Err(Refusal::Malformed(Fault::AddressKind { kind: record.peer.kind }));
		}
		if record.name_len as usize > MAX_NAME {
			return Err(Refusal::Malformed(Fault::Name { len: record.name_len }));
		}
		if let Some(at) = self.loaded.entries().iter().position(|held| held.local == record.local && held.peer == record.peer) {
			self.loaded.records[at] = *record;
			return Ok(());
		}
		if self.loaded.len >= MAX_RECORDS {
			return Err(Refusal::Full);
		}
		self.loaded.records[self.loaded.len] = *record;
		self.loaded.len += 1;
		Ok(())
	}

	/// Delete one, answering `NotFound` for a record that was not there - which is a different
	/// answer from a delete that failed.
	pub fn delete(&mut self, local: &Address, peer: &Address) -> Result<(), Refusal> {
		let Some(at) = self.loaded.entries().iter().position(|record| record.local == *local && record.peer == *peer) else {
			return Err(Refusal::NotFound);
		};
		// The last entry fills the gap: this is a set and nothing indexes across a delete.
		self.loaded.len -= 1;
		self.loaded.records[at] = self.loaded.records[self.loaded.len];
		self.loaded.records[self.loaded.len] = Record::default();
		Ok(())
	}

	/// The whole store's bytes, for the write that replaces the file.
	///
	/// ALL OF IT AND NOT A DELTA. The file is replaced whole under the writer session's own
	/// commit, which is what makes a store atomic: a partially written bond is a key that decrypts
	/// nothing and a peer that cannot be paired again without being forgotten first.
	pub fn bytes(&self) -> alloc::vec::Vec<u8> {
		let mut out = alloc::vec::Vec::with_capacity(self.loaded.len * RECORD_BYTES);
		for record in self.loaded.entries() {
			out.extend_from_slice(&encode(record));
		}
		out
	}
}

#[cfg(test)]
mod tests;
