//! What counts as an answer, and the forgeries a predictable resolver cannot reject.

use super::*;

fn question(name: &str, qtype: u16) -> Question {
	Question { name: normalize(name.as_bytes()), qtype, qclass: CLASS_IN }
}

/// A response builder, so the fixtures say what they are about rather than what byte is where.
struct Response {
	message: Vec<u8>,
	answers: u16,
}

impl Response {
	fn new(transaction: u16, question: &Question, flags: u16) -> Response {
		let mut message: Vec<u8> = Vec::new();
		message.extend_from_slice(&transaction.to_be_bytes());
		message.extend_from_slice(&flags.to_be_bytes());
		message.extend_from_slice(&1u16.to_be_bytes());
		message.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
		encode_name(&question.name, &mut message);
		message.extend_from_slice(&question.qtype.to_be_bytes());
		message.extend_from_slice(&question.qclass.to_be_bytes());
		Response { message, answers: 0 }
	}

	fn record(mut self, owner: &[u8], rtype: u16, ttl: u32, rdata: &[u8]) -> Response {
		encode_name(owner, &mut self.message);
		self.message.extend_from_slice(&rtype.to_be_bytes());
		self.message.extend_from_slice(&CLASS_IN.to_be_bytes());
		self.message.extend_from_slice(&ttl.to_be_bytes());
		self.message.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
		self.message.extend_from_slice(rdata);
		self.answers += 1;
		self
	}

	fn cname(mut self, owner: &[u8], target: &[u8], ttl: u32) -> Response {
		let mut rdata: Vec<u8> = Vec::new();
		encode_name(target, &mut rdata);
		self = self.record(owner, TYPE_CNAME, ttl, &rdata);
		self
	}

	fn finish(mut self) -> Vec<u8> {
		self.message[6..8].copy_from_slice(&self.answers.to_be_bytes());
		self.message
	}

	/// Claim more answers than the message carries, for the count checks.
	fn claim(mut self, answers: u16) -> Vec<u8> {
		self.message[6..8].copy_from_slice(&answers.to_be_bytes());
		self.message
	}
}

const TUPLE: Tuple = Tuple { transaction: 0x4d2f, source_port: 51_000 };

#[test]
fn a_name_is_compared_in_one_normalized_form() {
	// A SERVER MAY ECHO THE QUESTION IN ANY CASE IT LIKES, and several deliberately randomize it - so
	// a comparison against the bytes as sent rejects correct answers.
	assert_eq!(normalize(b"EXAMPLE.Com."), b"example.com".to_vec());
	assert_eq!(normalize(b"example.com"), b"example.com".to_vec());
	assert_eq!(normalize(b"example.com."), normalize(b"EXAMPLE.COM"));
}

#[test]
fn an_ordinary_answer_is_read_with_its_addresses_and_the_smallest_lifetime() {
	let q = question("example.com", TYPE_A);
	let message = Response::new(TUPLE.transaction, &q, 0x8180).record(&q.name, TYPE_A, 300, &[93, 184, 216, 34]).record(&q.name, TYPE_A, 120, &[93, 184, 216, 35]).finish();
	let answer = parse_response(&message, TUPLE, &q).expect("an answer");
	assert_eq!(answer.addresses, alloc::vec![Address::V4([93, 184, 216, 34]), Address::V4([93, 184, 216, 35])]);
	assert_eq!(answer.ttl, 120, "the smallest of the records used, because that is when the set stops being true");
}

#[test]
fn both_families_come_back_from_one_answer() {
	let q = question("example.com", TYPE_AAAA);
	let mut octets = [0u8; 16];
	octets[0] = 0x26;
	octets[15] = 1;
	let message = Response::new(TUPLE.transaction, &q, 0x8180).record(&q.name, TYPE_AAAA, 60, &octets).finish();
	let answer = parse_response(&message, TUPLE, &q).expect("an answer");
	assert_eq!(answer.addresses, alloc::vec![Address::V6(octets)]);
}

#[test]
fn a_forgery_carrying_the_next_sequential_identity_is_refused() {
	// THE DISCRIMINATING NEGATIVE. A resolver that increments its transaction ID by one and sends
	// from a fixed port cannot reject this: every field it compares matches.
	let q = question("example.com", TYPE_A);
	let forged = Response::new(TUPLE.transaction.wrapping_add(1), &q, 0x8180).record(&q.name, TYPE_A, 300, &[10, 0, 0, 1]).finish();
	assert_eq!(parse_response(&forged, TUPLE, &q), Err(Refusal::NotOurs));
}

#[test]
fn a_cross_query_answer_carries_somebody_elses_question() {
	let asked = question("example.com", TYPE_A);
	let other = question("example.net", TYPE_A);
	let message = Response::new(TUPLE.transaction, &other, 0x8180).record(&other.name, TYPE_A, 300, &[10, 0, 0, 1]).finish();
	assert_eq!(parse_response(&message, TUPLE, &asked), Err(Refusal::NotOurs));

	// The same name asked as a different type is also a different question.
	let as_aaaa = question("example.com", TYPE_AAAA);
	let message = Response::new(TUPLE.transaction, &as_aaaa, 0x8180).finish();
	assert_eq!(parse_response(&message, TUPLE, &asked), Err(Refusal::NotOurs));
}

#[test]
fn a_query_rather_than_a_response_and_a_wrong_opcode_are_both_refused() {
	let q = question("example.com", TYPE_A);
	let as_query = Response::new(TUPLE.transaction, &q, 0x0100).record(&q.name, TYPE_A, 300, &[10, 0, 0, 1]).finish();
	assert_eq!(parse_response(&as_query, TUPLE, &q), Err(Refusal::NotOurs), "QR says it is a question");
	let wrong_opcode = Response::new(TUPLE.transaction, &q, 0x8800).record(&q.name, TYPE_A, 300, &[10, 0, 0, 1]).finish();
	assert_eq!(parse_response(&wrong_opcode, TUPLE, &q), Err(Refusal::NotOurs));
}

#[test]
fn the_server_s_own_answers_are_kept_apart_from_each_other() {
	// COLLAPSING THESE WOULD LOSE THE DIFFERENCE A CALLER ACTS ON: a name that does not exist is not
	// a server that is broken, and neither is a timeout.
	let q = question("nowhere.example", TYPE_A);
	assert_eq!(parse_response(&Response::new(TUPLE.transaction, &q, 0x8183).finish(), TUPLE, &q), Err(Refusal::NameError));
	assert_eq!(parse_response(&Response::new(TUPLE.transaction, &q, 0x8182).finish(), TUPLE, &q), Err(Refusal::ServerFailure));
	// A well-formed answer with no address record is neither: the name exists and has no address.
	let empty = Response::new(TUPLE.transaction, &q, 0x8180).record(&q.name, TYPE_NS, 300, &[0]).finish();
	assert_eq!(parse_response(&empty, TUPLE, &q), Err(Refusal::NoAddress));
}

#[test]
fn truncation_is_its_own_outcome_because_the_answer_is_asked_again_over_tcp() {
	// AAAA AND CNAME SETS ARE EXACTLY THE ANSWERS THAT OVERFLOW, so this is an ordinary outcome
	// rather than a failure - and a resolver that read it as malformed would never retry.
	let q = question("example.com", TYPE_AAAA);
	let message = Response::new(TUPLE.transaction, &q, 0x8380).finish();
	assert_eq!(parse_response(&message, TUPLE, &q), Err(Refusal::Truncated));
}

#[test]
fn a_cname_chain_is_followed_to_the_address_at_the_end_of_it() {
	let q = question("www.example.com", TYPE_A);
	let message = Response::new(TUPLE.transaction, &q, 0x8180).cname(&q.name, b"cdn.example.net", 60).record(b"cdn.example.net", TYPE_A, 30, &[203, 0, 113, 5]).finish();
	let answer = parse_response(&message, TUPLE, &q).expect("an answer");
	assert_eq!(answer.addresses, alloc::vec![Address::V4([203, 0, 113, 5])]);
	assert_eq!(answer.ttl, 30, "the smallest across the chain, not just the address record");
}

#[test]
fn a_chain_that_revisits_a_name_is_a_loop_and_one_that_is_merely_long_is_refused_too() {
	// A COUNT ALONE WOULD FOLLOW A LOOP until it ran out rather than seeing what it is.
	let q = question("a.example", TYPE_A);
	let message = Response::new(TUPLE.transaction, &q, 0x8180).cname(&q.name, b"b.example", 60).cname(b"b.example", b"a.example", 60).finish();
	assert_eq!(parse_response(&message, TUPLE, &q), Err(Refusal::ChainTooLong));

	// And a chain longer than a delegation can plausibly be.
	let mut long = Response::new(TUPLE.transaction, &q, 0x8180).cname(&q.name, b"h1.example", 60);
	for hop in 1..MAX_CNAME_CHAIN + 2 {
		let from = alloc::format!("h{hop}.example");
		let to = alloc::format!("h{}.example", hop + 1);
		long = long.cname(from.as_bytes(), to.as_bytes(), 60);
	}
	assert_eq!(parse_response(&long.finish(), TUPLE, &q), Err(Refusal::ChainTooLong));
}

#[test]
fn a_compression_pointer_must_target_a_strictly_earlier_offset() {
	// THE BACKWARD RULE IS WHAT MAKES A LOOP IMPOSSIBLE. A pointer to itself or forward can be chased
	// for ever, and a counter alone only bounds how long that takes.
	let mut message: Vec<u8> = alloc::vec![0u8; HEADER_LEN];
	message[0..2].copy_from_slice(&TUPLE.transaction.to_be_bytes());
	let at: usize = message.len();
	// A pointer at offset 12 that points at offset 12.
	message.extend_from_slice(&[0xc0, at as u8]);
	assert_eq!(decode_name(&message, at), Err(Refusal::BadCompression));

	// And one that points forward.
	let mut forward: Vec<u8> = alloc::vec![0u8; HEADER_LEN];
	forward.extend_from_slice(&[0xc0, 20]);
	assert_eq!(decode_name(&forward, HEADER_LEN), Err(Refusal::BadCompression));

	// A legal backward pointer is followed, and the offset it returns is past the POINTER rather than
	// past what it pointed at - which is what keeps the record walk in step.
	let mut legal: Vec<u8> = alloc::vec![0u8; HEADER_LEN];
	encode_name(b"example.com", &mut legal);
	let pointer_at: usize = legal.len();
	legal.extend_from_slice(&[0xc0, HEADER_LEN as u8]);
	let (name, past) = decode_name(&legal, pointer_at).expect("a legal pointer");
	assert_eq!(name, b"example.com".to_vec());
	assert_eq!(past, pointer_at + 2);
}

#[test]
fn a_response_declaring_more_records_than_this_host_parses_is_refused() {
	// AND THE QUERY IS NOT RETRIED AGAINST THE SAME SERVER, which is what stops a hostile one costing
	// an unbounded number of parses.
	let q = question("example.com", TYPE_A);
	let message = Response::new(TUPLE.transaction, &q, 0x8180).claim((MAX_ANSWER_RECORDS + 1) as u16);
	assert_eq!(parse_response(&message, TUPLE, &q), Err(Refusal::TooManyRecords));

	// A count the message does not actually carry is malformed rather than accepted short.
	let short = Response::new(TUPLE.transaction, &q, 0x8180).claim(2);
	assert_eq!(parse_response(&short, TUPLE, &q), Err(Refusal::Malformed));
}

#[test]
fn more_addresses_than_the_bound_are_dropped_rather_than_refusing_the_name() {
	// A NAME WITH NINE ADDRESSES IS WELL PROVISIONED, not wrong, and the ones kept are the ones that
	// would have been tried first.
	let q = question("many.example", TYPE_A);
	let mut message = Response::new(TUPLE.transaction, &q, 0x8180);
	for index in 0..(MAX_RETURNED_ADDRESSES + 4) as u8 {
		message = message.record(&q.name, TYPE_A, 300, &[10, 0, 0, index]);
	}
	let answer = parse_response(&message.finish(), TUPLE, &q).expect("an answer");
	assert_eq!(answer.addresses.len(), MAX_RETURNED_ADDRESSES);
	assert_eq!(answer.addresses[0], Address::V4([10, 0, 0, 0]), "the first ones, in the order the server gave");
}

#[test]
fn a_query_round_trips_through_its_own_encoding() {
	let q = question("example.com", TYPE_A);
	let message = build_query(TUPLE, &q).expect("it encodes");
	assert_eq!(u16::from_be_bytes([message[0], message[1]]), TUPLE.transaction);
	assert_eq!(u16::from_be_bytes([message[2], message[3]]), 0x0100, "recursion desired, and nothing else");
	let (name, past) = decode_name(&message, HEADER_LEN).expect("the question");
	assert_eq!(name, q.name);
	assert_eq!(u16::from_be_bytes([message[past], message[past + 1]]), TYPE_A);
	// A name longer than the protocol permits is refused rather than truncated onto the wire.
	let long: alloc::string::String = core::iter::repeat("a.").take(200).collect();
	assert_eq!(build_query(TUPLE, &question(&long, TYPE_A)), None);
}

#[test]
fn an_identity_is_unique_while_it_is_live_and_matches_nothing_once_retired() {
	let mut flight = InFlight::new();
	// A fixed source of "randomness" that repeats, so the redraw is the thing under test.
	let mut sequence: Vec<(u16, u16)> = alloc::vec![(7, 7), (7, 7), (9, 9)];
	let first = flight.draw(|| sequence.remove(0), 8).expect("an identity");
	// TWO LIVE QUERIES SHARING AN IDENTITY WOULD EACH ACCEPT THE OTHER'S ANSWER, which is the same
	// failure as having no correlation - arrived at by accident instead of by an attacker.
	let second = flight.draw(|| sequence.remove(0), 8).expect("a different identity");
	assert_ne!(first, second);
	assert_eq!(flight.len(), 2);

	// RETIRED, SO A LATE ANSWER MATCHES NOTHING.
	assert!(flight.retire(first));
	assert!(!flight.holds(first));
	assert!(!flight.retire(first), "and retiring it twice releases nothing twice");
	assert_eq!(flight.len(), 1);
}

#[test]
fn a_drawn_source_port_is_an_ephemeral_one() {
	let mut flight = InFlight::new();
	for raw in [0u16, 1, 12_345, 65_535] {
		let mut once: Vec<(u16, u16)> = alloc::vec![(raw, raw)];
		let tuple = flight.draw(|| once.remove(0), 4).expect("an identity");
		assert!(tuple.source_port >= 49_152, "port {} is not ephemeral", tuple.source_port);
	}
}

#[test]
fn a_link_local_answer_cannot_escape_without_its_link() {
	// `fe80::1` NAMES A DIFFERENT HOST ON EACH LINK, and nothing in a DNS answer says which link it
	// meant - so the resolver cannot supply the provenance the scoped form requires, and handing it
	// out as though it were routable would be handing out an address that means nothing.
	let q = question("local.example", TYPE_AAAA);
	let mut link_local = [0u8; 16];
	link_local[0] = 0xfe;
	link_local[1] = 0x80;
	link_local[15] = 1;
	let only = Response::new(TUPLE.transaction, &q, 0x8180).record(&q.name, TYPE_AAAA, 60, &link_local).finish();
	assert_eq!(parse_response(&only, TUPLE, &q), Err(Refusal::NoAddress));

	// And it is dropped from a mixed answer rather than refusing the whole name.
	let mut global = [0u8; 16];
	global[0] = 0x26;
	global[15] = 1;
	let mixed = Response::new(TUPLE.transaction, &q, 0x8180).record(&q.name, TYPE_AAAA, 60, &link_local).record(&q.name, TYPE_AAAA, 60, &global).finish();
	let answer = parse_response(&mixed, TUPLE, &q).expect("the routable one survives");
	assert_eq!(answer.addresses, alloc::vec![Address::V6(global)]);
}
