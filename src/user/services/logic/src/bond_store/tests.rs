use super::{Address, Fault, MAX_FILE_BYTES, MAX_NAME, MAX_RECORDS, RECORD_BYTES, Record, Refusal, Store, VERSION, decode, encode, load};

fn address(kind: u8, last: u8) -> Address {
	Address { kind, bytes: [0x00, 0x11, 0x22, 0x33, 0x44, last] }
}

fn record(local: u8, peer: u8) -> Record {
	let mut out = Record { local: address(1, local), peer: address(2, peer), key: [0xab; 16], security: 2, enabled: true, ..Record::default() };
	let name = b"a mouse";
	out.name[..name.len()].copy_from_slice(name);
	out.name_len = name.len() as u8;
	out
}

#[test]
// A RECORD ROUND-TRIPS, which is the ordinary case and the one every other test is a deviation from.
fn a_record_written_and_read_back_is_the_record() {
	let written = record(1, 2);
	let bytes = encode(&written);
	assert_eq!(bytes.len(), RECORD_BYTES);
	assert_eq!(decode(&bytes, 0), Ok(written));
	assert_eq!(decode(&bytes, 0).map(|r| r.name().to_vec()), Ok(b"a mouse".to_vec()));
}

#[test]
// A CORRUPT RECORD IS UNAVAILABLE AND NEVER AN IMPLICIT NEW PEER. This is the rule the module is
// shaped by: a damaged bond must answer "I do not have that", not "here is one" and not "there is no
// such peer, pair it again without asking".
fn a_damaged_record_is_refused_rather_than_read() {
	let mut bytes = encode(&record(1, 2));
	// A byte of the key, flipped.
	bytes[20] ^= 0xff;
	assert_eq!(decode(&bytes, 7), Err(Fault::Corrupt { at: 7 }));
	// A byte of the name, flipped - the checksum covers every field and not only the key.
	let mut name = encode(&record(1, 2));
	name[40] ^= 0x01;
	assert_eq!(decode(&name, 0), Err(Fault::Corrupt { at: 0 }));
	// AND THE CHECKSUM IS CHECKED BEFORE ANY FIELD IS BELIEVED: a record whose bytes are damaged has
	// a damaged name length too, and reporting that would name the wrong fault.
	let mut length = encode(&record(1, 2));
	length[36] = 200;
	assert_eq!(decode(&length, 0), Err(Fault::Corrupt { at: 0 }), "corrupt, not a bad name length");
}

#[test]
// A RECORD READ UNDER THE WRONG LAYOUT IS A KEY APPLIED TO THE WRONG LINK, which is the one outcome
// worse than having no key at all.
fn a_version_this_build_does_not_know_is_refused() {
	let mut bytes = encode(&record(1, 2));
	bytes[..4].copy_from_slice(&(VERSION + 1).to_le_bytes());
	// The checksum has to be right, or this would be testing the checksum instead.
	let sum = super::checksum(&bytes[..85]);
	bytes[85..89].copy_from_slice(&sum.to_le_bytes());
	assert_eq!(decode(&bytes, 0), Err(Fault::Version { found: VERSION + 1 }));
}

#[test]
// AN ADDRESS KIND THIS STORE DOES NOT KNOW IS A RECORD WRITTEN BY SOMETHING ELSE. Resolvable
// private addresses need a resolution step this milestone excludes; treated as static, a bond would
// be kept against an address that is different by the time the peer reconnects.
fn an_address_kind_this_profile_does_not_speak_is_refused() {
	let mut odd = record(1, 2);
	odd.peer.kind = 3;
	let bytes = encode(&odd);
	assert_eq!(decode(&bytes, 0), Err(Fault::AddressKind { kind: 3 }));
	let mut store = Store::empty();
	assert_eq!(store.put(&odd), Err(Refusal::Malformed(Fault::AddressKind { kind: 3 })));
	// And a name past the field that holds it.
	let mut long = record(1, 2);
	long.name_len = MAX_NAME as u8 + 1;
	assert_eq!(store.put(&long), Err(Refusal::Malformed(Fault::Name { len: MAX_NAME as u8 + 1 })));
}

#[test]
// ONE BAD RECORD IS NOT A BAD FILE. A store that refused the whole file for one damaged record would
// lose every other bond on the machine to one torn sector - so the good ones are kept, the bad ones
// are counted, and the count is reported rather than a peer quietly not being there.
fn a_file_with_one_damaged_record_keeps_the_others_and_counts_the_loss() {
	let mut file = alloc::vec::Vec::new();
	file.extend_from_slice(&encode(&record(1, 1)));
	let mut bad = encode(&record(1, 2));
	bad[30] ^= 0xff;
	file.extend_from_slice(&bad);
	file.extend_from_slice(&encode(&record(1, 3)));
	let loaded = load(&file).expect("a well-sized file");
	assert_eq!(loaded.len, 2);
	assert_eq!(loaded.unreadable, 1);
	assert_eq!(loaded.entries()[0], record(1, 1));
	assert_eq!(loaded.entries()[1], record(1, 3), "the one after the damaged record is still read");
}

#[test]
// A FILE LARGER THAN THIS STORE WILL HOLD IS NOT THIS STORE'S, whatever it contains, and is refused
// rather than parsed for whatever fits.
fn a_file_that_is_not_this_stores_shape_is_refused_whole() {
	assert_eq!(load(&[0u8; MAX_FILE_BYTES + 1]), Err(Fault::TooLarge { len: MAX_FILE_BYTES + 1 }));
	assert_eq!(load(&[0u8; RECORD_BYTES + 1]), Err(Fault::Ragged { len: RECORD_BYTES + 1 }));
	let too_many = alloc::vec![0u8; RECORD_BYTES * (MAX_RECORDS + 1)];
	assert_eq!(load(&too_many), Err(Fault::TooMany { records: MAX_RECORDS + 1 }));
	// An empty file is an empty store and not a fault: that is what a machine that has paired
	// nothing looks like.
	let empty = load(&[]).expect("an empty file");
	assert_eq!((empty.len, empty.unreadable), (0, 0));
}

#[test]
// ONE MACHINE'S KEY IS NOT ANOTHER'S. A store answering by peer address alone would hand a second
// controller the first one's link.
fn a_lookup_matches_the_controller_as_well_as_the_peer() {
	let mut store = Store::empty();
	let first = record(1, 9);
	let mut second = record(1, 9);
	second.local = address(1, 2);
	second.key = [0xcd; 16];
	store.put(&first).expect("room");
	store.put(&second).expect("room");
	assert_eq!(store.lookup(&first.local, &first.peer).map(|r| r.key), Ok([0xab; 16]));
	assert_eq!(store.lookup(&second.local, &second.peer).map(|r| r.key), Ok([0xcd; 16]));
	assert_eq!(store.lookup(&address(1, 77), &first.peer), Err(Refusal::NotFound));
	// And one controller's list is its own.
	assert_eq!(store.for_controller(&first.local).count(), 1);
}

#[test]
// REPLACE IS THE ORDINARY CASE. Pairing a peer that is already bonded produces a new key, and a
// store that appended would hold two records for one peer - of which a lookup finds the old one.
fn pairing_a_bonded_peer_again_replaces_its_record_rather_than_adding_one() {
	let mut store = Store::empty();
	store.put(&record(1, 5)).expect("room");
	let mut again = record(1, 5);
	again.key = [0x77; 16];
	again.enabled = false;
	store.put(&again).expect("replace");
	assert_eq!(store.entries().len(), 1);
	assert_eq!(store.lookup(&again.local, &again.peer).map(|r| r.key), Ok([0x77; 16]));
	assert_eq!(store.lookup(&again.local, &again.peer).map(|r| r.enabled), Ok(false));
}

#[test]
// THE STORE IS BOUNDED AND A DELETE GIVES ITS SLOT BACK. A store that filled and never released
// would stop admitting bonds after sixty-four pairings for the life of the machine.
fn the_store_is_bounded_and_a_delete_returns_the_slot() {
	let mut store = Store::empty();
	for at in 0..MAX_RECORDS {
		store.put(&record(1, at as u8)).expect("room");
	}
	assert_eq!(store.put(&record(1, 200)), Err(Refusal::Full));
	let gone = record(1, 0);
	assert_eq!(store.delete(&gone.local, &gone.peer), Ok(()));
	assert_eq!(store.delete(&gone.local, &gone.peer), Err(Refusal::NotFound), "a delete of what is not there is a different answer from one that failed");
	assert_eq!(store.put(&record(1, 200)), Ok(()));
	assert_eq!(store.entries().len(), MAX_RECORDS);
}

#[test]
// THE FILE IS REPLACED WHOLE, which is what makes a store atomic under the writer session's commit:
// a partially written bond is a key that decrypts nothing and a peer that cannot be paired again
// without being forgotten first.
fn the_whole_store_round_trips_through_its_own_bytes() {
	let mut store = Store::empty();
	for at in 0..5u8 {
		store.put(&record(1, at)).expect("room");
	}
	let bytes = store.bytes();
	assert_eq!(bytes.len(), 5 * RECORD_BYTES);
	let reloaded = load(&bytes).expect("its own bytes");
	assert_eq!(reloaded.len, 5);
	assert_eq!(reloaded.unreadable, 0);
	assert_eq!(reloaded.entries(), store.entries());
	assert!(bytes.len() <= MAX_FILE_BYTES);
}
