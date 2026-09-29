use super::*;
use alloc::string::String;

#[test]
fn power_role_wire_is_stable() {
	let sample = PowerRole::Sink;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(PowerRole::decode(&bytes).unwrap(), sample);
}
#[test]
fn data_role_wire_is_stable() {
	let sample = DataRole::Device;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(DataRole::decode(&bytes).unwrap(), sample);
}
#[test]
fn transport_wire_is_stable() {
	let sample = Transport::Ucsi;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Transport::decode(&bytes).unwrap(), sample);
}
#[test]
fn capabilities_wire_is_stable() {
	let sample = Capabilities { transport: Transport::Ucsi, sink: true, source: true, data_device: true, data_host: true, power_delivery: true, ucsi_version: 7, ucsi_features: 7, alternate_mode_override: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 1, 1, 1, 7, 0, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(Capabilities::decode(&bytes).unwrap(), sample);
}
#[test]
fn partner_kind_wire_is_stable() {
	let sample = PartnerKind::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PartnerKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn orientation_wire_is_stable() {
	let sample = Orientation::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Orientation::decode(&bytes).unwrap(), sample);
}
#[test]
fn operation_mode_wire_is_stable() {
	let sample = OperationMode::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(OperationMode::decode(&bytes).unwrap(), sample);
}
#[test]
fn typec_current_wire_is_stable() {
	let sample = TypecCurrent::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(TypecCurrent::decode(&bytes).unwrap(), sample);
}
#[test]
fn offer_wire_is_stable() {
	let sample = Offer { position: 7, object: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Offer::decode(&bytes).unwrap(), sample);
}
#[test]
fn contract_wire_is_stable() {
	let sample = Contract { offer: Offer { position: 7, object: 7 }, operating: 7, maximum: 7, capability_mismatch: true, in_transition: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(Contract::decode(&bytes).unwrap(), sample);
}
#[test]
fn cable_wire_is_stable() {
	let sample = Cable { speed: 7, current: 7, vbus: true, active: true, directional: true, plug_end: 7, alternate_modes: true, pd_revision: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 1, 1, 1, 7, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Cable::decode(&bytes).unwrap(), sample);
}
#[test]
fn alternate_mode_wire_is_stable() {
	let sample = AlternateMode { svid: 7, vdo: 7, entered: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(AlternateMode::decode(&bytes).unwrap(), sample);
}
#[test]
fn displayport_wire_is_stable() {
	let sample = Displayport { pin_assignment: 7, hot_plug: Some(true) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(Displayport::decode(&bytes).unwrap(), sample);
}
#[test]
fn request_kind_wire_is_stable() {
	let sample = RequestKind::DataRoleSwap;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(RequestKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn refusal_wire_is_stable() {
	let sample = Refusal::ConnectorCannot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Refusal::decode(&bytes).unwrap(), sample);
}
#[test]
fn refused_request_wire_is_stable() {
	let sample = RefusedRequest { kind: RequestKind::DataRoleSwap, reason: Refusal::ConnectorCannot, error: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(RefusedRequest::decode(&bytes).unwrap(), sample);
}
#[test]
fn connector_wire_is_stable() {
	let sample = Connector { number: 7, capabilities: Capabilities { transport: Transport::Ucsi, sink: true, source: true, data_device: true, data_host: true, power_delivery: true, ucsi_version: 7, ucsi_features: 7, alternate_mode_override: true }, partner: PartnerKind::None, orientation: Orientation::Unknown, power_role: Some(PowerRole::Sink), data_role: Some(DataRole::Device), operation_mode: OperationMode::Unknown, contract: Some(Contract { offer: Offer { position: 7, object: 7 }, operating: 7, maximum: 7, capability_mismatch: true, in_transition: true }), typec_current: TypecCurrent::None, offers: alloc::vec![Offer { position: 7, object: 7 }], cable: Some(Cable { speed: 7, current: 7, vbus: true, active: true, directional: true, plug_end: 7, alternate_modes: true, pd_revision: 7 }), modes: alloc::vec![AlternateMode { svid: 7, vdo: 7, entered: true }], displayport: Some(Displayport { pin_assignment: 7, hot_plug: Some(true) }), last_refusal: Some(RefusedRequest { kind: RequestKind::DataRoleSwap, reason: Refusal::ConnectorCannot, error: 7 }), answering: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		1,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		1,
		1,
		1,
		1,
		0,
		1,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		1,
		0,
		7,
		7,
		0,
		0,
		0,
		1,
		7,
		0,
		7,
		0,
		1,
		1,
		1,
		7,
		1,
		7,
		1,
		0,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		7,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		0,
		0,
		1,
	];
	assert_eq!(bytes, golden);
	assert_eq!(Connector::decode(&bytes).unwrap(), sample);
}
#[test]
fn update_kind_wire_is_stable() {
	let sample = UpdateKind::Snapshot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(UpdateKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_update_wire_is_stable() {
	let sample = ProviderUpdate { revision: 7, kind: UpdateKind::Snapshot, connector: Some(Connector { number: 7, capabilities: Capabilities { transport: Transport::Ucsi, sink: true, source: true, data_device: true, data_host: true, power_delivery: true, ucsi_version: 7, ucsi_features: 7, alternate_mode_override: true }, partner: PartnerKind::None, orientation: Orientation::Unknown, power_role: Some(PowerRole::Sink), data_role: Some(DataRole::Device), operation_mode: OperationMode::Unknown, contract: Some(Contract { offer: Offer { position: 7, object: 7 }, operating: 7, maximum: 7, capability_mismatch: true, in_transition: true }), typec_current: TypecCurrent::None, offers: alloc::vec![Offer { position: 7, object: 7 }], cable: Some(Cable { speed: 7, current: 7, vbus: true, active: true, directional: true, plug_end: 7, alternate_modes: true, pd_revision: 7 }), modes: alloc::vec![AlternateMode { svid: 7, vdo: 7, entered: true }], displayport: Some(Displayport { pin_assignment: 7, hot_plug: Some(true) }), last_refusal: Some(RefusedRequest { kind: RequestKind::DataRoleSwap, reason: Refusal::ConnectorCannot, error: 7 }), answering: true }), gone: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		1,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		1,
		1,
		1,
		1,
		0,
		1,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		1,
		0,
		7,
		7,
		0,
		0,
		0,
		1,
		7,
		0,
		7,
		0,
		1,
		1,
		1,
		7,
		1,
		7,
		1,
		0,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		7,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		0,
		0,
		1,
		1,
		7,
	];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderUpdate::decode(&bytes).unwrap(), sample);
}
#[test]
fn request_wire_is_stable() {
	let sample = Request { kind: RequestKind::DataRoleSwap, connector: 7, data_role: Some(DataRole::Device), power_role: Some(PowerRole::Sink), svid: 7, vdo: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1, 1, 1, 1, 7, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Request::decode(&bytes).unwrap(), sample);
}
#[test]
fn outcome_wire_is_stable() {
	let sample = Outcome::Done;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Outcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn answer_wire_is_stable() {
	let sample = Answer { outcome: Outcome::Done, reason: Some(Refusal::ConnectorCannot), error: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Answer::decode(&bytes).unwrap(), sample);
}
#[test]
fn connector_id_wire_is_stable() {
	let sample = ConnectorId { slot: 7, generation: 7, binding_generation: 7, number: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ConnectorId::decode(&bytes).unwrap(), sample);
}
#[test]
fn connector_snapshot_wire_is_stable() {
	let sample = ConnectorSnapshot { id: ConnectorId { slot: 7, generation: 7, binding_generation: 7, number: 7 }, received: 7, connector: Connector { number: 7, capabilities: Capabilities { transport: Transport::Ucsi, sink: true, source: true, data_device: true, data_host: true, power_delivery: true, ucsi_version: 7, ucsi_features: 7, alternate_mode_override: true }, partner: PartnerKind::None, orientation: Orientation::Unknown, power_role: Some(PowerRole::Sink), data_role: Some(DataRole::Device), operation_mode: OperationMode::Unknown, contract: Some(Contract { offer: Offer { position: 7, object: 7 }, operating: 7, maximum: 7, capability_mismatch: true, in_transition: true }), typec_current: TypecCurrent::None, offers: alloc::vec![Offer { position: 7, object: 7 }], cable: Some(Cable { speed: 7, current: 7, vbus: true, active: true, directional: true, plug_end: 7, alternate_modes: true, pd_revision: 7 }), modes: alloc::vec![AlternateMode { svid: 7, vdo: 7, entered: true }], displayport: Some(Displayport { pin_assignment: 7, hot_plug: Some(true) }), last_refusal: Some(RefusedRequest { kind: RequestKind::DataRoleSwap, reason: Refusal::ConnectorCannot, error: 7 }), answering: true } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		1,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		1,
		1,
		1,
		1,
		0,
		1,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		1,
		0,
		7,
		7,
		0,
		0,
		0,
		1,
		7,
		0,
		7,
		0,
		1,
		1,
		1,
		7,
		1,
		7,
		1,
		0,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		7,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		0,
		0,
		1,
	];
	assert_eq!(bytes, golden);
	assert_eq!(ConnectorSnapshot::decode(&bytes).unwrap(), sample);
}
#[test]
fn change_kind_wire_is_stable() {
	let sample = ChangeKind::Snapshot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ChangeKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn typec_change_wire_is_stable() {
	let sample = TypecChange { epoch: 7, revision: 7, kind: ChangeKind::Snapshot, connector: Some(ConnectorSnapshot { id: ConnectorId { slot: 7, generation: 7, binding_generation: 7, number: 7 }, received: 7, connector: Connector { number: 7, capabilities: Capabilities { transport: Transport::Ucsi, sink: true, source: true, data_device: true, data_host: true, power_delivery: true, ucsi_version: 7, ucsi_features: 7, alternate_mode_override: true }, partner: PartnerKind::None, orientation: Orientation::Unknown, power_role: Some(PowerRole::Sink), data_role: Some(DataRole::Device), operation_mode: OperationMode::Unknown, contract: Some(Contract { offer: Offer { position: 7, object: 7 }, operating: 7, maximum: 7, capability_mismatch: true, in_transition: true }), typec_current: TypecCurrent::None, offers: alloc::vec![Offer { position: 7, object: 7 }], cable: Some(Cable { speed: 7, current: 7, vbus: true, active: true, directional: true, plug_end: 7, alternate_modes: true, pd_revision: 7 }), modes: alloc::vec![AlternateMode { svid: 7, vdo: 7, entered: true }], displayport: Some(Displayport { pin_assignment: 7, hot_plug: Some(true) }), last_refusal: Some(RefusedRequest { kind: RequestKind::DataRoleSwap, reason: Refusal::ConnectorCannot, error: 7 }), answering: true } }), gone: Some(ConnectorId { slot: 7, generation: 7, binding_generation: 7, number: 7 }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		1,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		1,
		1,
		1,
		1,
		0,
		1,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		1,
		0,
		7,
		7,
		0,
		0,
		0,
		1,
		7,
		0,
		7,
		0,
		1,
		1,
		1,
		7,
		1,
		7,
		1,
		0,
		7,
		0,
		7,
		0,
		0,
		0,
		1,
		1,
		7,
		1,
		1,
		1,
		1,
		1,
		7,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
	];
	assert_eq!(bytes, golden);
	assert_eq!(TypecChange::decode(&bytes).unwrap(), sample);
}
