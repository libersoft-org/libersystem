use super::*;

const DONE: u32 = 0x8000_0003;

fn limits() -> Limits {
	Limits::negotiate(MAX_FRAGMENTS, MAX_MESSAGE, MAX_AGGREGATE, MAX_ASSEMBLIES)
}

#[test]
fn a_message_cut_into_fragments_comes_back_whole() {
	let body: Vec<u8> = (0..300u16).map(|n| n as u8).collect();
	let transfers = fragments(DONE, 42, &body, 64);
	assert!(transfers.len() > 1);
	let mut assembler = Assembler::new(limits());
	let mut done = None;
	for transfer in &transfers {
		let fragment = parse_fragment(transfer).unwrap();
		assert_eq!(fragment.total as usize, transfers.len());
		done = assembler.feed(&fragment, 0).unwrap();
	}
	let message = done.expect("the last fragment completes it");
	assert_eq!((message.message_type, message.transaction, message.body), (DONE, 42, body));
	assert_eq!(assembler.open(), 0, "nothing stays allocated");
}

#[test]
fn a_transfer_whose_length_disagrees_is_malformed() {
	let mut transfer = fragments(DONE, 1, &[1, 2, 3], 64).remove(0);
	transfer.push(0);
	assert_eq!(parse_fragment(&transfer), Err(Refused::Malformed));
	assert_eq!(parse_fragment(&transfer[..8]), Err(Refused::Malformed));
	// A fragment numbered at or past its own total.
	let mut past = fragments(DONE, 1, &[1, 2, 3], 64).remove(0);
	past[16..20].copy_from_slice(&1u32.to_le_bytes());
	assert_eq!(parse_fragment(&past), Err(Refused::Order));
}

#[test]
fn duplicates_that_agree_are_ignored_and_ones_that_conflict_end_the_assembly() {
	let body = alloc::vec![7u8; 100];
	let transfers = fragments(DONE, 5, &body, 40);
	let mut assembler = Assembler::new(limits());
	assembler.feed(&parse_fragment(&transfers[0]).unwrap(), 0).unwrap();
	assembler.feed(&parse_fragment(&transfers[1]).unwrap(), 0).unwrap();
	assert_eq!(assembler.feed(&parse_fragment(&transfers[1]).unwrap(), 0), Ok(None), "the same fragment again");
	let mut conflicting = transfers[1].clone();
	*conflicting.last_mut().unwrap() ^= 0xff;
	assert_eq!(assembler.feed(&parse_fragment(&conflicting).unwrap(), 0), Err(Refused::Conflict));
	assert_eq!(assembler.open(), 0);
}

#[test]
fn out_of_order_or_inconsistent_fragments_are_refused() {
	let transfers = fragments(DONE, 9, &[3u8; 100], 40);
	let mut assembler = Assembler::new(limits());
	assert_eq!(assembler.feed(&parse_fragment(&transfers[1]).unwrap(), 0), Err(Refused::Order), "no first fragment");
	assembler.feed(&parse_fragment(&transfers[0]).unwrap(), 0).unwrap();
	assert_eq!(assembler.feed(&parse_fragment(&transfers[2]).unwrap(), 0), Err(Refused::Order), "a skipped fragment");
	assembler.feed(&parse_fragment(&transfers[0]).unwrap(), 0).unwrap();
	let mut other_total = transfers[1].clone();
	other_total[12..16].copy_from_slice(&9u32.to_le_bytes());
	assert_eq!(assembler.feed(&parse_fragment(&other_total).unwrap(), 0), Err(Refused::Order), "a total that changed");
}

#[test]
fn every_bound_is_negotiated_down_and_enforced() {
	let negotiated = Limits::negotiate(1000, 1 << 20, 1 << 30, 99);
	assert_eq!((negotiated.fragments, negotiated.message, negotiated.aggregate, negotiated.assemblies), (MAX_FRAGMENTS, MAX_MESSAGE, MAX_AGGREGATE, MAX_ASSEMBLIES));
	// Fragments past the limit.
	let mut small = Assembler::new(Limits::negotiate(2, MAX_MESSAGE, MAX_AGGREGATE, MAX_ASSEMBLIES));
	let three = fragments(DONE, 1, &[0u8; 100], 60);
	assert_eq!(three.len(), 3);
	assert_eq!(small.feed(&parse_fragment(&three[0]).unwrap(), 0), Err(Refused::Bound));
	// A message past its byte limit.
	let mut short = Assembler::new(Limits::negotiate(MAX_FRAGMENTS, 64, MAX_AGGREGATE, MAX_ASSEMBLIES));
	let long = fragments(DONE, 2, &[0u8; 100], 60);
	short.feed(&parse_fragment(&long[0]).unwrap(), 0).unwrap();
	assert_eq!(short.feed(&parse_fragment(&long[1]).unwrap(), 0), Err(Refused::Bound));
	// Four assemblies at once, and the fifth refused.
	let mut many = Assembler::new(limits());
	for transaction in 0..MAX_ASSEMBLIES as u32 {
		many.feed(&parse_fragment(&fragments(DONE, transaction, &[0u8; 100], 60)[0]).unwrap(), 0).unwrap();
	}
	assert_eq!(many.feed(&parse_fragment(&fragments(DONE, 99, &[0u8; 100], 60)[0]).unwrap(), 0), Err(Refused::Bound));
	// The aggregate across assemblies: forty bytes each of the first two fragments against sixty.
	let mut aggregate = Assembler::new(Limits::negotiate(MAX_FRAGMENTS, MAX_MESSAGE, 60, MAX_ASSEMBLIES));
	aggregate.feed(&parse_fragment(&fragments(DONE, 1, &[0u8; 200], 60)[0]).unwrap(), 0).unwrap();
	assert_eq!(aggregate.feed(&parse_fragment(&fragments(DONE, 2, &[0u8; 200], 60)[0]).unwrap(), 0), Err(Refused::Bound));
}

#[test]
fn an_incomplete_assembly_expires_after_two_seconds() {
	let mut assembler = Assembler::new(limits());
	assembler.feed(&parse_fragment(&fragments(DONE, 1, &[0u8; 100], 60)[0]).unwrap(), 10).unwrap();
	assert_eq!(assembler.expire(10 + ASSEMBLY_TICKS - 1), 0);
	assert_eq!(assembler.expire(10 + ASSEMBLY_TICKS), 1);
	assert_eq!(assembler.open(), 0);
}

#[test]
fn a_transfer_block_yields_its_sessions_datagrams() {
	let first = [0x45u8; 40];
	let second = [0x46u8; 28];
	let built = block(0, &[&first, &second], 7);
	let ranges = datagrams(&built, 0, 1500).unwrap();
	assert_eq!(ranges.len(), 2);
	assert_eq!(&built[ranges[0].clone()], &first);
	assert_eq!(&built[ranges[1].clone()], &second);
	assert_eq!(datagrams(&built, 1, 1500), Err(Refused::Session), "another session's datagrams are not this context's");
	assert_eq!(datagrams(&built, 0, 30), Err(Refused::Bound), "a datagram past the MTU");
}

#[test]
fn every_offset_in_a_block_is_checked_and_a_cycle_is_refused() {
	let built = block(0, &[&[0x45u8; 40]], 1);
	// A block length past what arrived.
	let mut long = built.clone();
	long[8..10].copy_from_slice(&((built.len() + 4) as u16).to_le_bytes());
	assert_eq!(datagrams(&long, 0, 1500), Err(Refused::Offset));
	// An NDP index outside the block, and a misaligned one.
	let mut outside = built.clone();
	outside[10..12].copy_from_slice(&((built.len() + 8) as u16).to_le_bytes());
	assert_eq!(datagrams(&outside, 0, 1500), Err(Refused::Offset));
	let mut misaligned = built.clone();
	let ndp = u16::from_le_bytes([built[10], built[11]]);
	misaligned[10..12].copy_from_slice(&(ndp + 2).to_le_bytes());
	assert_eq!(datagrams(&misaligned, 0, 1500), Err(Refused::Offset));
	// A datagram pointer reaching past the block.
	let mut pointer = built.clone();
	let at = usize::from(ndp) + 8;
	pointer[at + 2..at + 4].copy_from_slice(&0x0fffu16.to_le_bytes());
	assert_eq!(datagrams(&pointer, 0, 4096), Err(Refused::Offset));
	// An NDP whose next index is itself: a cycle.
	let mut cycle = built.clone();
	let next = usize::from(ndp) + 6;
	cycle[next..next + 2].copy_from_slice(&ndp.to_le_bytes());
	assert_eq!(datagrams(&cycle, 0, 1500), Err(Refused::Offset));
	// Not an NTB at all.
	assert_eq!(datagrams(&[0u8; 12], 0, 1500), Err(Refused::Malformed));
}
