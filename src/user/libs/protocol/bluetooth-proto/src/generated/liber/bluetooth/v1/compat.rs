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
fn radio_wire_is_stable() {
	let sample = Radio::Classic;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Radio::decode(&bytes).unwrap(), sample);
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
	let sample = ControllerInfo { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, powered: true, secure_connections: true, epoch: 7, classic: true, connectable: true, discoverable: true, pairable: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 1, 7, 0, 0, 0, 1, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(ControllerInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn service_class_wire_is_stable() {
	let sample = ServiceClass { uuid: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ServiceClass::decode(&bytes).unwrap(), sample);
}
#[test]
fn scan_result_wire_is_stable() {
	let sample = ScanResult { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, name: String::from("x"), rssi: 7, human_interface: true, radio: Radio::Classic, class_of_device: 7, services: alloc::vec![ServiceClass { uuid: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 0, 120, 7, 0, 0, 0, 1, 1, 7, 0, 0, 0, 1, 0, 7, 0];
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
fn key_agreement_wire_is_stable() {
	let sample = KeyAgreement::Legacy;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(KeyAgreement::decode(&bytes).unwrap(), sample);
}
#[test]
fn bond_level_wire_is_stable() {
	let sample = BondLevel { agreement: KeyAgreement::Legacy, authenticated: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(BondLevel::decode(&bytes).unwrap(), sample);
}
#[test]
fn profile_wire_is_stable() {
	let sample = Profile::Input;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Profile::decode(&bytes).unwrap(), sample);
}
#[test]
fn prompt_question_wire_is_stable() {
	let sample = PromptQuestion::Compare;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(PromptQuestion::decode(&bytes).unwrap(), sample);
}
#[test]
fn pairing_prompt_wire_is_stable() {
	let sample = PairingPrompt { id: 7, controller: 7, peer: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, radio: Radio::Classic, question: PromptQuestion::Compare, value: 7, keypresses: 7, deadline_ms: 7, name: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 1, 1, 0, 7, 1, 1, 7, 0, 0, 0, 7, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(PairingPrompt::decode(&bytes).unwrap(), sample);
}
#[test]
fn prompt_reply_wire_is_stable() {
	let sample = PromptReply::Yes;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PromptReply::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_status_wire_is_stable() {
	let sample = DeviceStatus { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, radio: Radio::Classic, name: String::from("x"), alias: String::from("x"), bonded: true, connected: true, level: Some(BondLevel { agreement: KeyAgreement::Legacy, authenticated: true }), trusted: alloc::vec![Profile::Input], connected_profiles: alloc::vec![Profile::Input], battery: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 1, 0, 120, 1, 0, 120, 1, 1, 1, 1, 1, 1, 0, 1, 1, 0, 1, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceStatus::decode(&bytes).unwrap(), sample);
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
	let sample = BondedPeer { address: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, name: String::from("x"), enabled: true, security: SecurityLevel::None, radio: Radio::Classic, level: BondLevel { agreement: KeyAgreement::Legacy, authenticated: true }, trusted: alloc::vec![Profile::Input], alias: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 1, 0, 120, 1, 1, 1, 1, 1, 1, 0, 1, 1, 0, 120];
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
fn media_command_wire_is_stable() {
	let sample = MediaCommand::Play;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(MediaCommand::decode(&bytes).unwrap(), sample);
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
fn key_transition_wire_is_stable() {
	let sample = KeyTransition { page: 7, usage: 7, down: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(KeyTransition::decode(&bytes).unwrap(), sample);
}
#[test]
fn gamepad_frame_wire_is_stable() {
	let sample = GamepadFrame { bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(GamepadFrame::decode(&bytes).unwrap(), sample);
}
#[test]
fn input_report_wire_is_stable() {
	let sample = InputReport::Pointer(MouseReport { dx: 7, dy: 7, wheel: 7, buttons: 7 });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(InputReport::decode(&bytes).unwrap(), sample);
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
fn gatt_service_wire_is_stable() {
	let sample = GattService { uuid: 7, start: 7, end: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(GattService::decode(&bytes).unwrap(), sample);
}
#[test]
fn gatt_characteristic_wire_is_stable() {
	let sample = GattCharacteristic { handle: 7, uuid: 7, properties: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(GattCharacteristic::decode(&bytes).unwrap(), sample);
}
#[test]
fn gatt_value_wire_is_stable() {
	let sample = GattValue { handle: 7, value: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(GattValue::decode(&bytes).unwrap(), sample);
}
#[test]
fn bond_record_wire_is_stable() {
	let sample = BondRecord { version: 7, local: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, peer: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, key: alloc::vec![7], security: SecurityLevel::None, name: String::from("x"), enabled: true, radio: Radio::Classic, link_key: alloc::vec![7], link_key_type: 7, level: BondLevel { agreement: KeyAgreement::Legacy, authenticated: true }, trusted: alloc::vec![Profile::Input], alias: String::from("x"), irk: alloc::vec![7], ediv: 7, rand: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 1, 0, 7, 1, 1, 0, 7, 1, 0, 7, 1, 1, 0, 120, 1, 1, 1, 0, 7, 7, 1, 1, 1, 0, 1, 1, 0, 120, 1, 0, 7, 7, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(BondRecord::decode(&bytes).unwrap(), sample);
}
#[test]
fn fixture_action_wire_is_stable() {
	let sample = FixtureAction::Page;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(FixtureAction::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_endpoint_kind_wire_is_stable() {
	let sample = AudioEndpointKind::Output;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AudioEndpointKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_endpoint_wire_is_stable() {
	let sample = AudioEndpoint { id: 7, peer: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, name: String::from("x"), kind: AudioEndpointKind::Output, rate: 7, channels: 7, latency_us: 7, hardware_volume: true, volume: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 1, 0, 7, 1, 0, 120, 1, 7, 0, 0, 0, 7, 7, 0, 0, 0, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(AudioEndpoint::decode(&bytes).unwrap(), sample);
}
#[test]
fn endpoint_volume_wire_is_stable() {
	let sample = EndpointVolume { id: 7, volume: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(EndpointVolume::decode(&bytes).unwrap(), sample);
}
#[test]
fn bt_call_state_wire_is_stable() {
	let sample = BtCallState::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(BtCallState::decode(&bytes).unwrap(), sample);
}
#[test]
fn bt_call_command_wire_is_stable() {
	let sample = BtCallCommand::Answer;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(BtCallCommand::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_event_wire_is_stable() {
	let sample = AudioEvent::Arrived(AudioEndpoint { id: 7, peer: PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![7] }, name: String::from("x"), kind: AudioEndpointKind::Output, rate: 7, channels: 7, latency_us: 7, hardware_volume: true, volume: 7 });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 1, 1, 0, 7, 1, 0, 120, 1, 7, 0, 0, 0, 7, 7, 0, 0, 0, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(AudioEvent::decode(&bytes).unwrap(), sample);
}
