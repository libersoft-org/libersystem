use super::*;
use alloc::vec;

const GRANTS: [Grant; 3] = [Grant::Tpm, Grant::Measure, Grant::Seal];

fn every_op() -> Vec<Op> {
	vec![
		Op::Info,
		Op::Random { count: 16 },
		Op::PcrRead { pcr: 16 },
		Op::PcrExtend { pcr: 16, digest: vec![0; 32] },
		Op::Seal { pcr: 16, secret: vec![1; 8] },
		Op::Unseal { sealed: vec![1; 200] },
		Op::Quote { pcr: 16, nonce: vec![2; 16] },
	]
}

#[test]
fn every_grant_carries_exactly_its_operations() {
	for grant in GRANTS {
		for op in every_op() {
			let expected = match op {
				Op::Info => true,
				Op::Random { .. } | Op::PcrRead { .. } | Op::Quote { .. } => grant == Grant::Tpm,
				Op::PcrExtend { .. } => grant == Grant::Measure,
				Op::Seal { .. } | Op::Unseal { .. } => grant == Grant::Seal,
			};
			assert_eq!(carries(grant, &op), expected, "{grant:?} and {op:?}");
			assert_eq!(admit(grant, &op, true).is_ok(), expected, "{grant:?} and {op:?}");
			if !expected {
				assert_eq!(admit(grant, &op, true), Err(Refusal::NotGranted), "{grant:?} and {op:?} is refused by name");
			}
		}
	}
}

#[test]
fn only_pcrs_16_and_23_are_extended_and_every_pcr_is_read_sealed_to_and_quoted() {
	for pcr in 0..PCRS {
		let extend = admit(Grant::Measure, &Op::PcrExtend { pcr, digest: vec![0; 32] }, true);
		if pcr == 16 || pcr == 23 {
			assert_eq!(extend, Ok(()), "PCR {pcr}");
		} else {
			assert_eq!(extend, Err(Refusal::PcrNotAllowed), "PCR {pcr} is the firmware's, the system's or the dynamic root's");
		}
		assert_eq!(admit(Grant::Tpm, &Op::PcrRead { pcr }, true), Ok(()));
		assert_eq!(admit(Grant::Seal, &Op::Seal { pcr, secret: vec![1] }, true), Ok(()));
		assert_eq!(admit(Grant::Tpm, &Op::Quote { pcr, nonce: vec![] }, true), Ok(()));
	}
	assert_eq!(admit(Grant::Tpm, &Op::PcrRead { pcr: 24 }, true), Err(Refusal::PcrNotAllowed), "there is no PCR 24");
	assert_eq!(admit(Grant::Seal, &Op::Seal { pcr: 24, secret: vec![1] }, true), Err(Refusal::PcrNotAllowed));
	assert_eq!(admit(Grant::Tpm, &Op::Quote { pcr: 99, nonce: vec![] }, true), Err(Refusal::PcrNotAllowed));
}

#[test]
fn the_bounds_are_the_contracts() {
	assert_eq!(admit(Grant::Tpm, &Op::Random { count: MAX_RANDOM }, true), Ok(()));
	assert_eq!(admit(Grant::Tpm, &Op::Random { count: MAX_RANDOM + 1 }, true), Err(Refusal::Bounds));
	assert_eq!(admit(Grant::Tpm, &Op::Random { count: 0 }, true), Err(Refusal::Bounds));
	assert_eq!(admit(Grant::Measure, &Op::PcrExtend { pcr: 23, digest: vec![0; 31] }, true), Err(Refusal::Bounds), "a digest is SHA-256's thirty-two bytes");
	assert_eq!(admit(Grant::Seal, &Op::Seal { pcr: 16, secret: vec![1; MAX_SECRET] }, true), Ok(()));
	assert_eq!(admit(Grant::Seal, &Op::Seal { pcr: 16, secret: vec![1; MAX_SECRET + 1] }, true), Err(Refusal::Bounds), "ninety-six bytes: the library's 128 less the tag");
	assert_eq!(admit(Grant::Seal, &Op::Seal { pcr: 16, secret: vec![] }, true), Err(Refusal::Bounds));
	assert_eq!(admit(Grant::Seal, &Op::Unseal { sealed: vec![0; MAX_SEALED + 1] }, true), Err(Refusal::Bounds));
	assert_eq!(admit(Grant::Tpm, &Op::Quote { pcr: 16, nonce: vec![0; MAX_NONCE] }, true), Ok(()));
	assert_eq!(admit(Grant::Tpm, &Op::Quote { pcr: 16, nonce: vec![0; MAX_NONCE + 1] }, true), Err(Refusal::Bounds));
	assert_eq!(MAX_SECRET, 96);
}

#[test]
fn a_provisioned_owner_hierarchy_refuses_seal_unseal_and_quote_and_nothing_else() {
	for (grant, op) in [(Grant::Seal, Op::Seal { pcr: 16, secret: vec![1] }), (Grant::Seal, Op::Unseal { sealed: vec![1] }), (Grant::Tpm, Op::Quote { pcr: 16, nonce: vec![] })] {
		assert_eq!(admit(grant, &op, false), Err(Refusal::OwnerHierarchyUnavailable), "{op:?}");
	}
	for (grant, op) in [
		(Grant::Tpm, Op::Info),
		(Grant::Tpm, Op::Random { count: 1 }),
		(Grant::Tpm, Op::PcrRead { pcr: 0 }),
		(Grant::Measure, Op::PcrExtend { pcr: 16, digest: vec![0; 32] }),
	] {
		assert_eq!(admit(grant, &op, false), Ok(()), "{op:?} keeps working");
	}
	assert_eq!(admit(Grant::Tpm, &Op::Seal { pcr: 16, secret: vec![1] }, false), Err(Refusal::NotGranted), "the grant is told first");
}

#[test]
fn a_sealed_secret_opens_for_the_component_that_sealed_it_and_no_other() {
	let sealed = tagged(b"tpm", b"secret");
	assert_eq!(sealed.len(), TAG_LEN + 6);
	assert_eq!(untagged(b"tpm", &sealed), Ok(b"secret".to_vec()));
	assert_eq!(untagged(b"tpmprobe", &sealed), Err(Refusal::OtherComponent), "another component's name is another tag");
	assert_eq!(untagged(b"tpm", &sealed[..TAG_LEN - 1]), Err(Refusal::OtherComponent), "shorter than a tag carries none");
	assert_eq!(untagged(b"tpm", &tagged(b"tpm", b"")), Ok(Vec::new()));
	assert_ne!(tag(b"tpm"), tag(b"tpm "), "the whole name, byte for byte");
}

#[test]
fn one_call_at_the_driver_one_per_connection_and_sixteen_waiting() {
	let mut queue: Queue<u32> = Queue::default();
	assert!(matches!(queue.submit(1, 100, Op::Info).as_slice(), [Effect::Refuse(Pending { call: 100, .. }, Refusal::Unavailable)]), "no provider, no queue");
	queue.arrived();
	assert!(matches!(queue.submit(1, 100, Op::Info).as_slice(), [Effect::Send(Pending { connection: 1, call: 100, .. })]), "an idle driver is sent the call at once");
	assert!(matches!(queue.submit(1, 101, Op::Info).as_slice(), [Effect::Refuse(Pending { call: 101, .. }, Refusal::Busy)]), "a second call on one connection is busy");
	for connection in 2..2 + MAX_WAITING as u32 {
		assert_eq!(queue.submit(connection, connection * 10, Op::Info), Vec::new(), "connection {connection} waits");
	}
	assert_eq!(queue.waiting(), MAX_WAITING);
	assert!(matches!(queue.submit(99, 990, Op::Info).as_slice(), [Effect::Refuse(Pending { call: 990, .. }, Refusal::Busy)]), "the seventeenth waiting call is busy");
	let (done, next) = queue.answered();
	assert_eq!(done.map(|pending| pending.call), Some(100));
	assert!(matches!(next.as_slice(), [Effect::Send(Pending { connection: 2, call: 20, .. })]), "in the order received");
	assert!(matches!(queue.submit(1, 102, Op::Info).as_slice(), []), "the first connection may ask again, and waits");
}

#[test]
fn a_lost_driver_interrupts_the_call_it_held_and_never_replays_it() {
	let mut queue: Queue<u32> = Queue::default();
	queue.arrived();
	queue.submit(1, 10, Op::PcrExtend { pcr: 16, digest: vec![0; 32] });
	queue.submit(2, 20, Op::Info);
	queue.submit(3, 30, Op::Random { count: 4 });
	let lost: Vec<(u32, Refusal)> = queue
		.lost()
		.into_iter()
		.map(|effect| match effect {
			Effect::Refuse(pending, refusal) => (pending.call, refusal),
			Effect::Send(_) => panic!("nothing is sent while the provider is going"),
		})
		.collect();
	assert_eq!(lost, vec![(10, Refusal::Interrupted), (20, Refusal::Unavailable), (30, Refusal::Unavailable)]);
	assert!(queue.in_flight().is_none() && queue.waiting() == 0, "nothing is kept to replay");
	assert!(matches!(queue.submit(1, 11, Op::Info).as_slice(), [Effect::Refuse(Pending { call: 11, .. }, Refusal::Unavailable)]), "while no provider is present");
	queue.arrived();
	assert!(matches!(queue.submit(1, 12, Op::Info).as_slice(), [Effect::Send(Pending { call: 12, .. })]), "and served again once one is");
	assert_eq!(queue.answered().1, Vec::new(), "the extend that was interrupted is not sent again");
}

#[test]
fn a_closed_connection_leaves_the_queue_and_its_call_at_the_driver_is_answered_to_nobody() {
	let mut queue: Queue<u32> = Queue::default();
	queue.arrived();
	queue.submit(1, 10, Op::Info);
	queue.submit(2, 20, Op::Info);
	queue.submit(3, 30, Op::Info);
	queue.closed(2);
	queue.closed(1);
	assert_eq!(queue.waiting(), 1);
	let (done, next) = queue.answered();
	assert_eq!(done.map(|pending| pending.connection), Some(1), "the driver's answer still arrives, for a connection that is gone");
	assert!(matches!(next.as_slice(), [Effect::Send(Pending { connection: 3, .. })]), "and the next is the third, the second having left");
}

#[test]
fn the_digest_is_the_loaders_to_the_byte() {
	// The empty input, one block, the padding boundary and several blocks: the cases a SHA-256 gets wrong.
	for length in [0usize, 1, 55, 56, 63, 64, 65, 119, 128, 1000] {
		let input: Vec<u8> = (0..length).map(|at| (at * 31 + 7) as u8).collect();
		assert_eq!(sha256(&input), bootproto::sha256::digest(&input), "{length} bytes");
	}
	assert_eq!(sha256(b"abc")[..4], [0xba, 0x78, 0x16, 0xbf], "the standard's own first test vector");
}
