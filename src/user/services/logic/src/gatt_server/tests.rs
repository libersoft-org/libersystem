use super::*;

#[test]
fn the_services_are_found_by_group() {
	let mut server = Server::new(b"LiberSystem");
	let answer = server.answer(&[op::READ_BY_GROUP_TYPE_REQUEST, 0x01, 0x00, 0xff, 0xff, 0x00, 0x28]).unwrap();
	assert_eq!(answer, [op::READ_BY_GROUP_TYPE_RESPONSE, 6, 0x01, 0x00, 0x05, 0x00, 0x00, 0x18, 0x06, 0x00, 0x09, 0x00, 0x01, 0x18]);
	let past = server.answer(&[op::READ_BY_GROUP_TYPE_REQUEST, 0x0a, 0x00, 0xff, 0xff, 0x00, 0x28]).unwrap();
	assert_eq!(past, [op::ERROR_RESPONSE, op::READ_BY_GROUP_TYPE_REQUEST, 0x0a, 0x00, error::ATTRIBUTE_NOT_FOUND]);
}

#[test]
fn the_name_is_read_by_type_and_by_handle() {
	let mut server = Server::new(b"LiberSystem");
	let declarations = server.answer(&[op::READ_BY_TYPE_REQUEST, 0x01, 0x00, 0x05, 0x00, 0x03, 0x28]).unwrap();
	assert_eq!(declarations, [op::READ_BY_TYPE_RESPONSE, 7, 0x02, 0x00, PROPERTY_READ, 0x03, 0x00, 0x00, 0x2a, 0x04, 0x00, PROPERTY_READ, 0x05, 0x00, 0x01, 0x2a]);
	let name = server.answer(&[op::READ_REQUEST, 0x03, 0x00]).unwrap();
	assert_eq!(&name[1..], b"LiberSystem");
	let by_type = server.answer(&[op::READ_BY_TYPE_REQUEST, 0x01, 0x00, 0xff, 0xff, 0x00, 0x2a]).unwrap();
	assert_eq!(by_type[1] as usize, 2 + b"LiberSystem".len());
}

#[test]
fn only_a_configuration_descriptor_is_written() {
	let mut server = Server::new(b"x");
	assert_eq!(server.answer(&[op::WRITE_REQUEST, 0x03, 0x00, b'y']).unwrap(), [op::ERROR_RESPONSE, op::WRITE_REQUEST, 0x03, 0x00, error::WRITE_NOT_PERMITTED]);
	assert_eq!(server.answer(&[op::WRITE_REQUEST, 0x09, 0x00, 0x02, 0x00]).unwrap(), [op::WRITE_RESPONSE]);
	assert_eq!(server.value(0x09), Some(&[0x02, 0x00][..]));
	assert_eq!(server.answer(&[op::READ_REQUEST, 0x08, 0x00]).unwrap(), [op::ERROR_RESPONSE, op::READ_REQUEST, 0x08, 0x00, error::READ_NOT_PERMITTED]);
	assert_eq!(server.answer(&[0x20, 0x00]).unwrap()[4], error::REQUEST_NOT_SUPPORTED);
}

#[test]
fn a_service_added_follows_the_table() {
	let mut server = Server::new(b"x");
	let start = server.add_service(0x1850, &[(0x2bc9, 0x12, alloc::vec![1, 2], false)]);
	assert_eq!(start, 0x000a);
	let groups = server.answer(&[op::READ_BY_GROUP_TYPE_REQUEST, 0x0a, 0x00, 0xff, 0xff, 0x00, 0x28]).unwrap();
	assert_eq!(groups, [op::READ_BY_GROUP_TYPE_RESPONSE, 6, 0x0a, 0x00, 0x0d, 0x00, 0x50, 0x18]);
	assert_eq!(server.value(0x0c), Some(&[1, 2][..]));
}

#[test]
// A CONTROL POINT: a client's write handed to the stack, and the stack's value notified only where the client asked.
fn control_point_writes_reach_the_stack_and_values_are_notified() {
	let mut server = Server::new(b"host");
	let start = server.add_service(0x184c, &[(0x2bbd, 0x12, alloc::vec![], false), (0x2bbe, 0x18, alloc::vec![], true)]);
	let state = server.handle_of(0x2bbd).unwrap();
	let point = server.handle_of(0x2bbe).unwrap();
	assert_eq!((start, state, point), (10, 12, 15));
	server.set_value(state, alloc::vec![1, 0, 4]);
	assert_eq!(server.notification(state), None, "not before the client asks");
	assert_eq!(server.answer(&[op::WRITE_REQUEST, 13, 0, 1, 0]), Some(alloc::vec![op::WRITE_RESPONSE]));
	assert_eq!(server.notification(state), Some(alloc::vec![op::HANDLE_VALUE_NOTIFICATION, 12, 0, 1, 0, 4]));
	assert!(server.take_writes().is_empty(), "a configuration write is not a control point's");
	assert_eq!(server.answer(&[op::WRITE_REQUEST, 15, 0, 0x00, 1]), Some(alloc::vec![op::WRITE_RESPONSE]));
	assert_eq!(server.take_writes(), alloc::vec![(15, alloc::vec![0x00, 1])]);
	assert_eq!(server.answer(&[op::WRITE_REQUEST, 12, 0, 9]), Some(alloc::vec![op::ERROR_RESPONSE, op::WRITE_REQUEST, 12, 0, error::WRITE_NOT_PERMITTED]));
}
