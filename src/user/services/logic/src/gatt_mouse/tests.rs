use super::{Discovery, Next, Unsupported, uuid};
use crate::att::{ATTRIBUTE_NOT_FOUND, DEFAULT_MTU, Refusal, op};

// A PEER'S ATTRIBUTE TABLE, and a server that answers the procedure from it exactly as the ATT rules
// say - as many entries as fit the default MTU, and "attribute not found" past the last one. Each test
// breaks one thing about the table or the server and nothing else.
#[derive(Clone)]
struct Attribute {
	handle: u16,
	kind: u16,
	// For a service: its end handle. For a declaration: its value handle and the characteristic UUID.
	end: u16,
	value_handle: u16,
	characteristic: u16,
}

fn service(handle: u16, end: u16, kind: u16) -> Attribute {
	Attribute { handle, kind: uuid::PRIMARY_SERVICE, end, value_handle: 0, characteristic: kind }
}

fn declaration(handle: u16, value_handle: u16, characteristic: u16) -> Attribute {
	Attribute { handle, kind: uuid::CHARACTERISTIC, end: 0, value_handle, characteristic }
}

fn other(handle: u16, kind: u16) -> Attribute {
	Attribute { handle, kind, end: 0, value_handle: 0, characteristic: 0 }
}

// A boot mouse as HOGP lays one out: GAP, then HID with protocol mode, the boot report and its
// configuration descriptor, HID information, then battery.
fn mouse() -> alloc::vec::Vec<Attribute> {
	alloc::vec![
		service(0x0001, 0x0005, 0x1800),
		declaration(0x0002, 0x0003, 0x2a00),
		other(0x0003, 0x2a00),
		service(0x0006, 0x0010, uuid::HUMAN_INTERFACE),
		declaration(0x0007, 0x0008, uuid::PROTOCOL_MODE),
		other(0x0008, uuid::PROTOCOL_MODE),
		declaration(0x0009, 0x000a, uuid::BOOT_MOUSE_INPUT),
		other(0x000a, uuid::BOOT_MOUSE_INPUT),
		other(0x000b, uuid::CLIENT_CONFIGURATION),
		declaration(0x000c, 0x000d, 0x2a4a),
		other(0x000d, 0x2a4a),
		service(0x0011, 0x0013, 0x180f),
	]
}

fn not_found(request: u8, handle: u16) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![op::ERROR_RESPONSE, request];
	out.extend_from_slice(&handle.to_le_bytes());
	out.push(ATTRIBUTE_NOT_FOUND);
	out
}

fn range(request: &[u8]) -> (u16, u16) {
	(u16::from_le_bytes([request[1], request[2]]), u16::from_le_bytes([request[3], request[4]]))
}

// Answer one request from the table, the way a correct server does.
fn serve(table: &[Attribute], request: &[u8]) -> Option<alloc::vec::Vec<u8>> {
	let room = DEFAULT_MTU - 2;
	match request[0] {
		op::READ_BY_GROUP_TYPE_REQUEST => {
			let (from, to) = range(request);
			let mut out = alloc::vec![op::READ_BY_GROUP_TYPE_RESPONSE, 6];
			for attribute in table.iter().filter(|a| a.kind == uuid::PRIMARY_SERVICE && a.handle >= from && a.handle <= to).take(room / 6) {
				out.extend_from_slice(&attribute.handle.to_le_bytes());
				out.extend_from_slice(&attribute.end.to_le_bytes());
				out.extend_from_slice(&attribute.characteristic.to_le_bytes());
			}
			Some(if out.len() == 2 { not_found(op::READ_BY_GROUP_TYPE_REQUEST, from) } else { out })
		}
		op::READ_BY_TYPE_REQUEST => {
			let (from, to) = range(request);
			let mut out = alloc::vec![op::READ_BY_TYPE_RESPONSE, 7];
			for attribute in table.iter().filter(|a| a.kind == uuid::CHARACTERISTIC && a.handle >= from && a.handle <= to).take(room / 7) {
				out.extend_from_slice(&attribute.handle.to_le_bytes());
				out.push(0x12);
				out.extend_from_slice(&attribute.value_handle.to_le_bytes());
				out.extend_from_slice(&attribute.characteristic.to_le_bytes());
			}
			Some(if out.len() == 2 { not_found(op::READ_BY_TYPE_REQUEST, from) } else { out })
		}
		op::FIND_INFORMATION_REQUEST => {
			let (from, to) = range(request);
			let mut out = alloc::vec![op::FIND_INFORMATION_RESPONSE, 1];
			for attribute in table.iter().filter(|a| a.handle >= from && a.handle <= to).take(room / 4) {
				out.extend_from_slice(&attribute.handle.to_le_bytes());
				out.extend_from_slice(&attribute.kind.to_le_bytes());
			}
			Some(if out.len() == 2 { not_found(op::FIND_INFORMATION_REQUEST, from) } else { out })
		}
		op::WRITE_REQUEST => Some(alloc::vec![op::WRITE_RESPONSE]),
		op::WRITE_COMMAND => None,
		_ => None,
	}
}

// Run the procedure against a server to its end, recording every write it made.
fn run(table: &[Attribute]) -> (Next, alloc::vec::Vec<alloc::vec::Vec<u8>>) {
	let (mut discovery, mut next) = Discovery::start(DEFAULT_MTU);
	let mut writes = alloc::vec::Vec::new();
	for _ in 0..64 {
		next = match next {
			Next::Request(request) => {
				if request[0] == op::WRITE_REQUEST {
					writes.push(request.clone());
				}
				let answer = serve(table, &request).expect("a request is answered");
				discovery.on_answer(&answer)
			}
			Next::Command(command, then) => {
				writes.push(command);
				*then
			}
			finished => return (finished, writes),
		};
	}
	panic!("the procedure did not finish in sixty-four exchanges - which is the loop it exists to prevent");
}

#[test]
// THE ORDINARY CASE: a boot mouse is found, put in boot mode and has its reports turned on - in that
// order, because a device left in report mode would notify in a layout the boot decoder refuses.
fn a_boot_mouse_is_found_put_in_boot_mode_and_its_reports_turned_on() {
	let (end, writes) = run(&mouse());
	assert_eq!(end, Next::Ready { report: 0x000a });
	assert_eq!(writes, alloc::vec![alloc::vec![op::WRITE_COMMAND, 0x08, 0x00, 0x00], alloc::vec![op::WRITE_REQUEST, 0x0b, 0x00, 0x01, 0x00]], "boot mode on the protocol mode value, then notifications on the report's own descriptor");
}

#[test]
// WHAT IS NOT FOUND IS A TYPED ANSWER, which is the milestone's own rule for a slice that does not claim
// full HOGP conformance.
fn a_peer_that_is_not_a_boot_mouse_is_a_typed_unsupported_answer() {
	let no_hid: alloc::vec::Vec<Attribute> = mouse().into_iter().filter(|a| !(a.kind == uuid::PRIMARY_SERVICE && a.characteristic == uuid::HUMAN_INTERFACE)).collect();
	assert_eq!(run(&no_hid).0, Next::Unsupported(Unsupported::NoHumanInterface));
	let keyboard: alloc::vec::Vec<Attribute> = mouse().into_iter().filter(|a| a.characteristic != uuid::BOOT_MOUSE_INPUT && a.kind != uuid::BOOT_MOUSE_INPUT).collect();
	assert_eq!(run(&keyboard).0, Next::Unsupported(Unsupported::NoBootMouseReport));
	let no_mode: alloc::vec::Vec<Attribute> = mouse().into_iter().filter(|a| a.characteristic != uuid::PROTOCOL_MODE && a.kind != uuid::PROTOCOL_MODE).collect();
	assert_eq!(run(&no_mode).0, Next::Unsupported(Unsupported::NoProtocolMode));
}

#[test]
// THE REPORT'S DESCRIPTORS ARE BETWEEN ITS VALUE AND THE NEXT DECLARATION. A configuration descriptor
// past that boundary belongs to another characteristic, and enabling it would turn on the wrong
// notifications and leave the report's off.
fn a_configuration_descriptor_belonging_to_the_next_characteristic_is_not_taken() {
	let mut table: alloc::vec::Vec<Attribute> = mouse().into_iter().filter(|a| a.handle != 0x000b).collect();
	table.push(other(0x000e, uuid::CLIENT_CONFIGURATION));
	table.sort_by_key(|a| a.handle);
	assert_eq!(run(&table).0, Next::Unsupported(Unsupported::NoConfiguration));
}

#[test]
// A SERVER WHOSE SERVICE LIST DOES NOT ADVANCE WOULD BE ASKED THE SAME QUESTION FOR EVER, and the
// procedure ends with the fault rather than looping - the run helper panics if it takes sixty-four
// exchanges, which is how a hang would show up here.
fn a_service_list_that_does_not_advance_ends_the_procedure_rather_than_repeating_it() {
	let (mut discovery, _) = Discovery::start(DEFAULT_MTU);
	// Two groups where the second starts inside the first.
	let answer = [op::READ_BY_GROUP_TYPE_RESPONSE, 6, 0x01, 0x00, 0x05, 0x00, 0x00, 0x18, 0x03, 0x00, 0x08, 0x00, 0x0f, 0x18];
	assert_eq!(discovery.on_answer(&answer), Next::Failed(Refusal::HandleDidNotAdvance { got: 0x0008, from: 0x0003 }));
	// And a group whose end is before its start.
	let (mut backwards, _) = Discovery::start(DEFAULT_MTU);
	let answer = [op::READ_BY_GROUP_TYPE_RESPONSE, 6, 0x06, 0x00, 0x02, 0x00, 0x12, 0x18];
	assert!(matches!(backwards.on_answer(&answer), Next::Failed(Refusal::HandleDidNotAdvance { .. })));
}

#[test]
// A VALUE HANDLE OUTSIDE ITS SERVICE, OR AT ITS OWN DECLARATION, describes an attribute somewhere this
// discovery did not look - and writing to it would be writing to whatever is actually there.
fn a_characteristic_pointing_outside_its_service_is_refused() {
	let mut table = mouse();
	for attribute in &mut table {
		if attribute.handle == 0x0009 {
			attribute.value_handle = 0x0020;
		}
	}
	assert!(matches!(run(&table).0, Next::Failed(Refusal::HandleOutOfRange { got: 0x0020, .. })));
}

#[test]
// AN ERROR OTHER THAN "NOT FOUND" IS ONE THIS PROCEDURE CANNOT CONTINUE PAST, and it is reported with
// its code rather than read as the end of a list.
fn a_server_error_other_than_not_found_is_reported_with_its_code() {
	let (mut discovery, _) = Discovery::start(DEFAULT_MTU);
	let error = [op::ERROR_RESPONSE, op::READ_BY_GROUP_TYPE_REQUEST, 0x01, 0x00, 0x05];
	assert_eq!(discovery.on_answer(&error), Next::ServerError(0x05));
	assert_eq!(discovery.report(), None, "and nothing was found");
}
