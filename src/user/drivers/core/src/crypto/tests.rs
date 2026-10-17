use super::*;

fn le32(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
	u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn config(services: u32, algos: u32, key: u32, max_size: u64) -> Config {
	Config { status: STATUS_HW_READY, max_dataqueues: 1, services, cipher_algo_l: algos, max_cipher_key_len: key, max_size }
}

#[test]
// THE OFFSETS ARE THE SPECIFICATION'S, field by field: a header out by four bytes still reaches the device and is
// read as a different request, which is a failure the device reports as `BADMSG` at best.
fn a_session_creation_is_the_control_header_then_the_cipher_parameters_then_the_kind() {
	let request = create_session(AES_CBC, 16, OP_ENCRYPT, 0);
	assert_eq!(request.len(), 72, "the control header and its 56-byte union");
	assert_eq!(le32(&request, 0), 0x0002, "CIPHER_CREATE_SESSION is the cipher service's operation 2");
	assert_eq!(le32(&request, 4), AES_CBC);
	assert_eq!(le32(&request, 8), 0, "no flag");
	assert_eq!(le32(&request, 12), 0, "the data queue the session's requests run on");
	assert_eq!((le32(&request, 16), le32(&request, 20), le32(&request, 24)), (AES_CBC, 16, OP_ENCRYPT), "the cipher's algorithm, key length and direction");
	assert!(request[28..64].iter().all(|byte| *byte == 0), "the cipher parameters' padding and the rest of the 48-byte union are zero");
	assert_eq!(le32(&request, 64), SYM_OP_CIPHER, "a plain cipher session, after the union");
	assert_eq!(le32(&request, 68), 0);
}

#[test]
fn a_session_destruction_names_the_session_after_the_control_header() {
	let request = destroy_session(AES_CBC, 0x0123_4567_89AB_CDEF, 0);
	assert_eq!(le32(&request, 0), 0x0003, "CIPHER_DESTROY_SESSION is the cipher service's operation 3");
	assert_eq!(le32(&request, 4), AES_CBC);
	assert_eq!(le64(&request, 16), 0x0123_4567_89AB_CDEF);
	assert!(request[24..].iter().all(|byte| *byte == 0));
}

#[test]
fn a_cipher_request_is_the_operation_header_then_the_lengths_then_the_kind() {
	let request = cipher_request(CIPHER_DECRYPT, AES_CBC, 7, 16, 64, 64);
	assert_eq!(le32(&request, 0), 0x0001, "CIPHER_DECRYPT is the cipher service's operation 1, ENCRYPT operation 0");
	assert_eq!(le32(&cipher_request(CIPHER_ENCRYPT, AES_CBC, 7, 16, 64, 64), 0), 0x0000);
	assert_eq!(le32(&request, 4), AES_CBC);
	assert_eq!(le64(&request, 8), 7, "the session the request runs on");
	assert_eq!((le32(&request, 16), le32(&request, 20)), (0, 0), "no flag, and the header's padding");
	assert_eq!((le32(&request, 24), le32(&request, 28), le32(&request, 32)), (16, 64, 64), "the IV, source and destination lengths");
	assert!(request[36..64].iter().all(|byte| *byte == 0), "the rest of the 40-byte union");
	assert_eq!(le32(&request, 64), SYM_OP_CIPHER);
}

#[test]
fn what_the_device_wrote_for_a_session_is_its_id_then_its_status() {
	let mut input = [0u8; SESSION_INPUT_LEN];
	input[0..8].copy_from_slice(&42u64.to_le_bytes());
	input[8..12].copy_from_slice(&STATUS_NOTSUPP.to_le_bytes());
	assert_eq!(session_input(&input), Some((42, STATUS_NOTSUPP)));
	assert_eq!(session_input(&input[..15]), None, "a short answer is no answer");
}

#[test]
fn the_configuration_is_read_in_the_specifications_order() {
	let mut bytes = [0u8; 56];
	bytes[0..4].copy_from_slice(&STATUS_HW_READY.to_le_bytes());
	bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
	bytes[8..12].copy_from_slice(&(1u32 << SERVICE_CIPHER).to_le_bytes());
	bytes[12..16].copy_from_slice(&((1u32 << AES_CBC) | (1u32 << 4)).to_le_bytes());
	bytes[36..40].copy_from_slice(&64u32.to_le_bytes());
	bytes[48..56].copy_from_slice(&(1u64 << 20).to_le_bytes());
	let config = Config::parse(&bytes).expect("a whole configuration");
	assert!(config.usable());
	assert!(config.offers(AES_CBC) && config.offers(4) && !config.offers(2), "CBC and CTR, and not ECB - of which this driver names CBC");
	assert_eq!(config.control_queue(), 1, "the control queue follows the one data queue");
	assert_eq!(config.max_key(), MAX_KEY, "the device takes 64 bytes; this driver names keys up to 32");
	assert_eq!(config.max_request(), MAX_DATA, "a megabyte the device would take; one page this driver carries");
	assert_eq!(Config::parse(&bytes[..55]), None);
	let not_ready = Config { status: 0, ..config };
	assert!(!not_ready.usable(), "an engine that has not said it is ready is not used");
	let no_cipher = Config { services: 1 << 4, ..config };
	assert!(!no_cipher.usable(), "an engine with asymmetric ciphers alone serves nothing this contract names");
}

#[test]
fn a_key_is_one_of_aes_three_sizes_and_no_longer_than_the_device_takes() {
	let roomy = config(1, 1 << AES_CBC, 64, 0);
	for len in [16, 24, 32] {
		assert_eq!(check_key(len, &roomy), Ok(()));
	}
	for len in [0, 15, 17, 31, 33, 64] {
		assert_eq!(check_key(len, &roomy), Err(Refusal::Key), "{len} bytes");
	}
	let small = config(1, 1 << AES_CBC, 16, 0);
	assert_eq!(check_key(32, &small), Err(Refusal::Key), "a 32-byte key on a device that takes 16");
}

#[test]
fn a_request_carries_one_block_of_iv_and_whole_blocks_of_data() {
	let device = config(1, 1 << AES_CBC, 32, 1024);
	assert_eq!(check_request(16, 64, &device), Ok(()));
	assert_eq!(check_request(16, 63, &device), Err(Refusal::Data), "CBC takes whole blocks");
	assert_eq!(check_request(16, 0, &device), Err(Refusal::Data), "no data is no request");
	assert_eq!(check_request(12, 16, &device), Err(Refusal::Iv));
	assert_eq!(check_request(16, 1040, &device), Err(Refusal::TooLong), "past the device's own bound");
	assert_eq!(check_request(16, 1024, &device), Ok(()));
}

#[test]
// A SESSION BELONGS TO THE CONNECTION THAT OPENED IT, and its number is never handed out again on that connection:
// a late request naming a closed session cannot reach the one opened after it.
fn a_connection_holds_a_bounded_set_of_sessions_and_never_reuses_a_number() {
	let mut sessions = Sessions::new();
	let session = |id: u64| Session { device_id: id, algo: AES_CBC, encrypt: true };
	let first = sessions.insert(session(100)).expect("room");
	assert_eq!(first, 1, "numbered from one, so zero is never a session");
	assert_eq!(sessions.get(first), Some(session(100)));
	assert_eq!(sessions.remove(first), Some(session(100)));
	assert_eq!(sessions.get(first), None, "closed");
	let second = sessions.insert(session(101)).expect("room");
	assert_ne!(second, first, "the closed number is not reused");
	for id in 0..(MAX_SESSIONS as u64 - 1) {
		sessions.insert(session(200 + id)).expect("room for the rest of the bound");
	}
	assert!(!sessions.has_room());
	assert_eq!(sessions.insert(session(300)), None, "one past the bound is refused");
	let drained: std::vec::Vec<u64> = sessions.drain().map(|session| session.device_id).collect();
	assert_eq!(drained.len(), MAX_SESSIONS, "a connection that has gone gives every session back for destruction");
	assert!(sessions.is_empty());
}

#[test]
fn key_material_is_overwritten() {
	let mut key = [0xA5u8; 32];
	zeroize(&mut key);
	assert!(key.iter().all(|byte| *byte == 0));
}
