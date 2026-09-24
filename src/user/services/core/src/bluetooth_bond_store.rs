// BluetoothBondStore - the confined keeper of link keys, and nothing else.
//
// A BOND IS A LONG-TERM KEY and the little that goes with it: which controller it belongs to, which
// peer, what the link it came from was worth, and whether an operator has made that peer an input
// source. It outlives the service that uses it, the boot and the machine being switched off, which
// is the whole reason it is written down - a device that had to be paired again after every reboot
// is a device nobody pairs once.
//
// WHY THIS IS A SEPARATE PROGRAM. The key could live inside BluetoothService, and then the process
// that parses a stranger's radio packets would be the process holding every link key on the machine.
// It is here instead, in a Domain of its own, reachable over ONE capability that ServiceManager
// grants to BluetoothService and to nothing else. That does not make a key unreachable - the stack
// must issue encryption commands with it - but it makes reaching one a typed operation on a named
// peer rather than a pointer into the address space where the parser runs.
//
// AND "ASKED RATHER THAN READ" IS THE WHOLE OF THE CLAIM. This is not an enclave and the milestone
// does not pretend it is: a trusted storage administrator can read the file, and the threat model
// says so. What this buys is that a defect in the protocol code cannot walk the key table, because
// the key table is not in that address space.
//
// THE FILE IS REPLACED WHOLE, UNDER THE WRITER SESSION'S OWN COMMIT. A partially written bond is a
// key that decrypts nothing and a peer that cannot be paired again without being forgotten first,
// so there is no in-place edit and no remove-then-rename: the staged bytes become the file's
// contents at `commit` or they do not become anything.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::bluetooth_bond_store::{self, Service};
use proto::system::{BondRecord, Error, OpenOpts, PeerAddress, PeerKind, SecurityLevel, WriterMode, volume, writer};
use rt::*;
use service_logic::bond_store::{Address, MAX_NAME, Record, Refusal, Store, load};

include!(concat!(env!("OUT_DIR"), "/roles_bluetooth_bond_store.rs"));

// Where the store lives. ABSOLUTE, because a scoped volume client checks the path it is given
// against its scope rather than resolving a relative one - so a bare name would be refused as a path
// outside the scope rather than read as a name inside it. The client cannot reach anything else, so
// this is a name within a granted scope rather than an authority.
const STORE_PATH: &str = "vol://system/state/bluetooth-bonds/bonds.store";

// The largest chunk the writer contract takes. Stated by `writer.write` itself; a client picking its
// own number is how four tools came to pick four different ones, three of which never worked.
const WRITE_CHUNK: usize = 4096;

struct Bonds {
	volume: u64,
	store: Store,
}

// The wire's address and the store's are the same seven bytes in a different shape. Neither is
// derived from the other by a cast: the wire is a bounded list and the store is a fixed array, and a
// list that is not six bytes long is a request this service refuses rather than pads.
fn address_from_wire(address: &PeerAddress) -> Option<Address> {
	if address.bytes.len() != 6 {
		return None;
	}
	let mut bytes = [0u8; 6];
	bytes.copy_from_slice(&address.bytes);
	Some(Address { kind: peer_kind_wire(address.kind), bytes })
}

fn address_to_wire(address: &Address) -> Option<PeerAddress> {
	Some(PeerAddress { kind: peer_kind_from(address.kind)?, bytes: address.bytes.to_vec() })
}

fn peer_kind_wire(kind: PeerKind) -> u8 {
	match kind {
		PeerKind::Public => 1,
		PeerKind::RandomStatic => 2,
	}
}

fn peer_kind_from(kind: u8) -> Option<PeerKind> {
	match kind {
		1 => Some(PeerKind::Public),
		2 => Some(PeerKind::RandomStatic),
		_ => None,
	}
}

fn security_wire(level: SecurityLevel) -> u8 {
	match level {
		SecurityLevel::None => 1,
		SecurityLevel::EncryptedUnauthenticated => 2,
		SecurityLevel::EncryptedAuthenticated => 3,
	}
}

fn security_from(value: u8) -> Option<SecurityLevel> {
	match value {
		1 => Some(SecurityLevel::None),
		2 => Some(SecurityLevel::EncryptedUnauthenticated),
		3 => Some(SecurityLevel::EncryptedAuthenticated),
		_ => None,
	}
}

// A wire record into a stored one. EVERY BOUND IS CHECKED HERE and not at the far end: the key is
// exactly sixteen bytes because that is what a long-term key is, and a shorter one is a caller that
// has confused a key with a nonce rather than one that needs padding.
fn record_from_wire(wire: &BondRecord) -> Option<Record> {
	if wire.version != service_logic::bond_store::VERSION {
		return None;
	}
	if wire.key.len() != 16 {
		return None;
	}
	let mut record = Record { local: address_from_wire(&wire.local)?, peer: address_from_wire(&wire.peer)?, security: security_wire(wire.security), enabled: wire.enabled, ..Record::default() };
	record.key.copy_from_slice(&wire.key);
	let name = wire.name.as_bytes();
	let kept = name.len().min(MAX_NAME);
	record.name[..kept].copy_from_slice(&name[..kept]);
	record.name_len = kept as u8;
	Some(record)
}

// A stored record onto the wire. `key` is filled only where the caller is entitled to it, which is
// `lookup` and never `list` - see `list`.
fn record_to_wire(record: &Record, with_key: bool) -> Option<BondRecord> {
	Some(BondRecord { version: service_logic::bond_store::VERSION, local: address_to_wire(&record.local)?, peer: address_to_wire(&record.peer)?, key: if with_key { record.key.to_vec() } else { Vec::new() }, security: security_from(record.security)?, name: String::from(core::str::from_utf8(record.name()).unwrap_or("")), enabled: record.enabled })
}

impl Bonds {
	// Read the store off the volume. A missing file is an empty store, which is what a machine that
	// has paired nothing looks like; a file that cannot be read at all leaves this service running
	// with nothing, because refusing to start would make one damaged file a boot that does not
	// finish.
	fn read(volume: u64) -> Store {
		if volume == 0 {
			return Store::empty();
		}
		let mut client = volume::Client::new(ChannelTransport { chan: volume });
		let opts = OpenOpts { path: String::from(STORE_PATH), write: false, create: false };
		let opened = match client.open(&opts) {
			Some(Ok(result)) if result.file != 0 => result,
			_ => return Store::empty(),
		};
		let Some(base) = (unsafe { map_object(opened.file) }) else {
			close(opened.file);
			return Store::empty();
		};
		let bytes: &[u8] = unsafe { core::slice::from_raw_parts(base as *const u8, opened.size as usize) };
		let store = match load(bytes) {
			Ok(loaded) => Store::new(loaded),
			// A FILE THIS SERVICE CANNOT READ IS NOT AN EMPTY MACHINE. It is said out loud, because
			// every peer on it will fail to connect and the operator's only other evidence would be
			// a mouse that stopped working.
			Err(_) => {
				print(b"BluetoothBondStore: the bond file is not this store's shape; every bond on it is unavailable\n");
				Store::empty()
			}
		};
		unmap_object(opened.file);
		close(opened.file);
		if store.unreadable() > 0 {
			print(b"BluetoothBondStore: some bonds could not be read and are unavailable; their peers will not reconnect\n");
		}
		store
	}

	// Replace the file with what the store holds now.
	//
	// THE COMMIT IS THE PUBLICATION AND `flush` IS NOT. `flush` confirms what is staged and
	// publishes nothing; a client that flushed and reported success would be reporting a bond that
	// is not on the medium. The answer here is the commit's, and a failed commit is a failed store.
	fn write(&mut self) -> bool {
		if self.volume == 0 {
			return false;
		}
		let bytes = self.store.bytes();
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let session = match client.open_writer(STORE_PATH, &WriterMode::Replace) {
			Some(Ok(handle)) if handle != 0 => handle,
			_ => return false,
		};
		let mut session_client = writer::Client::new(ChannelTransport { chan: session });
		for chunk in bytes.chunks(WRITE_CHUNK) {
			if !matches!(session_client.write(chunk), Some(Ok(_))) {
				// ABORT RATHER THAN LEAVE THE SESSION OPEN. A session abandoned mid-write holds the
				// staged bytes until its channel closes, and the file it would have replaced is
				// untouched either way - saying so is what makes the next attempt a fresh one.
				let _ = session_client.abort();
				close(session);
				return false;
			}
		}
		// AN EMPTY STORE IS A WRITE TOO. A replace session with nothing written publishes an empty
		// file, which is what a machine whose last bond was forgotten must have - not the previous
		// file left in place.
		let committed = matches!(session_client.commit(), Some(Ok(_)));
		close(session);
		committed
	}
}

impl Service for Bonds {
	// The record for this controller and peer, KEY INCLUDED. Answered only over this capability,
	// which ServiceManager grants to BluetoothService alone.
	fn lookup(&mut self, local: PeerAddress, peer: PeerAddress) -> Result<BondRecord, Error> {
		let (Some(local), Some(peer)) = (address_from_wire(&local), address_from_wire(&peer)) else {
			return Err(Error::Invalid);
		};
		match self.store.lookup(&local, &peer) {
			Ok(record) => record_to_wire(record, true).ok_or(Error::Invalid),
			Err(_) => Err(Error::NotFound),
		}
	}

	// Store or replace one, and do not answer until it is on the medium.
	//
	// A BOND REPORTED AS STORED AND NOT WRITTEN IS THE DEFECT THIS ORDER EXISTS TO PREVENT: the
	// caller reports a pairing as bonded on this answer, and a person told they had paired would
	// find the device asking again after a reboot. The in-memory table is rolled back when the write
	// fails, so the two cannot disagree.
	fn store(&mut self, record: BondRecord) -> Result<(), Error> {
		let Some(record) = record_from_wire(&record) else {
			return Err(Error::Invalid);
		};
		let previous = self.store;
		match self.store.put(&record) {
			Ok(()) => {}
			Err(Refusal::Full) => return Err(Error::Exhausted),
			Err(_) => return Err(Error::Invalid),
		}
		if !self.write() {
			self.store = previous;
			return Err(Error::Io);
		}
		Ok(())
	}

	// Delete one, DURABLY AND BEFORE THIS ANSWERS. A forget that reported success and left the
	// record on the volume is a device an operator believes is gone and which reconnects after a
	// reboot.
	fn delete(&mut self, local: PeerAddress, peer: PeerAddress) -> Result<(), Error> {
		let (Some(local), Some(peer)) = (address_from_wire(&local), address_from_wire(&peer)) else {
			return Err(Error::Invalid);
		};
		let previous = self.store;
		if self.store.delete(&local, &peer).is_err() {
			return Err(Error::NotFound);
		}
		if !self.write() {
			self.store = previous;
			return Err(Error::Io);
		}
		Ok(())
	}

	// Every record for this controller, WITHOUT KEY MATERIAL. This is what an operator's list of
	// bonded peers is built from, and building it from `lookup` would mean reading every key on the
	// machine to show a list of names.
	fn list(&mut self, local: PeerAddress) -> Result<Vec<BondRecord>, Error> {
		let Some(local) = address_from_wire(&local) else {
			return Err(Error::Invalid);
		};
		Ok(self.store.for_controller(&local).filter_map(|record| record_to_wire(record, false)).collect())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// 1. the roles the plan says this service is handed: a volume client scoped to this service's
	//    own state directory, and the private endpoint BluetoothService reaches it on.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (storage, service): (u64, u64) = (roles[0], roles[1]);

	// 2. read what is on the medium. A machine that has paired nothing has no file, which is an
	//    empty store and not a failure.
	let mut bonds = Bonds { volume: storage, store: Bonds::read(storage) };
	send_blocking(bootstrap, b"BluetoothBondStore: online", 0);

	// 3. serve the one capability until its holder goes.
	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 1024] = [0u8; 1024];
	serve_multi(service, &mut request, &mut reply, |_chan, req, handle, out, reply_handle| -> Option<usize> { bluetooth_bond_store::dispatch(&mut bonds, req, handle, out, reply_handle) });
	exit();
}
