use super::*;

const DISPLAY: u8 = FIRST_LE_DEVICE + 2;
const AT: usize = 2;
const HOST: [u8; 7] = [1, 0xd1, 2, 3, 4, 5, 6];
const PASSKEY: u32 = 531_246;

fn pdu(code: u8, value: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![code];
	out.extend_from_slice(value);
	out
}

fn keyboard() -> LeWorld {
	let mut world = LeWorld::new(7);
	assert!(world.set_io_capability(DISPLAY, 2));
	world.connect(AT, HOST, false);
	let response = world.smp(AT, &[1, 4, 0, 0x0d, 16, 0, 0]);
	assert_eq!(response, [alloc::vec![2, 2, 0, 0x0d, 16, 0, 0]]);
	let answer = world.smp(AT, &pdu(0x0c, &[0x42; 64]));
	assert_eq!(answer, [pdu(0x0c, &public_key(AT))]);
	world
}

// Both deliveries occur in the guest: a person's control call can precede or follow the initiator's first
// commitment. Neither may skip the twenty bit commitments or the final passkey-bound DHKey checks.
#[test]
fn sc_keyboard_checks_twenty_rounds_with_digits_before_or_after_the_first_commitment() {
	for typed_first in [false, true] {
		let mut world = keyboard();
		let pkax = [0x42; 32];
		let pkbx: [u8; 32] = reversed(&public_key(AT)[..32]);
		if typed_first {
			assert!(world.type_passkey(DISPLAY, PASSKEY).unwrap().is_empty());
		}
		for round in 0..20 {
			let na = [round + 1; 16];
			let bit = 0x80 | ((PASSKEY >> round) & 1) as u8;
			let ca = f4(&pkax, &pkbx, &na, bit);
			let answer = world.smp(AT, &pdu(3, &reversed::<16>(&ca)));
			if round == 0 && !typed_first {
				assert!(answer.is_empty(), "the keyboard cannot commit before its person supplies the digits");
				let out = world.type_passkey(DISPLAY, PASSKEY).unwrap();
				assert!(matches!(out.as_slice(), [LeOut::Acl(bytes)] if bytes[8] == 3));
			} else {
				let nb = world.devices[AT].link.as_ref().unwrap().nb;
				assert_eq!(answer, [pdu(3, &reversed::<16>(&f4(&pkbx, &pkax, &nb, bit)))]);
			}
			let nb = world.devices[AT].link.as_ref().unwrap().nb;
			assert_eq!(world.smp(AT, &pdu(4, &reversed::<16>(&na))), [pdu(4, &reversed::<16>(&nb))]);
			assert_eq!(world.devices[AT].link.as_ref().unwrap().round, round + 1);
		}
		let link = world.devices[AT].link.as_ref().unwrap();
		assert_eq!(link.smp, Smp::AwaitCheck);
		let own = world.own(AT);
		let (mac_key, ltk) = f5(&dhkey(AT), &link.na, &link.nb, &HOST, &own);
		let mut r = [0; 16];
		r[12..].copy_from_slice(&PASSKEY.to_be_bytes());
		let ea = f6(&mac_key, &link.na, &link.nb, &r, &[0x0d, 0, 4], &HOST, &own);
		let eb = f6(&mac_key, &link.nb, &link.na, &r, &[0x0d, 0, 2], &own, &HOST);
		assert_eq!(world.smp(AT, &pdu(0x0d, &reversed::<16>(&ea))), [pdu(0x0d, &reversed::<16>(&eb))]);
		let handle = FIRST_HANDLE + AT as u16;
		assert!(world.encrypt(handle, [0; 8], 0, ltk).is_some());
		assert_eq!(world.devices[AT].ltk, Some(ltk));
		assert!(world.devices[AT].link.as_ref().unwrap().encrypted);
		assert!(world.take_log().iter().any(|line| line == "display completed twenty Secure Connections passkey rounds host-shown"));
	}
}

#[test]
fn sc_keyboard_rejects_a_commitment_to_a_different_digit() {
	let mut world = keyboard();
	assert!(world.type_passkey(DISPLAY, 1_000_000).is_none());
	assert!(world.type_passkey(DISPLAY, PASSKEY).is_some());
	assert!(world.type_passkey(DISPLAY, PASSKEY + 1).is_none());
	let pkbx: [u8; 32] = reversed(&public_key(AT)[..32]);
	let na = [0x11; 16];
	let wrong_bit = 0x80 | (((PASSKEY ^ 1) & 1) as u8);
	let wrong = f4(&[0x42; 32], &pkbx, &na, wrong_bit);
	assert_eq!(world.smp(AT, &pdu(3, &reversed::<16>(&wrong))).len(), 1);
	assert_eq!(world.smp(AT, &pdu(4, &reversed::<16>(&na))), [alloc::vec![5, 4]]);
	assert_eq!(world.devices[AT].link.as_ref().unwrap().smp, Smp::Idle);
	assert!(world.devices[AT].ltk.is_none());
}

#[test]
fn fixture_key_restore_is_explicit_and_controls_refuse_live_or_invalid_peers() {
	let mut world = LeWorld::new(7);
	for invalid in [0, FIRST_LE_DEVICE - 1, 14, u8::MAX, bt_le_audio::EARBUD_L, bt_le_audio::EARBUD_R] {
		assert!(!world.set_io_capability(invalid, 2));
		assert!(!world.save_keys(invalid));
		assert!(!world.restore_keys(invalid));
	}
	for unsupported in [1, 3, 4, 5, u8::MAX] {
		assert!(!world.set_io_capability(DISPLAY, unsupported));
	}
	for other_peer in [FIRST_LE_DEVICE, FIRST_LE_DEVICE + 1, FIRST_LE_DEVICE + 3] {
		assert!(!world.set_io_capability(other_peer, 2));
	}
	assert!(world.set_io_capability(DISPLAY, 0));
	assert!(!world.save_keys(DISPLAY));
	assert!(!world.restore_keys(DISPLAY));
	let old = [0x5a; 16];
	world.devices[AT].ltk = Some(old);
	world.devices[AT].ediv = 42;
	world.devices[AT].rand = [0x22; 8];
	world.devices[AT].host_irk = Some([0x33; 16]);
	assert!(world.save_keys(DISPLAY));
	world.connect(AT, HOST, false);
	assert!(!world.set_io_capability(DISPLAY, 2));
	assert!(!world.save_keys(DISPLAY));
	assert!(!world.restore_keys(DISPLAY));
	world.devices[AT].ltk = Some([0xa5; 16]);
	world.disconnect(FIRST_HANDLE + AT as u16, true).unwrap();
	assert_eq!(world.devices[AT].ltk, Some([0xa5; 16]), "disconnect must never silently restore the snapshot");
	assert!(world.restore_keys(DISPLAY));
	assert_eq!(world.devices[AT].ltk, Some(old));
	assert_eq!(world.devices[AT].ediv, 42);
	assert_eq!(world.devices[AT].rand, [0x22; 8]);
	assert_eq!(world.devices[AT].host_irk, Some([0x33; 16]));
	assert!(!world.restore_keys(DISPLAY), "a restore consumes its explicit snapshot");
	world.connect(AT, HOST, false);
	world.encrypt(FIRST_HANDLE + AT as u16, [0; 8], 0, old).unwrap();
	assert!(world.devices[AT].link.as_ref().unwrap().encrypted, "the peer must actually accept the restored key");
}
