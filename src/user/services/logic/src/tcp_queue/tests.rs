//! What the sender owes the wire, and what an acknowledgement takes off it.

use super::*;

const MSS: usize = 1460;
const ROOM: usize = MAX_UNACKED_TOTAL;

fn queue() -> SendQueue {
	SendQueue::new(1000)
}

#[test]
fn accepted_bytes_are_copied_and_the_count_is_what_was_taken() {
	// THE CONTRACT `send` REPORTS. The bytes are in the queue before the call returns, so a caller
	// may reuse or free its buffer immediately - which is the property the fixture below is about.
	let mut q = queue();
	let mut buffer = alloc::vec![b'a'; 10];
	assert_eq!(q.accept(&buffer, ROOM), 10);
	buffer.fill(b'z');
	assert_eq!(q.pending(), &alloc::vec![b'a'; 10][..], "what was accepted is what goes out, not what the buffer holds afterwards");
}

#[test]
fn a_send_past_the_queue_budget_accepts_the_prefix_and_no_more() {
	let mut q = queue();
	let offered = alloc::vec![b'x'; MAX_UNACKED_PER_FLOW + 4096];
	let accepted: usize = q.accept(&offered, ROOM);
	assert_eq!(accepted, MAX_UNACKED_PER_FLOW, "exactly the per-flow ceiling");
	assert_eq!(q.pending().len(), MAX_UNACKED_PER_FLOW);

	// AND THE NEXT OFFER IS REFUSED RATHER THAN SILENTLY DROPPED. Zero accepted is what a caller
	// turns into `Again`; a success that carried nothing would be the data-loss bug this replaces.
	assert_eq!(q.accept(b"more", ROOM), 0);
}

#[test]
fn the_service_wide_budget_binds_when_it_is_the_smaller_of_the_two() {
	// A connection alone cannot fill the service, and the service's room does not let one connection
	// past its own limit - so the smaller ceiling decides, whichever it is.
	let mut q = queue();
	assert_eq!(q.accept(&alloc::vec![b'x'; 8192], 1000), 1000, "the aggregate is tighter here");
	assert_eq!(q.accept(&alloc::vec![b'x'; 8192], 0), 0, "and a spent aggregate accepts nothing");
}

#[test]
fn a_buffer_is_segmented_across_several_segments_rather_than_truncated_to_one() {
	// THE DATA-LOSS BUG THIS REPLACES. The old path built one segment from the caller's buffer and
	// reported the whole length as sent.
	let mut q = queue();
	let payload = alloc::vec![b'p'; 3 * MSS + 100];
	assert_eq!(q.accept(&payload, ROOM), payload.len());

	let mut sent: usize = 0;
	let mut segments: usize = 0;
	while let Some(segment) = q.next_segment(MSS, 64 * 1024) {
		assert!(segment.len <= MSS);
		assert_eq!(segment.sequence, 1000 + sent as u32);
		assert_eq!(segment.offset, sent);
		sent += segment.len;
		segments += 1;
	}
	assert_eq!(sent, payload.len(), "every byte accepted goes out");
	assert_eq!(segments, 4, "three full segments and the remainder");
	assert_eq!(q.flight(), payload.len() as u32);
	assert_eq!(q.unsent(), 0);
}

#[test]
fn the_window_bounds_what_leaves_and_the_rest_waits() {
	let mut q = queue();
	q.accept(&alloc::vec![b'w'; 10 * MSS], ROOM);
	let segment = q.next_segment(MSS, 500).expect("a segment");
	assert_eq!(segment.len, 500, "the window is tighter than the segment size");
	assert_eq!(q.next_segment(MSS, 0), None, "a closed window sends nothing");
	assert_eq!(q.unsent(), 10 * MSS - 500, "and the rest is still owed");
}

#[test]
fn an_acknowledgement_retires_what_it_covers_and_leaves_the_rest() {
	let mut q = queue();
	q.accept(&alloc::vec![b'a'; 3000], ROOM);
	q.next_segment(MSS, 64 * 1024);
	q.next_segment(MSS, 64 * 1024);
	assert_eq!(q.flight(), 2920);

	assert_eq!(q.on_ack(1000 + 1460), AckOutcome::Advanced { bytes: 1460, covered_fin: false });
	assert_eq!(q.snd_una(), 2460);
	assert_eq!(q.flight(), 1460);
	assert_eq!(q.pending().len(), 3000 - 1460, "the retired bytes are gone and the rest stays");

	// A PARTIAL ACKNOWLEDGEMENT IS STILL AN ADVANCE, and it retires exactly what it covered.
	assert_eq!(q.on_ack(1000 + 2000), AckOutcome::Advanced { bytes: 540, covered_fin: false });
	assert_eq!(q.pending().len(), 1000);
}

#[test]
fn a_stale_or_impossible_acknowledgement_changes_nothing() {
	let mut q = queue();
	q.accept(&alloc::vec![b'a'; 100], ROOM);
	q.next_segment(MSS, 64 * 1024);
	q.on_ack(1050);

	assert_eq!(q.on_ack(1020), AckOutcome::Old, "older than what is already acknowledged");
	assert_eq!(q.snd_una(), 1050);
	// A PEER CANNOT ACKNOWLEDGE WHAT IT HAS NOT BEEN SENT, and believing one that does would retire
	// bytes this host still owes.
	assert_eq!(q.on_ack(9999), AckOutcome::Invalid);
	assert_eq!(q.snd_una(), 1050);
	assert_eq!(q.pending().len(), 50);
}

#[test]
fn a_repeated_acknowledgement_with_something_outstanding_is_a_duplicate() {
	let mut q = queue();
	q.accept(&alloc::vec![b'a'; 3000], ROOM);
	q.next_segment(MSS, 64 * 1024);
	q.next_segment(MSS, 64 * 1024);
	for expected in 1..=3u8 {
		assert_eq!(q.on_ack(1000), AckOutcome::Duplicate);
		assert_eq!(q.duplicates(), expected);
	}
	// AND AN ADVANCE CLEARS THE COUNT, so a later run of duplicates starts from nothing.
	q.on_ack(1100);
	assert_eq!(q.duplicates(), 0);

	// With nothing outstanding the same acknowledgement is not a duplicate at all: there is no
	// missing segment for it to be pointing at.
	let mut idle = queue();
	assert_eq!(idle.on_ack(1000), AckOutcome::Old);
	assert_eq!(idle.duplicates(), 0);
}

#[test]
fn the_sequence_arithmetic_survives_the_wrap() {
	// A queue that starts just below the wrap and crosses it: `ack > snd_una` is wrong for half the
	// space, and a sender using it would retire its whole queue on a stale acknowledgement.
	let mut q = SendQueue::new(u32::MAX - 100);
	q.accept(&alloc::vec![b'w'; 300], ROOM);
	let first = q.next_segment(200, 64 * 1024).expect("a segment");
	assert_eq!(first.sequence, u32::MAX - 100);
	let second = q.next_segment(200, 64 * 1024).expect("a segment");
	assert_eq!(second.sequence, 99, "the sequence wrapped");

	assert_eq!(q.on_ack(99), AckOutcome::Advanced { bytes: 200, covered_fin: false });
	assert_eq!(q.snd_una(), 99);
	assert_eq!(q.on_ack(u32::MAX - 50), AckOutcome::Old, "before the wrap is before, not after");
	assert_eq!(q.on_ack(199), AckOutcome::Advanced { bytes: 100, covered_fin: false });
	assert!(q.fully_acknowledged());
}

#[test]
fn the_fin_is_sequence_space_and_goes_behind_every_byte() {
	let mut q = queue();
	q.accept(b"hello", ROOM);
	assert!(q.queue_fin());
	assert!(!q.queue_fin(), "a second close queues nothing");
	// NOTHING GOES BEHIND THE FIN: a byte accepted after it would need the sequence number the FIN
	// has already taken.
	assert_eq!(q.accept(b"more", ROOM), 0);

	let segment = q.next_segment(MSS, 64 * 1024).expect("a segment");
	assert_eq!(segment.len, 5);
	assert!(segment.fin, "the FIN rides the last data segment rather than costing another one");
	assert_eq!(segment.sequence_len(), 6, "five bytes and the FIN's own sequence number");
	assert_eq!(q.flight(), 6);
	assert!(q.fin_sent());
	assert!(!q.fully_acknowledged());

	// Acknowledging the data alone does not finish it.
	assert_eq!(q.on_ack(1005), AckOutcome::Advanced { bytes: 5, covered_fin: false });
	assert!(!q.fully_acknowledged(), "the FIN is still outstanding");
	assert_eq!(q.on_ack(1006), AckOutcome::Advanced { bytes: 1, covered_fin: true });
	assert!(q.fin_acknowledged());
	assert!(q.fully_acknowledged());
}

#[test]
fn a_fin_with_nothing_to_carry_it_goes_out_alone_and_needs_no_window() {
	let mut q = queue();
	q.queue_fin();
	// A CLOSED RECEIVE WINDOW DOES NOT HOLD A FIN BACK. It carries no data, so a peer that has
	// stopped reading cannot stop this connection from finishing.
	let segment = q.next_segment(MSS, 0).expect("the FIN");
	assert!(segment.fin && segment.len == 0);
	assert_eq!(q.flight(), 1);
	assert_eq!(q.next_segment(MSS, 64 * 1024), None, "and it is sent once until something rewinds it");
}

#[test]
fn a_rewind_sends_the_outstanding_bytes_again_at_whatever_segment_size_is_now_in_force() {
	// RESEGMENTATION AFTER A PACKET TOO BIG IS NOT A SEPARATE MECHANISM. Lower the segment size and
	// rewind: the same bytes go back on the wire cut to fit the path that just complained.
	let mut q = queue();
	q.accept(&alloc::vec![b'r'; 2000], ROOM);
	let first = q.next_segment(MSS, 64 * 1024).expect("a segment");
	assert_eq!(first.len, MSS);
	assert!(!first.retransmitted);

	q.rewind();
	assert_eq!(q.flight(), 0, "nothing is outstanding until it is sent again");
	let smaller = q.next_segment(1220, 64 * 1024).expect("a segment");
	assert_eq!(smaller.len, 1220, "cut to the smaller path MTU");
	assert_eq!(smaller.sequence, 1000, "from the oldest unacknowledged byte");
	assert!(smaller.retransmitted, "and Karn's rule will refuse a measurement from it");
	assert!(q.outstanding_was_retransmitted());

	// Once everything outstanding is acknowledged the next segment is fresh again.
	while q.next_segment(1220, 64 * 1024).is_some() {}
	q.on_ack(3000);
	assert!(!q.outstanding_was_retransmitted());
}

#[test]
fn a_rewind_puts_the_fin_back_on_the_wire_with_the_data() {
	// A LOST FIN IS RETRANSMITTED LIKE A BYTE, which is the point of holding it in the queue: the
	// alternative sends it once and leaves the peer half-open for ever.
	let mut q = queue();
	q.accept(b"bye", ROOM);
	q.queue_fin();
	let first = q.next_segment(MSS, 64 * 1024).expect("a segment");
	assert!(first.fin);
	q.rewind();
	assert!(!q.fin_sent(), "the FIN is owed again");
	let again = q.next_segment(MSS, 64 * 1024).expect("a segment");
	assert!(again.fin && again.retransmitted);
	assert_eq!(again.sequence, 1000);
}

// ---------------------------------------------------------------------------------------------
// The four pieces driven together, because each is correct alone and the interesting failures are
// between them: a timer that fires against a queue that has nothing to resend, a window that grows
// while the queue is empty, a probe that a retransmission was supposed to cover.
// ---------------------------------------------------------------------------------------------

use crate::tcp_close::{CloseState, Closing};
use crate::tcp_rto::{MAX_DATA_RETRANSMISSIONS, Rto};
use crate::tcp_window::{CongestionWindow, DuplicateAction, Persist};

/// One sender, and a clock the test moves by hand.
struct Sender {
	tx: SendQueue,
	rto: Rto,
	cwnd: CongestionWindow,
	persist: Persist,
	closing: Closing,
	peer_window: u32,
	armed_at: Option<u64>,
}

impl Sender {
	fn new() -> Sender {
		Sender { tx: SendQueue::new(1000), rto: Rto::new(), cwnd: CongestionWindow::new(1000), persist: Persist::new(), closing: Closing::new(), peer_window: 64 * 1024, armed_at: None }
	}

	/// Everything the sender may put on the wire right now.
	fn drain(&mut self, now: u64) -> alloc::vec::Vec<Segment> {
		let mut out: alloc::vec::Vec<Segment> = alloc::vec::Vec::new();
		loop {
			let flight: u32 = self.tx.flight();
			let usable: u32 = self.cwnd.usable(flight, self.peer_window);
			let Some(segment) = self.tx.next_segment(1000, usable) else {
				break;
			};
			if flight == 0 {
				self.armed_at = Some(now);
			}
			if segment.fin {
				self.closing.on_fin_sent(now);
			}
			out.push(segment);
		}
		out
	}

	fn deadline(&self) -> Option<u64> {
		self.armed_at.map(|at| at + u64::from(self.rto.rto_ms()))
	}

	/// The retransmission timer fired.
	fn on_timeout(&mut self, now: u64) -> bool {
		if self.rto.back_off() >= MAX_DATA_RETRANSMISSIONS {
			return false;
		}
		self.cwnd.on_timeout(self.tx.flight());
		self.tx.rewind();
		self.armed_at = Some(now);
		true
	}

	fn on_ack(&mut self, ack: u32, window: u32, now: u64) -> AckOutcome {
		let flight: u32 = self.tx.flight();
		let snd_nxt: u32 = self.tx.snd_nxt();
		let retransmitted: bool = self.tx.outstanding_was_retransmitted();
		self.peer_window = window;
		let outcome = self.tx.on_ack(ack);
		match outcome {
			AckOutcome::Advanced { bytes, covered_fin } => {
				self.rto.sample(now.saturating_sub(self.armed_at.unwrap_or(now)) as u32, retransmitted);
				self.cwnd.on_new_ack(bytes, ack);
				if covered_fin {
					self.closing.on_fin_acknowledged(now);
				}
				self.armed_at = (self.tx.flight() != 0).then_some(now);
			}
			AckOutcome::Duplicate => {
				if self.cwnd.on_duplicate_ack(flight, snd_nxt) == DuplicateAction::FastRetransmit {
					self.tx.rewind();
				}
			}
			_ => {}
		}
		self.persist.on_window(window, self.tx.pending().len(), now, self.rto.rto_ms());
		outcome
	}
}

#[test]
fn a_lost_segment_is_retransmitted_by_the_timer_with_the_backoff_the_profile_fixes() {
	let mut sender = Sender::new();
	sender.tx.accept(&alloc::vec![b'l'; 500], ROOM);
	let first = sender.drain(0);
	assert_eq!(first.len(), 1);
	assert_eq!(sender.deadline(), Some(1000), "the initial RTO, before any measurement");

	// Nothing acknowledges it. The timer fires, the window collapses to one segment, and the same
	// bytes go back on the wire.
	assert!(sender.on_timeout(1000));
	assert_eq!(sender.cwnd.cwnd(), 1000, "one segment, not half a window");
	let again = sender.drain(1000);
	assert_eq!(again.len(), 1);
	assert_eq!(again[0].sequence, 1000, "from the oldest unacknowledged byte");
	assert!(again[0].retransmitted);
	assert_eq!(sender.deadline(), Some(3000), "and the next wait is twice as long");

	assert!(sender.on_timeout(3000));
	assert_eq!(sender.deadline(), Some(7000), "doubling again");

	// KARN'S RULE: the acknowledgement that finally arrives supplies no measurement, because it
	// cannot say which of the three transmissions it answers.
	sender.drain(3000);
	sender.on_ack(1500, 64 * 1024, 3100);
	assert!(!sender.rto.measured(), "no sample from a retransmitted segment");
	assert!(sender.tx.fully_acknowledged());
}

#[test]
fn a_sender_that_is_never_answered_gives_up_rather_than_retrying_for_ever() {
	let mut sender = Sender::new();
	sender.tx.accept(b"gone", ROOM);
	sender.drain(0);
	let mut now: u64 = 0;
	for _ in 0..MAX_DATA_RETRANSMISSIONS - 1 {
		now += u64::from(sender.rto.rto_ms());
		assert!(sender.on_timeout(now), "still trying");
		sender.drain(now);
	}
	now += u64::from(sender.rto.rto_ms());
	assert!(!sender.on_timeout(now), "the retry limit ends the connection with a typed failure");
}

#[test]
fn three_duplicate_acknowledgements_resend_without_waiting_for_the_timer() {
	let mut sender = Sender::new();
	sender.tx.accept(&alloc::vec![b'd'; 5000], ROOM);
	let sent = sender.drain(0);
	assert_eq!(sent.len(), 5, "the initial window allows five of these segments");

	// The peer keeps acknowledging the same point: segments are arriving and one is missing.
	for _ in 0..2 {
		assert_eq!(sender.on_ack(1000, 64 * 1024, 10), AckOutcome::Duplicate);
	}
	assert_eq!(sender.tx.flight(), 5000, "nothing has been resent yet");
	assert_eq!(sender.on_ack(1000, 64 * 1024, 10), AckOutcome::Duplicate);

	// THE THIRD ONE RESENDS AT ONCE, a whole RTO before the timer would have.
	let resent = sender.drain(10);
	assert!(!resent.is_empty());
	assert_eq!(resent[0].sequence, 1000);
	assert!(resent[0].retransmitted);
	assert!(sender.cwnd.in_recovery());
}

#[test]
fn the_congestion_window_and_not_the_queue_decides_how_much_is_on_the_wire() {
	let mut sender = Sender::new();
	sender.cwnd.on_timeout(10_000);
	assert_eq!(sender.cwnd.cwnd(), 1000, "one segment after a timeout");
	sender.tx.accept(&alloc::vec![b'c'; 20_000], ROOM);
	let sent = sender.drain(0);
	assert_eq!(sent.len(), 1, "one segment, however much is queued");
	assert_eq!(sender.tx.flight(), 1000);
	assert_eq!(sender.tx.unsent(), 19_000, "and the rest waits");

	// Each acknowledgement opens it by one more segment while slow start runs.
	sender.on_ack(2000, 64 * 1024, 100);
	assert_eq!(sender.drain(100).len(), 2);
}

#[test]
fn a_window_that_shuts_stops_the_sender_and_a_lost_update_is_recovered_by_persist() {
	// THE DEADLOCK THIS MECHANISM EXISTS FOR. With a zero window nothing is outstanding, so no
	// retransmission timer is running and nothing will ever fire; the update that reopens the window
	// rides an acknowledgement, and an acknowledgement can be lost.
	let mut sender = Sender::new();
	sender.tx.accept(&alloc::vec![b'z'; 4000], ROOM);
	sender.drain(0);
	sender.on_ack(5000, 0, 100);
	// The caller has more to send and the window is shut, which is the state the deadlock lives in:
	// nothing outstanding, something owed, and no reason for any timer to fire.
	sender.tx.accept(&alloc::vec![b'z'; 3000], ROOM);
	sender.persist.on_window(0, sender.tx.pending().len(), 100, sender.rto.rto_ms());
	assert_eq!(sender.tx.flight(), 0, "everything sent is acknowledged");
	assert_eq!(sender.deadline(), None, "so no retransmission timer is armed - nothing to retransmit");
	assert!(sender.persist.running(), "and the probe schedule is what is left");
	let due: u64 = sender.persist.due_ms().expect("a probe is scheduled");

	assert!(sender.drain(due).is_empty(), "a shut window sends no data");
	assert!(sender.persist.fire(due), "the probe goes out instead");

	// The peer answers the probe with its real window, and the transfer resumes.
	sender.on_ack(5000, 8000, due + 10);
	assert_eq!(sender.tx.unsent(), 3000, "the bytes that were waiting are still waiting");
	assert!(!sender.persist.running());
	assert!(!sender.drain(due + 10).is_empty(), "and the rest goes out");
}

#[test]
fn a_close_with_data_in_flight_sends_the_fin_behind_the_data_and_waits_for_both() {
	let mut sender = Sender::new();
	sender.tx.accept(&alloc::vec![b'f'; 2500], ROOM);
	sender.drain(0);
	assert_eq!(sender.tx.flight(), 2500);

	sender.tx.queue_fin();
	// THE FIN IS BEHIND EVERY BYTE, so there is nothing left to carry it and it goes out alone once
	// the data has been sent.
	let fin = sender.drain(0);
	assert_eq!(fin.len(), 1);
	assert!(fin[0].fin && fin[0].len == 0);
	assert_eq!(sender.closing.state(), CloseState::FinWait1);

	// Acknowledging the data alone finishes nothing.
	sender.on_ack(3500, 64 * 1024, 100);
	assert!(!sender.tx.fully_acknowledged());
	assert_eq!(sender.closing.state(), CloseState::FinWait1);

	sender.on_ack(3501, 64 * 1024, 200);
	assert!(sender.tx.fully_acknowledged());
	assert_eq!(sender.closing.state(), CloseState::FinWait2);
	assert_eq!(sender.deadline(), None, "and the timer is disarmed with nothing owed");
}

#[test]
fn a_smaller_path_mtu_resegments_what_is_outstanding_and_sends_it_again() {
	// PACKET TOO BIG IS THE ONE ICMP MESSAGE A SENDER MUST ACT ON, and acting on it is a rewind: the
	// same bytes, cut to a size the complaining hop will carry.
	let mut sender = Sender::new();
	sender.tx.accept(&alloc::vec![b'm'; 3000], ROOM);
	let large = sender.drain(0);
	assert!(large.iter().all(|segment| segment.len <= 1000));

	sender.cwnd.set_smss(500);
	sender.tx.rewind();
	let mut small: alloc::vec::Vec<Segment> = alloc::vec::Vec::new();
	loop {
		let flight: u32 = sender.tx.flight();
		let usable: u32 = sender.cwnd.usable(flight, sender.peer_window);
		let Some(segment) = sender.tx.next_segment(500, usable) else {
			break;
		};
		small.push(segment);
	}
	assert!(small.iter().all(|segment| segment.len <= 500), "cut to the smaller path");
	assert_eq!(small[0].sequence, 1000, "from the oldest unacknowledged byte");
	assert_eq!(small.iter().map(|segment| segment.len).sum::<usize>(), 3000, "and every byte goes again");
}
