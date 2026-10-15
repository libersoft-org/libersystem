use crate::codec::{Handles, Reader};
use crate::generated::liber::bluetooth::v1::{AudioEndpoint, AudioEvent, BtCallCommand, EndpointVolume, bluetooth_audio};

// Frozen pre-microphone v1 stream reader: the same four tags and unknown-tag
// refusal as the generated reader at c5811b19. AudioService consumes one IPC
// message before decoding and continues on None, so an unknown event cannot
// retain a message or corrupt the next frame's reader.
fn old_endpoints_read(bytes: &[u8]) -> Option<AudioEvent> {
	let mut reader = Reader::new(bytes);
	let _seq = reader.u32()?;
	let event = match reader.u8()? {
		0 => AudioEvent::Arrived(AudioEndpoint::read(&mut reader)?),
		1 => AudioEvent::Departed(reader.u32()?),
		2 => AudioEvent::Volume(EndpointVolume::read(&mut reader)?),
		3 => AudioEvent::Command(BtCallCommand::read(&mut reader)?),
		_ => return None,
	};
	reader.finish()?;
	Some(event)
}

#[test]
fn an_old_subscriber_skips_the_microphone_event_and_reads_the_next_speaker_event() {
	let events = [
		AudioEvent::MicrophoneVolume(EndpointVolume { id: 7, volume: 40 }),
		AudioEvent::Volume(EndpointVolume { id: 7, volume: 60 }),
		AudioEvent::Command(BtCallCommand::Answer),
		AudioEvent::Departed(7),
	];
	let mut received = alloc::vec::Vec::new();
	for (sequence, event) in events.iter().enumerate() {
		let mut bytes = [0; 64];
		let mut handles = Handles::new();
		let len = bluetooth_audio::endpoints_frame(sequence as u32, event, &mut bytes, &mut handles).unwrap();
		assert!(handles.as_slice().is_empty());
		// This is the old consumer's existing None => continue branch.
		let Some(event) = old_endpoints_read(&bytes[..len]) else { continue };
		received.push(event);
	}
	assert_eq!(received, events[1..]);
	assert_eq!(events[1].encode_vec().unwrap(), [2, 7, 0, 0, 0, 60]);
	assert_eq!(events[2].encode_vec().unwrap(), [3, 1]);
	assert_eq!(events[3].encode_vec().unwrap(), [1, 7, 0, 0, 0]);
	assert_eq!(bluetooth_audio::OP_ENDPOINTS, 1);
	assert_eq!(bluetooth_audio::OP_OPEN, 2);
	assert_eq!(bluetooth_audio::OP_SET_VOLUME, 3);
	assert_eq!(bluetooth_audio::OP_SET_CALL, 4);
}

// THE BOND STORE ANSWERS `list` IN ONE REPLY, so the reply buffer it serves from has to hold the store's whole bound:
// sixty-four records, each listed WITHOUT KEY MATERIAL but with every other field at its wire bound - a 48-byte name, a
// 48-byte alias, all eight trust bits. This is the number `bluetooth_bond_store`'s reply buffer is sized from; it was
// one kibibyte, which held seven such records, and every bond past it read as no bond at all.
const LISTED_BONDS: usize = 64;

#[test]
fn sixty_four_listed_bonds_at_their_wire_bounds_are_one_reply_of_under_ten_kibibytes() {
	use crate::generated::liber::base::v1::Error;
	use crate::generated::liber::bluetooth::v1::{BondLevel, BondRecord, KeyAgreement, PeerAddress, PeerKind, Profile, Radio, SecurityLevel, bluetooth_bond_store};
	use alloc::string::String;
	use alloc::vec::Vec;

	fn address() -> PeerAddress {
		PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![0xAA; 6] }
	}
	fn listed() -> BondRecord {
		BondRecord { version: u32::MAX, local: address(), peer: address(), key: Vec::new(), security: SecurityLevel::EncryptedUnauthenticated, name: String::from_utf8(alloc::vec![b'n'; 48]).unwrap(), enabled: true, radio: Radio::Le, link_key: Vec::new(), link_key_type: u8::MAX, level: BondLevel { agreement: KeyAgreement::LeLegacy, authenticated: true }, trusted: alloc::vec![Profile::Input; 8], alias: String::from_utf8(alloc::vec![b'a'; 48]).unwrap(), irk: Vec::new(), ediv: u16::MAX, rand: Vec::new() }
	}
	struct Store;
	impl bluetooth_bond_store::Service for Store {
		fn lookup(&mut self, _local: PeerAddress, _peer: PeerAddress) -> Result<BondRecord, Error> {
			Err(Error::NotFound)
		}
		fn store(&mut self, _record: BondRecord) -> Result<(), Error> {
			Err(Error::Denied)
		}
		fn delete(&mut self, _local: PeerAddress, _peer: PeerAddress) -> Result<(), Error> {
			Err(Error::NotFound)
		}
		fn list(&mut self, _local: PeerAddress) -> Result<Vec<BondRecord>, Error> {
			Ok((0..LISTED_BONDS).map(|_| listed()).collect())
		}
	}
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&bluetooth_bond_store::OP_LIST.to_le_bytes());
	request.extend_from_slice(&7u32.to_le_bytes());
	request.extend_from_slice(&address().encode_vec().unwrap());
	let mut out = alloc::vec![0u8; 64 * 1024];
	let written = bluetooth_bond_store::dispatch(&mut Store, &request, &mut Handles::new(), &mut out, &mut Handles::new()).expect("a list at its bound is one reply");
	let one = listed().encode_vec().unwrap().len();
	assert_eq!(one, 148, "a listed record at its wire bounds");
	assert_eq!(written, 7 + LISTED_BONDS * one, "the correlation, the result tag, the count and the records");
	assert!(written <= 10 * 1024, "the store serves from ten kibibytes: {written}");
	// And the reply buffer it used to serve from held only this many.
	assert_eq!((1024 - 7) / one, 6);
}
