use super::*;
use alloc::string::String;

#[test]
fn peer_kind_wire_is_stable() {
	let sample = PeerKind::Public;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(PeerKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn peer_address_wire_is_stable() {
	let sample = PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PeerAddress::decode(&bytes).unwrap(), sample);
}
#[test]
fn controller_info_wire_is_stable() {
	let sample = ControllerInfo { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, powered: true, secure_connections: true, epoch: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ControllerInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn scan_result_wire_is_stable() {
	let sample = ScanResult { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, name: String::from("x"), rssi: 7, human_interface: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 0, 120, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(ScanResult::decode(&bytes).unwrap(), sample);
}
#[test]
fn pairing_state_wire_is_stable() {
	let sample = PairingState::Idle;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(PairingState::decode(&bytes).unwrap(), sample);
}
#[test]
fn security_level_wire_is_stable() {
	let sample = SecurityLevel::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SecurityLevel::decode(&bytes).unwrap(), sample);
}
#[test]
fn pairing_progress_wire_is_stable() {
	let sample = PairingProgress { state: PairingState::Idle, security: SecurityLevel::None, address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PairingProgress::decode(&bytes).unwrap(), sample);
}
#[test]
fn bonded_peer_wire_is_stable() {
	let sample = BondedPeer { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, name: String::from("x"), enabled: true, security: SecurityLevel::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 0, 120, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(BondedPeer::decode(&bytes).unwrap(), sample);
}
#[test]
fn scan_handle_wire_is_stable() {
	let sample = ScanHandle { id: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ScanHandle::decode(&bytes).unwrap(), sample);
}
#[test]
fn mouse_report_wire_is_stable() {
	let sample = MouseReport { dx: 7, dy: 7, wheel: 7, buttons: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(MouseReport::decode(&bytes).unwrap(), sample);
}
#[test]
fn enabled_peer_wire_is_stable() {
	let sample = EnabledPeer { controller: 7, peer: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(EnabledPeer::decode(&bytes).unwrap(), sample);
}
#[test]
fn bond_record_wire_is_stable() {
	let sample = BondRecord { version: 7, local: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, peer: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, key: alloc::vec![7], security: SecurityLevel::None, name: String::from("x"), enabled: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 1, 0, 7, 1, 1, 0, 7, 1, 0, 7, 1, 1, 0, 120, 1];
	assert_eq!(bytes, golden);
	assert_eq!(BondRecord::decode(&bytes).unwrap(), sample);
}
