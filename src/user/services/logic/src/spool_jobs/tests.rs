use super::*;

const PRINTER: u32 = 1;
const ATTACHMENT: u64 = 5;

fn staged(spool: &mut Spool, client: u32, bytes: &[u8]) -> u32 {
	let id = spool.create(client, PRINTER, ATTACHMENT, true, bytes.len() as u32).unwrap();
	for chunk in bytes.chunks(FRAME) {
		assert_eq!(spool.write(id, chunk), Ok(chunk.len() as u32));
	}
	id
}

// Send everything a printer is owed through a backend that accepts `accepted(offered)` of each frame, and
// return what reached it.
fn transmit(spool: &mut Spool, mut accepted: impl FnMut(usize) -> usize, now: u64) -> Vec<u8> {
	let mut sink = Vec::new();
	for _ in 0..10_000 {
		let Some(frame) = spool.next_frame(PRINTER, ATTACHMENT, now) else { break };
		let (job, bytes) = (frame.job, frame.bytes.to_vec());
		let take = accepted(bytes.len()).min(bytes.len());
		sink.extend_from_slice(&bytes[..take]);
		spool.written(job, take as u32, now).unwrap();
	}
	sink
}

#[test]
fn admission_holds_every_bound_before_anything_is_reserved() {
	let mut spool = Spool::new();
	assert_eq!(spool.create(1, PRINTER, ATTACHMENT, false, 10), Err(Refusal::Unsupported), "a language the printer does not take");
	assert_eq!(spool.create(1, PRINTER, ATTACHMENT, true, 0), Err(Refusal::Invalid), "a zero-length job");
	assert_eq!(spool.create(1, PRINTER, ATTACHMENT, true, MAX_JOB_BYTES + 1), Err(Refusal::Invalid), "past the advertised maximum");
	assert!(spool.create(1, PRINTER, ATTACHMENT, true, MAX_JOB_BYTES).is_ok(), "exactly the maximum");
	assert!(spool.create(1, PRINTER, ATTACHMENT, true, 1).is_ok());
	assert_eq!(spool.create(1, PRINTER, ATTACHMENT, true, 1), Err(Refusal::Exhausted), "a third job for one client");
	// 16 MB reserved across every job: four clients at 4 MB each fill it exactly.
	let mut full = Spool::new();
	for client in 0..4 {
		full.create(client, PRINTER, ATTACHMENT, true, MAX_JOB_BYTES).unwrap();
	}
	assert_eq!(full.reserved(), MAX_RESERVED);
	assert_eq!(full.create(9, PRINTER, ATTACHMENT, true, 1), Err(Refusal::Exhausted), "one byte past 16 MB");
	// Sixteen live records in all.
	let mut many = Spool::new();
	for client in 0..8 {
		many.create(client, PRINTER, ATTACHMENT, true, 1).unwrap();
		many.create(client, PRINTER, ATTACHMENT, true, 1).unwrap();
	}
	assert_eq!(many.create(99, PRINTER, ATTACHMENT, true, 1), Err(Refusal::Exhausted), "a seventeenth job");
}

#[test]
fn a_write_takes_a_whole_frame_or_none_and_past_the_declaration_fails_the_job() {
	let mut spool = Spool::new();
	let id = spool.create(1, PRINTER, ATTACHMENT, true, 10).unwrap();
	assert_eq!(spool.write(id, &[0; FRAME + 1]), Err(Refusal::Invalid), "an oversized frame, refused before anything");
	assert_eq!(spool.write(id, &[1; 6]), Ok(6));
	assert_eq!(spool.submit(id, 0), Err(Refusal::Invalid), "short of the declared length");
	assert_eq!(spool.write(id, &[2; 5]), Err(Refusal::Invalid), "past the declared length: none of it is taken");
	let status = spool.status(id).unwrap();
	assert_eq!((status.state, status.cause, status.staged), (State::Failed, Some(Cause::SizeLimit), 6));
	assert_eq!(spool.reserved(), 0, "the reservation came back at once");
}

#[test]
fn submit_is_explicit_exact_and_happens_once() {
	let mut spool = Spool::new();
	let id = staged(&mut spool, 1, &[7; 100]);
	assert_eq!(spool.next_frame(PRINTER, ATTACHMENT, 0), None, "nothing is eligible before submission");
	assert_eq!(spool.submit(id, 1).unwrap().state, State::Queued);
	assert_eq!(spool.write(id, &[1]), Err(Refusal::Invalid), "no write after submission");
	assert_eq!(spool.submit(id, 2).unwrap().state, State::Queued, "a second submit answers the state");
	let sink = transmit(&mut spool, |offered| offered, 3);
	assert_eq!(sink, alloc::vec![7; 100], "sent exactly once");
	assert_eq!(spool.submit(id, 4).unwrap().state, State::Transferred);
	assert_eq!(transmit(&mut spool, |offered| offered, 5), Vec::<u8>::new(), "and never again");
}

#[test]
fn closing_never_submits_and_a_submitted_job_goes_on_unwatched() {
	let mut spool = Spool::new();
	// A crash mid-write: the channel closes on a writing job, which is abandoned and refunded.
	let abandoned = spool.create(1, PRINTER, ATTACHMENT, true, 10).unwrap();
	spool.write(abandoned, &[1; 5]).unwrap();
	spool.close(abandoned);
	assert_eq!(spool.reserved(), 0);
	assert!(spool.is_empty(), "an abandoned, unwatched job is retired at once");
	assert_eq!(transmit(&mut spool, |offered| offered, 0), Vec::<u8>::new(), "and nothing of it is ever sent");
	// A submitted job whose channel closes completes anyway, and its record goes when it does.
	let detached = staged(&mut spool, 1, &[3; 20]);
	spool.submit(detached, 0).unwrap();
	spool.close(detached);
	assert_eq!(spool.len(), 1, "still queued, unwatched");
	assert_eq!(transmit(&mut spool, |offered| offered, 1), alloc::vec![3; 20]);
	assert!(spool.is_empty(), "a detached terminal record is retired immediately");
	// A watched terminal record stays, still charged, until its channel closes.
	let watched = staged(&mut spool, 1, &[4; 5]);
	spool.submit(watched, 0).unwrap();
	transmit(&mut spool, |offered| offered, 1);
	assert_eq!((spool.len(), spool.charged(1), spool.reserved()), (1, 1, 0));
	spool.close(watched);
	assert!(spool.is_empty());
}

#[test]
fn a_job_larger_than_a_frame_goes_by_acknowledged_prefixes_without_duplication() {
	let mut spool = Spool::new();
	let body: Vec<u8> = (0..10_000u32).map(|n| (n % 251) as u8).collect();
	let id = staged(&mut spool, 1, &body);
	spool.submit(id, 0).unwrap();
	// The backend takes 1000 of each frame; the rest is offered again, from exactly where it stopped.
	let mut offers = Vec::new();
	let sink = transmit(
		&mut spool,
		|offered| {
			offers.push(offered);
			1000
		},
		1,
	);
	assert_eq!(sink, body, "every byte once, in order");
	assert!(offers.iter().all(|&offered| offered <= FRAME));
	assert_eq!(spool.status(id).unwrap().state, State::Transferred);
	// `again` accepts nothing and the frame is offered again, whole.
	let again = staged(&mut spool, 2, &[9; 50]);
	spool.submit(again, 0).unwrap();
	let frame = spool.next_frame(PRINTER, ATTACHMENT, 1).unwrap();
	assert_eq!((frame.offset, frame.bytes.len()), (0, 50));
	assert_eq!(spool.next_frame(PRINTER, ATTACHMENT, 1), None, "one write outstanding per printer");
	spool.written(again, 0, 1).unwrap();
	assert_eq!(spool.next_frame(PRINTER, ATTACHMENT, 2).map(|frame| (frame.offset, frame.bytes.len())), Some((0, 50)));
	// A count past what was offered acknowledges nothing: the job fails, uncertain.
	assert_eq!(spool.written(again, 51, 2), Err(Refusal::Invalid));
	let status = spool.status(again).unwrap();
	assert_eq!((status.state, status.cause, status.uncertain), (State::Failed, Some(Cause::BackendError), true));
}

#[test]
fn jobs_go_in_submission_order_and_printers_are_independent() {
	let mut spool = Spool::new();
	let first = staged(&mut spool, 1, &[1; 10]);
	let second = staged(&mut spool, 2, &[2; 10]);
	let elsewhere = spool.create(3, 2, ATTACHMENT, true, 10).unwrap();
	spool.write(elsewhere, &[3; 10]).unwrap();
	spool.submit(second, 0).unwrap();
	spool.submit(first, 1).unwrap();
	spool.submit(elsewhere, 2).unwrap();
	assert_eq!(transmit(&mut spool, |offered| offered, 3), [[2u8; 10], [1u8; 10]].concat(), "the earlier submission first, contiguous");
	assert!(spool.pending(2), "another printer's job is untouched by this one's");
}

#[test]
fn stalls_and_lifetimes_end_jobs_and_say_what_is_uncertain() {
	let mut spool = Spool::new();
	let id = staged(&mut spool, 1, &[5; 10]);
	spool.submit(id, 100).unwrap();
	let frame = spool.next_frame(PRINTER, ATTACHMENT, 100).unwrap();
	assert_eq!(frame.bytes.len(), 10);
	spool.tick(100 + STALL_TICKS - 1);
	assert_eq!(spool.status(id).unwrap().state, State::Active);
	spool.tick(100 + STALL_TICKS);
	let status = spool.status(id).unwrap();
	assert_eq!((status.state, status.cause, status.uncertain), (State::Failed, Some(Cause::Stalled), true), "a write was outstanding: delivery is uncertain");
	// Progress resets the stall, never the lifetime.
	let mut slow = Spool::new();
	let id = staged(&mut slow, 1, &[6; 100]);
	slow.submit(id, 0).unwrap();
	let mut now = 0;
	while now < LIFETIME_TICKS {
		if let Some(frame) = slow.next_frame(PRINTER, ATTACHMENT, now) {
			let job = frame.job;
			slow.written(job, 1, now).unwrap();
		}
		now += STALL_TICKS / 2;
		slow.tick(now);
	}
	assert_eq!(slow.status(id).unwrap().cause, Some(Cause::Lifetime));
}

#[test]
fn removal_reset_and_cancel_never_replay() {
	let mut spool = Spool::new();
	let active = staged(&mut spool, 1, &[1; 8000]);
	let queued = staged(&mut spool, 2, &[2; 10]);
	let writing = spool.create(3, PRINTER, ATTACHMENT, true, 10).unwrap();
	spool.submit(active, 0).unwrap();
	spool.submit(queued, 0).unwrap();
	let frame = spool.next_frame(PRINTER, ATTACHMENT, 0).unwrap();
	let job = frame.job;
	spool.written(job, FRAME as u32, 0).unwrap();
	spool.next_frame(PRINTER, ATTACHMENT, 0).unwrap();
	// Unplugged with a write outstanding: acknowledged what was, uncertain about the rest.
	spool.printer_lost(PRINTER);
	let status = spool.status(active).unwrap();
	assert_eq!((status.state, status.cause, status.acknowledged, status.uncertain), (State::Failed, Some(Cause::Unplugged), FRAME as u32, true));
	assert_eq!(spool.status(queued).unwrap().cause, Some(Cause::Unplugged));
	assert_eq!(spool.status(writing).unwrap().cause, Some(Cause::Unplugged));
	assert_eq!(spool.reserved(), 0);
	// A replacement generation is another attachment: nothing admitted for the old one is sent to it.
	let mut replaced = Spool::new();
	let old = staged(&mut replaced, 1, &[3; 10]);
	replaced.submit(old, 0).unwrap();
	assert_eq!(replaced.next_frame(PRINTER, ATTACHMENT + 1, 1), None);
	assert_eq!(replaced.status(old).unwrap().cause, Some(Cause::Reset));
	// Cancel: a queued job leaves the queue; an active one stops with its count and its uncertainty.
	let mut cancelled = Spool::new();
	let running = staged(&mut cancelled, 1, &[4; 10]);
	let waiting = staged(&mut cancelled, 2, &[5; 10]);
	cancelled.submit(running, 0).unwrap();
	cancelled.submit(waiting, 1).unwrap();
	cancelled.next_frame(PRINTER, ATTACHMENT, 2).unwrap();
	assert_eq!(cancelled.cancel(waiting).unwrap().state, State::Cancelled);
	let stopped = cancelled.cancel(running).unwrap();
	assert_eq!((stopped.state, stopped.uncertain), (State::Cancelled, true));
	assert_eq!(cancelled.next_frame(PRINTER, ATTACHMENT, 3), None, "nothing after either");
	// A reset ends everything on the printer too.
	let mut reset = Spool::new();
	let id = staged(&mut reset, 1, &[6; 10]);
	reset.submit(id, 0).unwrap();
	reset.printer_reset(PRINTER);
	assert_eq!(reset.status(id).unwrap().cause, Some(Cause::Reset));
}
