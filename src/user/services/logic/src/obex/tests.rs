use super::*;
use alloc::vec;

// Every packet a client sends, answered by a server, until neither has more to say: what the server heard.
fn run(client: &mut PushClient, server: &mut PushServer, first: Vec<u8>, object: &[u8]) -> (Vec<ServerEvent>, Option<Result<u64, Failure>>, usize) {
	let mut events = Vec::new();
	let mut done = None;
	let mut packets = 0;
	let mut outbox = vec![first];
	let mut offset = 0;
	for _ in 0..10_000 {
		let Some(packet) = outbox.pop() else {
			// NOTHING OUTSTANDING: the caller writes what the client takes, and ends it.
			if offset < object.len() {
				let (taken, outs) = client.write(&object[offset..]);
				offset += taken;
				for out in outs {
					if let ClientOut::Send(bytes) = out {
						outbox.push(bytes);
					}
				}
				continue;
			}
			let outs = client.finish();
			if outs.is_empty() {
				break;
			}
			for out in outs {
				if let ClientOut::Send(bytes) = out {
					outbox.push(bytes);
				}
			}
			continue;
		};
		packets += 1;
		assert!(packet.len() <= MAX_PACKET);
		let (reply, heard) = server.receive(&packet);
		events.extend(heard);
		for out in client.receive(&reply) {
			match out {
				ClientOut::Send(bytes) => outbox.push(bytes),
				ClientOut::Done(result) => done = Some(result),
			}
		}
		if done.is_some() {
			break;
		}
	}
	(events, done, packets)
}

#[test]
// PACKETS AND HEADERS round-trip: text in UTF-16 with its terminator, bytes, a byte and a word; a length that
// disagrees with the bytes, and a header running past the packet, refused.
fn packets_and_headers_round_trip() {
	let packet = Packet::new(
		opcode::PUT,
		vec![
			Header::Text(header::NAME, String::from("zpráva.txt")),
			Header::Bytes(header::TYPE, b"text/plain\0".to_vec()),
			Header::Word(header::LENGTH, 12),
			Header::Byte(0x97, 1),
		],
	);
	let bytes = packet.encode();
	assert_eq!(&bytes[..3], &[opcode::PUT, 0, bytes.len() as u8]);
	assert_eq!(&bytes[3..6], &[header::NAME, 0, 3 + 11 * 2], "ten characters and the terminator, two bytes each");
	assert_eq!(Packet::decode(&bytes, false), Ok(packet));
	let connect = Packet { code: opcode::CONNECT, connect: Some((VERSION, 0, 0x1000)), headers: Vec::new() };
	assert_eq!(connect.encode(), [0x80, 0x00, 0x07, 0x10, 0x00, 0x10, 0x00], "the specification's CONNECT");
	assert_eq!(Packet::decode(&connect.encode(), true), Ok(connect));
	let mut short = bytes.clone();
	short.pop();
	assert_eq!(Packet::decode(&short, false), Err(Refusal::Length));
	assert_eq!(Packet::decode(&[0x02, 0x00, 0x06, 0x48, 0x00, 0x09], false), Err(Refusal::Header));
}

#[test]
// THE ASSEMBLER puts a packet split across frames back together, and refuses one past its bound.
fn the_assembler_rebuilds_packets_and_bounds_them() {
	let packet = Packet::new(opcode::PUT, vec![Header::Bytes(header::BODY, vec![7; 300])]).encode();
	let mut assembler = Assembler::new(MAX_PACKET);
	let mut whole = Vec::new();
	for piece in packet.chunks(127) {
		whole.extend(assembler.push(piece).unwrap());
	}
	whole.extend(assembler.push(&packet[..10]).unwrap());
	assert_eq!(whole, vec![packet.clone()]);
	let mut small = Assembler::new(255);
	assert_eq!(small.push(&packet), Err(Refusal::Length));
}

#[test]
// ONE OBJECT PUSHED AND RECEIVED: CONNECT, the PUTs within the packet size both settled, the body in order, the
// final PUT, DISCONNECT - and the server heard the name, the type and the length the client gave.
fn an_object_is_pushed_and_received_whole() {
	let object: Vec<u8> = (0..10_000u32).map(|n| (n % 251) as u8).collect();
	let (mut client, first) = PushClient::new("photo.jpg", Some("image/jpeg"), Some(object.len() as u32));
	let mut server = PushServer::new(1 << 20);
	let (events, done, packets) = run(&mut client, &mut server, first, &object);
	assert_eq!(done, Some(Ok(object.len() as u64)));
	assert!(packets >= 4, "connect, puts, disconnect");
	assert_eq!(events[0], ServerEvent::Offered { name: String::from("photo.jpg"), kind: Some(String::from("image/jpeg")), length: Some(10_000) });
	let body: Vec<u8> = events.iter().filter_map(|event| if let ServerEvent::Data(bytes) = event { Some(bytes.clone()) } else { None }).flatten().collect();
	assert_eq!(body, object);
	assert_eq!(events.last(), Some(&ServerEvent::Complete));
	assert!(server.over() && client.done());
	// AN EMPTY OBJECT is a final PUT with its headers and an empty end of body.
	let (mut client, first) = PushClient::new("empty", None, Some(0));
	let mut server = PushServer::new(10);
	let (events, done, _) = run(&mut client, &mut server, first, &[]);
	assert_eq!(done, Some(Ok(0)));
	assert_eq!(events, vec![ServerEvent::Offered { name: String::from("empty"), kind: None, length: Some(0) }, ServerEvent::Complete]);
}

#[test]
// THE BOUND: an object declaring more than the receiver takes is refused before a byte; one that passes it while
// arriving is refused at that packet; and the server takes no second object.
fn the_receivers_bound_holds() {
	let object = vec![1u8; 5_000];
	let (mut client, first) = PushClient::new("big", None, Some(5_000));
	let mut server = PushServer::new(4_000);
	let (events, done, _) = run(&mut client, &mut server, first, &object);
	assert_eq!(done, Some(Err(Failure::Refused(response::TOO_LARGE))));
	assert_eq!(events, vec![ServerEvent::Failed(response::TOO_LARGE)]);
	// UNDECLARED, it is refused once it passes the bound.
	let (mut client, first) = PushClient::new("big", None, None);
	let mut server = PushServer::new(4_000);
	let (events, done, _) = run(&mut client, &mut server, first, &object);
	assert_eq!(done, Some(Err(Failure::Refused(response::TOO_LARGE))));
	assert!(matches!(events.first(), Some(ServerEvent::Offered { length: None, .. })));
	assert_eq!(events.last(), Some(&ServerEvent::Failed(response::TOO_LARGE)));
	let taken: usize = events.iter().map(|event| if let ServerEvent::Data(bytes) = event { bytes.len() } else { 0 }).sum();
	assert!(taken <= 4_000, "nothing past the bound was handed on");
	// ONE OBJECT A SERVER: a second push is unavailable.
	let (_, first) = PushClient::new("again", None, Some(1));
	let (reply, _) = server.receive(&first);
	assert_eq!(reply[0], response::SUCCESS);
	let put = Packet::new(opcode::PUT_FINAL, vec![Header::Text(header::NAME, String::from("again")), Header::Bytes(header::END_OF_BODY, vec![1])]).encode();
	assert_eq!(server.receive(&put).0[0], response::UNAVAILABLE);
}

#[test]
// A PUSH WITH NO NAME is refused; the pusher's ABORT and a lost transport are failures said to the receiver.
fn nameless_aborted_and_lost_objects_fail() {
	let mut server = PushServer::new(100);
	let (_, connect) = PushClient::new("x", None, None);
	server.receive(&connect);
	let nameless = Packet::new(opcode::PUT_FINAL, vec![Header::Bytes(header::END_OF_BODY, vec![1])]).encode();
	let (reply, events) = server.receive(&nameless);
	assert_eq!(reply[0], response::BAD_REQUEST);
	assert_eq!(events, vec![ServerEvent::Failed(response::BAD_REQUEST)]);

	let mut server = PushServer::new(100);
	server.receive(&connect);
	let put = Packet::new(opcode::PUT, vec![Header::Text(header::NAME, String::from("a")), Header::Bytes(header::BODY, vec![1, 2])]).encode();
	server.receive(&put);
	let (reply, events) = server.receive(&Packet::new(opcode::ABORT, Vec::new()).encode());
	assert_eq!(reply[0], response::SUCCESS);
	assert_eq!(events, vec![ServerEvent::Failed(opcode::ABORT)]);

	let mut server = PushServer::new(100);
	server.receive(&connect);
	server.receive(&put);
	assert_eq!(server.lost(), vec![ServerEvent::Failed(response::UNAVAILABLE)]);
	assert_eq!(server.received(), 2);

	// THE CLIENT'S OWN ABORT, mid-push.
	let (mut client, _) = PushClient::new("a", None, None);
	client.receive(&Packet { code: response::SUCCESS, connect: Some((VERSION, 0, 255)), headers: Vec::new() }.encode());
	let outs = client.abort();
	assert!(matches!(outs.last(), Some(ClientOut::Done(Err(Failure::Aborted)))));
	assert!(client.done());
}

#[test]
// THE CLIENT'S BUFFER is bounded: `room` says what it takes, and a refused CONNECT ends the push without a
// DISCONNECT.
fn the_clients_buffer_is_bounded_and_a_refused_connect_ends_it() {
	let (mut client, _) = PushClient::new("a", None, None);
	assert_eq!(client.room(), CLIENT_BUFFER);
	let (taken, outs) = client.write(&vec![0; CLIENT_BUFFER + 10]);
	assert_eq!(taken, CLIENT_BUFFER);
	assert!(outs.is_empty(), "nothing goes before the CONNECT is answered");
	assert_eq!(client.room(), 0);
	let outs = client.receive(&Packet::new(response::FORBIDDEN, Vec::new()).encode());
	assert_eq!(outs, vec![ClientOut::Done(Err(Failure::Refused(response::FORBIDDEN)))]);
}
