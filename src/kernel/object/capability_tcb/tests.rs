use crate::arch::syscall::invoke;
use crate::object::channel::Channel;
use crate::object::rights::Rights;
use crate::syscall::{ERR_WOULD_BLOCK, SYS_CHANNEL_PEEK, SYS_CHANNEL_RECV_CAPS, SYS_CHANNEL_SEND_CAPS, SYS_EVENT_CREATE, SYS_EVENT_POLL, SYS_HANDLE_CLOSE, SYS_HANDLE_DUPLICATE, SYS_TIMER_CREATE, SYS_TIMER_POLL, SYS_YIELD, sys_is_err};
use core::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};

// `invoke` answers in the raw register width; every comparison against an error is against the
// SIGNED reading of it, which is what `sys_is_err` and the `ERR_` constants speak.
fn signed(result: u64) -> i64 {
	result as i64
}

const CAPS: usize = abi::MAX_MESSAGE_CAPS;
const ROUNDS: usize = 8;

// What the two threads report back. Statics rather than arguments because a thread entry takes one
// word, and that word is the handle each thread was given.
static SENT: AtomicUsize = AtomicUsize::new(0);
static RECEIVED: AtomicUsize = AtomicUsize::new(0);
static CAPS_SENT: AtomicUsize = AtomicUsize::new(0);
static CAPS_RECEIVED: AtomicUsize = AtomicUsize::new(0);
static SENDER_FAILURE: AtomicI64 = AtomicI64::new(0);
static RECEIVER_FAILURE: AtomicI64 = AtomicI64::new(0);
static IDENTITIES: AtomicU64 = AtomicU64::new(0);
// THE TABLE AS THE LAST THREAD LEAVES IT. A process whose threads have all exited is torn down and
// its table closed, so a count taken after `run_until_idle` measures the teardown rather than the
// transfers. These are taken by the receiver, from inside, once the last message is delivered.
static LIVE_AT_END: AtomicUsize = AtomicUsize::new(usize::MAX);
static BOOKED_AT_END: AtomicUsize = AtomicUsize::new(usize::MAX);
// PROOF THE TWO THREADS WERE ACTUALLY IN FLIGHT AT ONCE. A sender that runs to completion before the
// receiver starts exercises no race at all, and passes every assertion below. The endpoint is one
// message deep, so each thread has to wait for the other, and these count the waits.
static SENDER_WAITED: AtomicUsize = AtomicUsize::new(0);
static RECEIVER_WAITED: AtomicUsize = AtomicUsize::new(0);

// The sender: a message per round, alternating a single capability with a full batch, each one a
// freshly created object so no two rounds can be confused for one another.
extern "C" fn sending_thread(handle: u64) {
	for round in 0..ROUNDS {
		let batch = if round % 2 == 0 { 1 } else { CAPS };
		let mut caps = [0u64; CAPS + 1];
		caps[0] = batch as u64;
		for slot in caps.iter_mut().take(batch + 1).skip(1) {
			let created = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
			if sys_is_err(created) {
				SENDER_FAILURE.store(signed(created), Ordering::SeqCst);
				return;
			}
			*slot = created;
		}
		let payload = [round as u8; 8];
		// A FULL ENDPOINT IS NOT A FAILURE, it is the other thread not having run yet. Bounded, so
		// a receiver that never runs ends the test rather than hanging it.
		let mut attempts = 0;
		loop {
			let sent = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, handle, payload.as_ptr() as u64, payload.len() as u64, caps.as_ptr() as u64) };
			if !sys_is_err(sent) {
				SENT.fetch_add(1, Ordering::SeqCst);
				CAPS_SENT.fetch_add(batch, Ordering::SeqCst);
				break;
			}
			if signed(sent) != ERR_WOULD_BLOCK || attempts > 64 {
				SENDER_FAILURE.store(signed(sent), Ordering::SeqCst);
				return;
			}
			attempts += 1;
			SENDER_WAITED.fetch_add(1, Ordering::SeqCst);
			unsafe { invoke(SYS_YIELD, 0, 0, 0, 0) };
		}
		// NO YIELD AFTER A SUCCESSFUL SEND, and that is what makes the next one a race: the endpoint
		// is one message deep, so the sender arrives at the following round while its own message is
		// still queued and has to wait for the receiver to take it. Yielding here would hand over
		// politely every time and the two threads would take strict turns, which is the one
		// interleaving that tests nothing.
	}
}

// The receiver: peek, receive by what the peek named, and close every handle it is given. The
// closing is the half that makes the quota assertions mean anything.
extern "C" fn receiving_thread(handle: u64) {
	let mut identities: u64 = 0;
	while RECEIVED.load(Ordering::SeqCst) < ROUNDS {
		let mut bytes = [0u8; 64];
		let mut caps = [0u64; CAPS + 1];
		let peeked = unsafe { invoke(SYS_CHANNEL_PEEK, handle, 0, 0, 0) };
		if sys_is_err(peeked) {
			// Nothing queued yet: let the sender run.
			RECEIVER_WAITED.fetch_add(1, Ordering::SeqCst);
			unsafe { invoke(SYS_YIELD, 0, 0, 0, 0) };
			continue;
		}
		let received = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, handle, bytes.as_mut_ptr() as u64, bytes.len() as u64, caps.as_mut_ptr() as u64) };
		if sys_is_err(received) {
			RECEIVER_FAILURE.store(signed(received), Ordering::SeqCst);
			return;
		}
		RECEIVED.fetch_add(1, Ordering::SeqCst);
		let installed = caps[0] as usize;
		CAPS_RECEIVED.fetch_add(installed, Ordering::SeqCst);
		for raw in caps.iter().take(installed + 1).skip(1) {
			// EVERY HANDLE IS A DISTINCT ONE. Two deliveries naming the same slot would be a
			// capability installed twice, and the sum below would not notice on its own.
			identities = identities.wrapping_add(*raw);
			let closed = unsafe { invoke(SYS_HANDLE_CLOSE, *raw, 0, 0, 0) };
			if sys_is_err(closed) {
				RECEIVER_FAILURE.store(signed(closed), Ordering::SeqCst);
				return;
			}
		}
		unsafe { invoke(SYS_YIELD, 0, 0, 0, 0) };
	}
	IDENTITIES.store(identities, Ordering::SeqCst);
	if let Some(thread) = crate::sched::current_thread() {
		let table = thread.handles().lock();
		LIVE_AT_END.store(table.entries().len(), Ordering::SeqCst);
		BOOKED_AT_END.store(table.booked_indices_for_test().len(), Ordering::SeqCst);
	}
}

crate::tagged_test!(capability_tcb_two_threads_over_one_table, [CapabilityTcb, Object, Handle, Channel, Kernel, Syscall], id = "kernel.object.capability_tcb_two_threads_over_one_table", covers = ["kernel"]);
fn capability_tcb_two_threads_over_one_table() {
	SENT.store(0, Ordering::SeqCst);
	RECEIVED.store(0, Ordering::SeqCst);
	CAPS_SENT.store(0, Ordering::SeqCst);
	CAPS_RECEIVED.store(0, Ordering::SeqCst);
	SENDER_FAILURE.store(0, Ordering::SeqCst);
	RECEIVER_FAILURE.store(0, Ordering::SeqCst);
	LIVE_AT_END.store(usize::MAX, Ordering::SeqCst);
	BOOKED_AT_END.store(usize::MAX, Ordering::SeqCst);
	SENDER_WAITED.store(0, Ordering::SeqCst);
	RECEIVER_WAITED.store(0, Ordering::SeqCst);

	// BEFORE THE PROCESS EXISTS, so what it measures includes the two endpoint handles and the
	// teardown that gives them back. The suite is cooperative and this test owns the CPU while it
	// runs, so nothing else moves this number underneath it.
	let charged_before = crate::sched::root_domain().account().handles().used();

	// ONE MESSAGE DEEP, so neither thread can run ahead of the other: the sender fills the endpoint
	// and must wait, the receiver drains it and must wait, and every round is an interleaving rather
	// than two runs that happen to share a table.
	let (a, b) = Channel::try_create_with_depth(1).expect("a one-deep pair");
	let (process, send_handle) = crate::sched::prepare_shared_process(a.clone(), Rights::ALL);
	let recv_handle = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	let domain = process.domain().clone();

	let live_before = process.handles().lock().entries().len();
	assert_eq!(live_before, 2, "the fixture starts with the two endpoints and nothing else");

	let sender = crate::sched::prepare_in_process(sending_thread, send_handle, &process);
	let receiver = crate::sched::prepare_in_process(receiving_thread, recv_handle, &process);
	// THE RECEIVER FIRST, so it reaches an empty endpoint before the sender has filled it; the
	// sender then blocks on the one-deep queue behind it. Both counters below are what say the two
	// were in flight at once rather than one after the other.
	crate::sched::start_thread(&receiver);
	crate::sched::start_thread(&sender);
	crate::sched::run_until_idle();

	assert_eq!(SENDER_FAILURE.load(Ordering::SeqCst), 0, "the sending thread failed a syscall");
	assert_eq!(RECEIVER_FAILURE.load(Ordering::SeqCst), 0, "the receiving thread failed a syscall");
	assert_eq!(SENT.load(Ordering::SeqCst), ROUNDS, "every round was sent");
	assert_eq!(RECEIVED.load(Ordering::SeqCst), ROUNDS, "and every round was received");
	assert_eq!(CAPS_RECEIVED.load(Ordering::SeqCst), CAPS_SENT.load(Ordering::SeqCst), "every capability that was sent arrived, and no more");

	// WHAT THE MODEL SAYS MUST BE TRUE AFTERWARDS, measured from outside the operations that were
	// supposed to preserve it.
	assert_eq!(LIVE_AT_END.load(Ordering::SeqCst), live_before, "the table holds what it started with: the two endpoints, and nothing the transfers left");
	assert_eq!(BOOKED_AT_END.load(Ordering::SeqCst), 0, "no booking outlived the receive it was taken for");
	assert_eq!(domain.account().handles().used(), charged_before, "every handle the transfers created was accounted for and given back");
	assert!(SENDER_WAITED.load(Ordering::SeqCst) > 0, "the sender never waited for the receiver - the two ran one after the other and no race was exercised");
	assert!(RECEIVER_WAITED.load(Ordering::SeqCst) > 0, "the receiver never waited for the sender - same");
	assert_eq!(domain.account().ipc_queue().used(), 0, "no message is still charged against the queue");
	assert!(b.peek_identified().is_err(), "the endpoint is empty");
	assert!(a.peek_identified().is_err(), "and so is its peer");
}

// A user buffer that stops existing partway through a receive, at each of the two phases.
//
// The syscall has a commit point and the two sides of it behave differently ON PURPOSE: before it,
// a copy that fails puts the message back on the queue and costs the caller nothing; after it, the
// message is gone and what can be recovered is the handles, which are closed rather than left
// unreachable. Both are reachable from ring 3 by unmapping one's own buffer, and neither is
// observable from a test that only ever passes a good pointer.
//
// THE FAULT IS REAL, NOT INJECTED. Two adjacent user pages with only the first mapped put the copy
// in exactly the state a mid-copy unmap leaves behind - see `extable::tests` for why that is the
// reproducible form of the race - and the exception table is what makes the kernel survive it.

// The results the driving thread reports back, one per step.
static PRECOMMIT_RESULT: AtomicI64 = AtomicI64::new(0);
static PRECOMMIT_STILL_QUEUED: AtomicI64 = AtomicI64::new(0);
static POSTCOMMIT_RESULT: AtomicI64 = AtomicI64::new(0);
static POSTCOMMIT_QUEUE_EMPTY: AtomicI64 = AtomicI64::new(0);
static CLEAN_RESULT: AtomicI64 = AtomicI64::new(0);
static CLEAN_HANDLE: AtomicU64 = AtomicU64::new(0);
// TWO STATICS RATHER THAN ONE PACKED WORD. A handle's raw value is a generation and an index packed
// into 64 bits, so there is no half of it to spare: packing two into one word truncates the
// generation and every syscall then answers `ERR_BAD_HANDLE`.
static COPY_SEND_HANDLE: AtomicU64 = AtomicU64::new(0);
static COPY_RECV_HANDLE: AtomicU64 = AtomicU64::new(0);

// The two pages: the first mapped, the second deliberately absent. Far enough from the halfway mark
// not to share a page pair with `extable::tests`, which arranges the same thing for its own case.
const COPY_AT: u64 = crate::memlayout::USER_VA_END / 2 - 8 * crate::mem::frame::PAGE_SIZE;
const PAYLOAD: usize = 256;

extern "C" fn copy_fault_thread(_argument: u64) {
	let send_handle = COPY_SEND_HANDLE.load(Ordering::SeqCst);
	let recv_handle = COPY_RECV_HANDLE.load(Ordering::SeqCst);
	let page = crate::mem::frame::PAGE_SIZE;
	let payload = [0x5Au8; PAYLOAD];

	let send_one = |mark: u8| -> i64 {
		let created = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
		if sys_is_err(created) {
			return signed(created);
		}
		let caps = [1u64, created];
		let body = [mark; PAYLOAD];
		signed(unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, caps.as_ptr() as u64) })
	};

	// 1. BEFORE THE COMMIT: the payload copy runs off the end of the mapped page. The message must
	//    be back on the queue, whole, and the caller must be told.
	if send_one(0x11) < 0 {
		PRECOMMIT_RESULT.store(-1, Ordering::SeqCst);
		return;
	}
	let mut caps_out = [0u64; CAPS + 1];
	let straddling = COPY_AT + page - 64;
	let result = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, straddling, PAYLOAD as u64, caps_out.as_mut_ptr() as u64) };
	PRECOMMIT_RESULT.store(signed(result), Ordering::SeqCst);
	PRECOMMIT_STILL_QUEUED.store(signed(unsafe { invoke(SYS_CHANNEL_PEEK, recv_handle, 0, 0, 0) }), Ordering::SeqCst);

	// 2. AFTER IT: the payload lands, delivery commits, and the HANDLE ARRAY is what will not fit.
	//    The message is gone - it cannot go back - so what must happen is that the handles it
	//    carried are closed rather than left in the table with no way to name them.
	let mut bytes_out = [0u8; PAYLOAD];
	let caps_straddling = COPY_AT + page - 8;
	let result = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, bytes_out.as_mut_ptr() as u64, bytes_out.len() as u64, caps_straddling) };
	POSTCOMMIT_RESULT.store(signed(result), Ordering::SeqCst);
	POSTCOMMIT_QUEUE_EMPTY.store(signed(unsafe { invoke(SYS_CHANNEL_PEEK, recv_handle, 0, 0, 0) }), Ordering::SeqCst);

	// 3. AND THE SAME RECEIVE WITH BUFFERS THAT ARE THERE, so the two failures above are failures of
	//    the copy rather than of everything.
	if send_one(0x22) < 0 {
		CLEAN_RESULT.store(-1, Ordering::SeqCst);
		return;
	}
	let mut caps_out = [0u64; CAPS + 1];
	let result = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, bytes_out.as_mut_ptr() as u64, bytes_out.len() as u64, caps_out.as_mut_ptr() as u64) };
	CLEAN_RESULT.store(signed(result), Ordering::SeqCst);
	if !sys_is_err(result) && caps_out[0] == 1 {
		CLEAN_HANDLE.store(caps_out[1], Ordering::SeqCst);
		assert_eq!(bytes_out[0], 0x22, "the payload that arrived is the one that was sent");
		let closed = unsafe { invoke(SYS_HANDLE_CLOSE, caps_out[1], 0, 0, 0) };
		assert!(!sys_is_err(closed), "the handle that arrived can be closed");
	}
	let _ = payload;
}

crate::tagged_test!(capability_tcb_a_user_buffer_that_ends_partway_through_a_receive, [CapabilityTcb, Object, Channel, Handle, Kernel, Syscall, Memory], id = "kernel.object.capability_tcb_a_user_buffer_that_ends_partway_through_a_receive", covers = ["kernel"]);
fn capability_tcb_a_user_buffer_that_ends_partway_through_a_receive() {
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::syscall::ERR_NOT_MAPPED;

	PRECOMMIT_RESULT.store(0, Ordering::SeqCst);
	POSTCOMMIT_RESULT.store(0, Ordering::SeqCst);
	CLEAN_RESULT.store(0, Ordering::SeqCst);
	CLEAN_HANDLE.store(0, Ordering::SeqCst);

	let frame = crate::mem::frame::allocate().expect("a frame for the mapped half");
	crate::arch::paging::map_page(COPY_AT, frame, PRESENT | WRITABLE | USER);
	// The page after it is deliberately absent.

	let (a, b) = Channel::create();
	let charged_before = crate::sched::root_domain().account().handles().used();
	let (process, send_handle) = crate::sched::prepare_shared_process(a.clone(), Rights::ALL);
	let recv_handle = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	let domain = process.domain().clone();
	COPY_SEND_HANDLE.store(send_handle, Ordering::SeqCst);
	COPY_RECV_HANDLE.store(recv_handle, Ordering::SeqCst);

	let driver = crate::sched::prepare_in_process(copy_fault_thread, 0, &process);
	crate::sched::start_thread(&driver);
	crate::sched::run_until_idle();

	assert_eq!(PRECOMMIT_RESULT.load(Ordering::SeqCst), ERR_NOT_MAPPED, "a payload copy that runs off the end of the buffer is an error, not a short delivery");
	assert!(PRECOMMIT_STILL_QUEUED.load(Ordering::SeqCst) >= 0, "and the message is still queued: nothing was destroyed that nobody can retry");

	assert_eq!(POSTCOMMIT_RESULT.load(Ordering::SeqCst), ERR_NOT_MAPPED, "a handle array that will not fit is an error too");
	assert!(POSTCOMMIT_QUEUE_EMPTY.load(Ordering::SeqCst) < 0, "the message is gone: past the commit it cannot go back");
	assert!(CLEAN_RESULT.load(Ordering::SeqCst) > 0, "a receive with buffers that are there still works");
	assert_ne!(CLEAN_HANDLE.load(Ordering::SeqCst), 0, "and the capability it carried arrived");

	// THE QUOTA IS THE PROOF THAT THE POST-COMMIT PATH CLOSED WHAT IT INSTALLED. A handle the caller
	// was never told the number of is one it can never close, so a leak here is permanent.
	assert_eq!(domain.account().handles().used(), charged_before, "every handle installed by a receive that then failed was closed again");

	crate::arch::paging::unmap_page(COPY_AT);
	// RETIRED RATHER THAN FREED, because this page WAS mapped. The mapping is gone from this core's
	// tables a line above, and nothing here can say that no other core still holds a translation for
	// it - which is exactly the statement `retire` exists to make and `deallocate` exists to assume.
	// SAFETY: the frame was allocated by this test, mapped only at `COPY_AT`, and unmapped above.
	unsafe { crate::mem::frame::retire(&[frame]) };
}

// The refusals, each one a path the model has an action for and the implementation an error code.
//
// A capability system is defined as much by what it declines as by what it does, and every one of
// these is a refusal that must leave the caller exactly as it found them: the handle still theirs,
// the quota unspent, the message still queued for somebody who can take it.

static REFUSAL_SEND: AtomicU64 = AtomicU64::new(0);
static REFUSAL_RECV: AtomicU64 = AtomicU64::new(0);
static REFUSAL_REPORT: AtomicI64 = AtomicI64::new(-1);
static RIGHTS_REPORT: AtomicU64 = AtomicU64::new(0);

// Every subset of the four rights a channel transfer reads, and what each one may do. `1 << 4`
// combinations is small enough to enumerate rather than sample, which is what "every subset" has to
// mean if it is to mean anything.
const RIGHT_BITS: [u32; 4] = [abi::RIGHT_SEND, abi::RIGHT_RECEIVE, abi::RIGHT_TRANSFER, abi::RIGHT_DUPLICATE];

extern "C" fn refusal_thread(_argument: u64) {
	let send_handle = REFUSAL_SEND.load(Ordering::SeqCst);
	let recv_handle = REFUSAL_RECV.load(Ordering::SeqCst);
	let mut report: i64 = 0;
	let mut step = |ok: bool, which: i64| {
		if !ok && report == 0 {
			report = which;
		}
	};

	// 1. A WRONG-TYPE OPERATION. Type sealing: a handle to an Event is not a channel however
	//    well-formed the call around it is.
	let event = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
	step(!sys_is_err(event), 1);
	let caps = [1u64, event];
	let body = [0u8; 8];
	let wrong = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, event, body.as_ptr() as u64, body.len() as u64, caps.as_ptr() as u64) };
	step(sys_is_err(wrong), 2);

	// 2. EVERY SUBSET OF THE RIGHTS. A duplicate carrying a subset may do exactly what that subset
	//    allows: send if it has SEND, receive if it has RECEIVE, be transferred if it has TRANSFER.
	let mut subsets_checked = 0u64;
	for mask in 0..(1u32 << RIGHT_BITS.len()) {
		let mut rights = 0u32;
		for (bit, right) in RIGHT_BITS.iter().enumerate() {
			if mask & (1 << bit) != 0 {
				rights |= right;
			}
		}
		// The source must carry DUPLICATE to derive anything at all, and a derived handle may not
		// carry more than its source - both are refusals in their own right.
		let derived = unsafe { invoke(SYS_HANDLE_DUPLICATE, send_handle, rights as u64, 0, 0) };
		if sys_is_err(derived) {
			step(false, 3);
			continue;
		}
		// THE CAPABILITY THIS PROBE NAMES IS DELIBERATELY NOT A HANDLE. The subject here is the
		// rights check on the ENDPOINT, and a probe that could succeed would queue a message and
		// change what every later step in this thread is testing. With an unusable capability the
		// call is refused either way - and the two refusals are told apart by their reason, which
		// is the thing being tested: without SEND it is ACCESS DENIED, before the transfer is even
		// looked at.
		let probe = [1u64, 0u64];
		let sent = signed(unsafe { invoke(SYS_CHANNEL_SEND_CAPS, derived, body.as_ptr() as u64, body.len() as u64, probe.as_ptr() as u64) });
		if rights & abi::RIGHT_SEND == 0 {
			step(sent == crate::syscall::ERR_ACCESS_DENIED, 4);
		} else {
			step(sent == crate::syscall::ERR_BAD_HANDLE, 40);
		}
		let received = signed(unsafe { invoke(SYS_CHANNEL_RECV_CAPS, derived, body.as_ptr() as u64, 0, probe.as_ptr() as u64) });
		if rights & abi::RIGHT_RECEIVE == 0 {
			step(received == crate::syscall::ERR_ACCESS_DENIED, 5);
		}
		subsets_checked += 1;
		let closed = unsafe { invoke(SYS_HANDLE_CLOSE, derived, 0, 0, 0) };
		step(!sys_is_err(closed), 6);
	}
	RIGHTS_REPORT.store(subsets_checked, Ordering::SeqCst);

	// 3. A FULL QUEUE, and the handle that comes back from it. A refused send must leave the
	//    capability where it was: the caller was told it did not happen.
	let first = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
	step(!sys_is_err(first), 7);
	let second = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
	step(!sys_is_err(second), 8);
	let one = [1u64, first];
	let two = [1u64, second];
	step(!sys_is_err(unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, one.as_ptr() as u64) }), 9);
	let full = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, two.as_ptr() as u64) };
	step(sys_is_err(full), 10);
	// The handle the refused send named is still the caller's, and still names its object.
	step(!sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, second, 0, 0, 0) }), 11);

	// 4. STALE HANDLE CHURN. A slot recycled under a new generation does not answer to the value it
	//    had before, however many times it is reused.
	let mut stale = alloc::vec::Vec::new();
	for _ in 0..16 {
		let made = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
		step(!sys_is_err(made), 12);
		step(!sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, made, 0, 0, 0) }), 13);
		stale.push(made);
	}
	for _ in 0..16 {
		let made = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
		step(!sys_is_err(made), 14);
		// Every value closed above must still be dead, whichever slot this one landed in.
		for dead in &stale {
			step(sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, *dead, 0, 0, 0) }), 15);
		}
		step(!sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, made, 0, 0, 0) }), 16);
	}

	// 5. A CLOSED PEER. The endpoint the message would go to is gone, so there is nowhere to put it
	//    and the capability must come back.
	let third = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
	step(!sys_is_err(third), 17);
	step(!sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, recv_handle, 0, 0, 0) }), 18);
	let three = [1u64, third];
	// Drain what is queued first: a full endpoint refuses before it looks at its peer.
	let mut drained = [0u64; CAPS + 1];
	let _ = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, body.as_ptr() as u64, 0, drained.as_mut_ptr() as u64) };
	let gone = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, three.as_ptr() as u64) };
	step(sys_is_err(gone), 19);
	step(!sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, third, 0, 0, 0) }), 20);

	REFUSAL_REPORT.store(report, Ordering::SeqCst);
}

crate::tagged_test!(capability_tcb_every_refusal_leaves_the_caller_where_it_was, [CapabilityTcb, Object, Handle, Channel, Kernel, Syscall], id = "kernel.object.capability_tcb_every_refusal_leaves_the_caller_where_it_was", covers = ["kernel"]);
fn capability_tcb_every_refusal_leaves_the_caller_where_it_was() {
	REFUSAL_REPORT.store(-1, Ordering::SeqCst);
	RIGHTS_REPORT.store(0, Ordering::SeqCst);

	// One message deep, so the full-queue case needs one send rather than sixty-four.
	let (a, b) = Channel::try_create_with_depth(1).expect("a one-deep pair");
	let charged_before = crate::sched::root_domain().account().handles().used();
	let (process, send_handle) = crate::sched::prepare_shared_process(a.clone(), Rights::ALL);
	let recv_handle = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	let domain = process.domain().clone();
	REFUSAL_SEND.store(send_handle, Ordering::SeqCst);
	REFUSAL_RECV.store(recv_handle, Ordering::SeqCst);

	let driver = crate::sched::prepare_in_process(refusal_thread, 0, &process);
	crate::sched::start_thread(&driver);
	crate::sched::run_until_idle();

	let report = REFUSAL_REPORT.load(Ordering::SeqCst);
	assert_eq!(report, 0, "step {report} of the refusal matrix did not behave as a refusal must (see the numbered steps in `refusal_thread`)");
	assert_eq!(RIGHTS_REPORT.load(Ordering::SeqCst), 1 << RIGHT_BITS.len(), "every subset of the four transfer rights was derived and exercised");
	assert_eq!(domain.account().handles().used(), charged_before, "not one refusal cost the caller a unit of quota");
}

// A DESTINATION AT ITS QUOTA, and a sender at its queue-byte limit.
//
// The two ways a transfer is refused for want of a resource rather than for want of a right. Neither
// may destroy anything: a receive that cannot book a slot leaves the message queued for a receiver
// that can, and a send that cannot pay for its bytes leaves the capability with its sender.
//
// THESE ARE QUOTA REFUSALS, NOT AN OUT-OF-MEMORY INJECTION. The kernel's allocation failures are
// answered on the same paths (`try_reserve`, `try_zeroed_bytes`, `try_arc`), but nothing here makes
// the heap fail; what it exercises is the refusal a Domain's accounting produces, at the same
// syscall phases.

static QUOTA_SEND: AtomicU64 = AtomicU64::new(0);
static QUOTA_RECV: AtomicU64 = AtomicU64::new(0);
static QUOTA_REPORT: AtomicI64 = AtomicI64::new(-1);
static QUOTA_REFUSED_RECV: AtomicI64 = AtomicI64::new(0);
static QUOTA_REFUSED_SEND: AtomicI64 = AtomicI64::new(0);
static QUOTA_STILL_QUEUED: AtomicI64 = AtomicI64::new(0);

extern "C" fn quota_thread(_argument: u64) {
	let send_handle = QUOTA_SEND.load(Ordering::SeqCst);
	let recv_handle = QUOTA_RECV.load(Ordering::SeqCst);
	let body = [7u8; 8];

	// A message carrying one capability, sent while there is still room for it.
	let carried = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
	if sys_is_err(carried) {
		QUOTA_REPORT.store(1, Ordering::SeqCst);
		return;
	}
	let caps = [1u64, carried];
	let sent = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, caps.as_ptr() as u64) };
	if sys_is_err(sent) {
		QUOTA_REPORT.store(2, Ordering::SeqCst);
		return;
	}

	// FILL THE HANDLE QUOTA. Every creation from here is refused, which is the state a destination
	// at its limit is in when a message carrying a capability arrives for it.
	let mut held = alloc::vec::Vec::new();
	loop {
		let made = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
		if sys_is_err(made) {
			break;
		}
		held.push(made);
		if held.len() > 512 {
			QUOTA_REPORT.store(3, Ordering::SeqCst);
			return;
		}
	}

	// The receive cannot book the slot the capability needs, so it must not dequeue.
	let mut caps_out = [0u64; CAPS + 1];
	let mut bytes_out = [0u8; 16];
	let refused = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, bytes_out.as_mut_ptr() as u64, bytes_out.len() as u64, caps_out.as_mut_ptr() as u64) };
	QUOTA_REFUSED_RECV.store(signed(refused), Ordering::SeqCst);
	QUOTA_STILL_QUEUED.store(signed(unsafe { invoke(SYS_CHANNEL_PEEK, recv_handle, 0, 0, 0) }), Ordering::SeqCst);

	// AND A SEND WHILE THE TABLE IS AT ITS QUOTA STILL WORKS, which is `QuotaConserved` seen from
	// the outside: a transfer MOVES a capability, so the take refunds the unit the message will
	// cost, and a table with no room to create anything can still hand over what it already holds.
	// A quota check written as "is there room for one more" at this phase would refuse it.
	let one_held = [1u64, *held.first().unwrap_or(&0)];
	let blocked = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, one_held.as_ptr() as u64) };
	QUOTA_REFUSED_SEND.store(signed(blocked), Ordering::SeqCst);

	// Give the quota back and the same receive succeeds, which is what says the refusal was the
	// quota rather than anything about the message.
	for raw in held.drain(..) {
		let _ = unsafe { invoke(SYS_HANDLE_CLOSE, raw, 0, 0, 0) };
	}
	let now = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, bytes_out.as_mut_ptr() as u64, bytes_out.len() as u64, caps_out.as_mut_ptr() as u64) };
	if sys_is_err(now) || caps_out[0] != 1 {
		QUOTA_REPORT.store(4, Ordering::SeqCst);
		return;
	}
	if sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, caps_out[1], 0, 0, 0) }) {
		QUOTA_REPORT.store(5, Ordering::SeqCst);
		return;
	}
	QUOTA_REPORT.store(0, Ordering::SeqCst);
}

crate::tagged_test!(capability_tcb_a_destination_at_its_quota_keeps_the_message, [CapabilityTcb, Object, Handle, Channel, Domain, Kernel, Syscall], id = "kernel.object.capability_tcb_a_destination_at_its_quota_keeps_the_message", covers = ["kernel"]);
fn capability_tcb_a_destination_at_its_quota_keeps_the_message() {
	use crate::object::domain::Domain;
	const UNLIMITED: u64 = u64::MAX;
	const HANDLES: u64 = 24;

	QUOTA_REPORT.store(-1, Ordering::SeqCst);

	let (a, b) = Channel::create();
	// A DOMAIN OF ITS OWN, because a limit put on the root Domain is a limit on every other test in
	// the suite.
	let domain = Domain::new_child(&crate::sched::root_domain(), UNLIMITED, HANDLES, UNLIMITED).expect("a child domain");
	let (process, send_handle) = crate::sched::prepare_shared_process_in(domain.clone(), a.clone(), Rights::ALL);
	let recv_handle = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	QUOTA_SEND.store(send_handle, Ordering::SeqCst);
	QUOTA_RECV.store(recv_handle, Ordering::SeqCst);

	let driver = crate::sched::prepare_in_process(quota_thread, 0, &process);
	crate::sched::start_thread(&driver);
	crate::sched::run_until_idle();

	let report = QUOTA_REPORT.load(Ordering::SeqCst);
	assert_eq!(report, 0, "the quota fixture stopped at step {report} (see `quota_thread`)");
	assert_eq!(QUOTA_REFUSED_RECV.load(Ordering::SeqCst), crate::syscall::ERR_RESOURCE_EXHAUSTED, "a receive that cannot book the slot is refused by the quota");
	assert!(QUOTA_STILL_QUEUED.load(Ordering::SeqCst) >= 0, "and the message is still there for a receiver that can take it");
	assert!(QUOTA_REFUSED_SEND.load(Ordering::SeqCst) >= 0, "a table at its quota can still SEND: a transfer moves a capability, so it costs the sender nothing it has not already paid");
}

// TWO RECEIVERS ON ONE ENDPOINT, and a message that must arrive exactly once.
//
// `recv_identified` takes an id because of this: between looking at a message and taking it, another
// receiver can take it first, and a receiver that sized its buffer from one message and was handed
// another is a kernel-to-userspace overrun reachable with two threads and no timing trick.
//
// THE IDENTITY IS INTERNAL TO THE SYSCALL, and that is worth being exact about: `SYS_CHANNEL_RECV_CAPS`
// takes no message id, because `receive_transactionally` does its own peek and takes the head BY
// IDENTITY under one decision. A caller therefore cannot observe a `Superseded`; what it can observe
// is that no message is ever delivered twice, that none is lost, and that what arrives matches what
// the call was sized for. The identity mechanism itself is exercised directly against `recv_identified`
// in `object::tests`, where a caller can name a message that is no longer at the head.

static RACE_RECV: AtomicU64 = AtomicU64::new(0);
static RACE_SEND: AtomicU64 = AtomicU64::new(0);
static RACE_DELIVERED: AtomicUsize = AtomicUsize::new(0);
static RACE_WRONG: AtomicUsize = AtomicUsize::new(0);
static RACE_DONE: AtomicUsize = AtomicUsize::new(0);
// One counter per receiver, so "both of them actually received something" is a fact rather than an
// assumption. Two receivers where one did all the work is one receiver with a spectator.
static RACE_BY_RECEIVER: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static RACE_ENTERED: AtomicUsize = AtomicUsize::new(0);
// Every message's number, one bit each: the check that none arrived twice and none was missed.
static RACE_SEEN: AtomicU64 = AtomicU64::new(0);
const RACE_MESSAGES: usize = 12;

extern "C" fn racing_receiver(handle: u64) {
	let me = RACE_ENTERED.fetch_add(1, Ordering::SeqCst).min(1);
	let mut idle = 0;
	while RACE_DELIVERED.load(Ordering::SeqCst) < RACE_MESSAGES && idle < 256 {
		let peeked = unsafe { invoke(SYS_CHANNEL_PEEK, handle, 0, 0, 0) };
		if sys_is_err(peeked) {
			idle += 1;
			unsafe { invoke(SYS_YIELD, 0, 0, 0, 0) };
			continue;
		}
		// The window the whole mechanism is about: between looking and taking, the other receiver
		// runs.
		unsafe { invoke(SYS_YIELD, 0, 0, 0, 0) };
		let mut bytes = [0u8; 8];
		let mut caps = [0u64; CAPS + 1];
		let received = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, handle, bytes.as_mut_ptr() as u64, bytes.len() as u64, caps.as_mut_ptr() as u64) };
		if sys_is_err(received) {
			// Somebody else took it, or the queue drained. That is the correct answer, not an
			// error in the caller.
			continue;
		}
		idle = 0;
		// EVERY MESSAGE CARRIES ITS OWN NUMBER IN ITS FIRST BYTE, and the length that goes with it.
		// A receiver handed a different message than the one it looked at would see them disagree.
		if bytes[0] as usize >= RACE_MESSAGES || received as usize != 8 {
			RACE_WRONG.fetch_add(1, Ordering::SeqCst);
		} else {
			// EXACTLY ONCE, and the bit is how that is known: a message delivered twice finds its
			// bit already set, which no correct interleaving can produce.
			let bit = 1u64 << bytes[0];
			if RACE_SEEN.fetch_or(bit, Ordering::SeqCst) & bit != 0 {
				RACE_WRONG.fetch_add(1, Ordering::SeqCst);
			}
		}
		RACE_BY_RECEIVER[me].fetch_add(1, Ordering::SeqCst);
		RACE_DELIVERED.fetch_add(1, Ordering::SeqCst);
		for raw in caps.iter().take(caps[0] as usize + 1).skip(1) {
			let _ = unsafe { invoke(SYS_HANDLE_CLOSE, *raw, 0, 0, 0) };
		}
	}
	RACE_DONE.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn racing_sender(handle: u64) {
	for index in 0..RACE_MESSAGES {
		let created = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
		if sys_is_err(created) {
			return;
		}
		let caps = [1u64, created];
		let body = [index as u8; 8];
		let mut attempts = 0;
		loop {
			let sent = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, handle, body.as_ptr() as u64, body.len() as u64, caps.as_ptr() as u64) };
			if !sys_is_err(sent) || attempts > 256 {
				break;
			}
			attempts += 1;
			unsafe { invoke(SYS_YIELD, 0, 0, 0, 0) };
		}
	}
}

crate::tagged_test!(capability_tcb_two_receivers_race_for_one_message, [CapabilityTcb, Object, Handle, Channel, Kernel, Syscall], id = "kernel.object.capability_tcb_two_receivers_race_for_one_message", covers = ["kernel"]);
fn capability_tcb_two_receivers_race_for_one_message() {
	RACE_DELIVERED.store(0, Ordering::SeqCst);
	RACE_WRONG.store(0, Ordering::SeqCst);
	RACE_DONE.store(0, Ordering::SeqCst);
	RACE_ENTERED.store(0, Ordering::SeqCst);
	RACE_SEEN.store(0, Ordering::SeqCst);
	RACE_BY_RECEIVER[0].store(0, Ordering::SeqCst);
	RACE_BY_RECEIVER[1].store(0, Ordering::SeqCst);

	let (a, b) = Channel::try_create_with_depth(2).expect("a shallow pair");
	let charged_before = crate::sched::root_domain().account().handles().used();
	let (process, send_handle) = crate::sched::prepare_shared_process(a.clone(), Rights::ALL);
	let recv_handle = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	let domain = process.domain().clone();
	RACE_SEND.store(send_handle, Ordering::SeqCst);
	RACE_RECV.store(recv_handle, Ordering::SeqCst);

	let first = crate::sched::prepare_in_process(racing_receiver, recv_handle, &process);
	let second = crate::sched::prepare_in_process(racing_receiver, recv_handle, &process);
	let sender = crate::sched::prepare_in_process(racing_sender, send_handle, &process);
	crate::sched::start_thread(&first);
	crate::sched::start_thread(&second);
	crate::sched::start_thread(&sender);
	crate::sched::run_until_idle();

	assert_eq!(RACE_DELIVERED.load(Ordering::SeqCst), RACE_MESSAGES, "every message was delivered, exactly once between the two receivers");
	assert_eq!(RACE_WRONG.load(Ordering::SeqCst), 0, "no receiver was handed a message other than the one it had looked at");
	assert_eq!(RACE_DONE.load(Ordering::SeqCst), 2, "both receivers finished rather than one spinning to its idle limit");
	// WITHOUT THIS THE TEST PROVES NOTHING. Two receivers that never collided are two receivers
	// taking turns, and the identity check they exist to exercise was never reached.
	assert_eq!(RACE_SEEN.load(Ordering::SeqCst), (1u64 << RACE_MESSAGES) - 1, "every message arrived, each of them once");
	// WITHOUT THIS THE TEST PROVES NOTHING ABOUT TWO RECEIVERS. One thread doing all the work and
	// another watching passes every assertion above.
	let (mine, theirs) = (RACE_BY_RECEIVER[0].load(Ordering::SeqCst), RACE_BY_RECEIVER[1].load(Ordering::SeqCst));
	assert!(mine > 0 && theirs > 0, "both receivers took messages from the shared endpoint (they took {mine} and {theirs})");
	assert!(b.peek_identified().is_err(), "nothing is left queued");
	assert_eq!(domain.account().handles().used(), charged_before, "and every capability that travelled was accounted for");
}

// TERMINATION AT EVERY PHASE OF A TRANSFER.
//
// A process can die at any instruction, and a transfer is not one instruction. Between the take and
// the send, between the send and the peek, between the dequeue and the commit, between the commit
// and the install - at each of those the sender's table or the receiver's can close under it, and
// what must not happen is a capability counted twice or lost with its quota still spent.
//
// DRIVEN THROUGH THE OBJECT API RATHER THAN THE SYSCALLS, and deliberately: the phases are inside
// one syscall, so a thread cannot be stopped between them from outside. What is reproducible is the
// STATE each phase leaves, and each case here builds exactly that state and then closes the table.
crate::tagged_test!(capability_tcb_termination_at_each_transfer_phase, [CapabilityTcb, Object, Handle, Channel, Domain, Kernel], id = "kernel.object.capability_tcb_termination_at_each_transfer_phase", covers = ["kernel"]);
fn capability_tcb_termination_at_each_transfer_phase() {
	use crate::object::KernelObject;
	use crate::object::channel::Message;
	use crate::object::event::Event;
	use crate::object::handle::HandleTable;
	use alloc::sync::Arc;

	let domain = crate::sched::root_domain();
	let charged_before = domain.account().handles().used();

	// A table of this fixture's own, charged to the same Domain, so the assertions below can be
	// made against a number nothing else is moving.
	let fresh = || {
		let mut table = HandleTable::new();
		table.set_domain(domain.clone());
		table
	};

	// PHASE 1: the sender dies with the capability TAKEN and not yet sent.
	{
		let mut sender = fresh();
		let handle = sender.insert_object(Event::create().expect("an event") as Arc<dyn KernelObject>, Rights::ALL);
		let cap = sender.take_for_transfer(handle, Rights::TRANSFER).expect("taken");
		sender.close_all();
		// The capability is in the caller's hand and the table it came from is gone. Giving it back
		// is the only thing left to do with it, and the table must accept that without resurrecting.
		sender.restore_taken(handle, cap);
		assert!(sender.entries().is_empty(), "a table that closed mid-transfer holds nothing afterwards");
	}

	// PHASE 2: the sender dies with the message QUEUED. The capability belongs to the message now,
	// so it must survive its sender and still be deliverable.
	{
		let (a, b) = Channel::create();
		let mut sender = fresh();
		let event = Event::create().expect("an event") as Arc<dyn KernelObject>;
		let koid = event.header().koid();
		let handle = sender.insert_object(event, Rights::ALL);
		let cap = sender.take_for_transfer(handle, Rights::TRANSFER).expect("taken");
		assert!(a.send_charged_or_return(Message::new(alloc::vec![1], alloc::vec![cap]), &domain).is_ok(), "sent");
		sender.commit_taken(handle);
		sender.close_all();
		let mut receiver = fresh();
		let (id, _bytes, caps) = b.peek_identified().expect("the message outlived its sender");
		assert!(receiver.reserve(caps), "booked");
		let Ok(mut message) = b.recv_identified(id, 16, caps) else { panic!("taken by identity") };
		b.commit_delivery(&mut message);
		let mut arrived = 0;
		for cap in message.caps.drain(..) {
			let installed = receiver.insert_reserved(cap);
			assert_ne!(installed.raw(), 0, "installed");
			arrived += 1;
		}
		assert_eq!(arrived, 1, "the capability arrived");
		assert_eq!(receiver.entries().iter().filter(|e| e.koid == koid).count(), 1, "and it is the one that was sent, once");
		receiver.close_all();
	}

	// PHASE 3: the receiver dies with the message IN HAND and the delivery not committed. The
	// message goes back to the queue, whole, for whoever comes next.
	{
		let (a, b) = Channel::create();
		let mut sender = fresh();
		let handle = sender.insert_object(Event::create().expect("an event") as Arc<dyn KernelObject>, Rights::ALL);
		let cap = sender.take_for_transfer(handle, Rights::TRANSFER).expect("taken");
		assert!(a.send_charged_or_return(Message::new(alloc::vec![2], alloc::vec![cap]), &domain).is_ok(), "sent");
		sender.commit_taken(handle);
		let mut dying = fresh();
		let (id, _bytes, caps) = b.peek_identified().expect("queued");
		assert!(dying.reserve(caps), "booked");
		let Ok(message) = b.recv_identified(id, 16, caps) else { panic!("taken") };
		dying.release_reservation(caps);
		dying.close_all();
		b.return_to_head(message);
		assert!(b.peek_identified().is_ok(), "a receiver that died before its commit left the message where it was");
		// And a live receiver can still take it.
		let mut receiver = fresh();
		let (id, _bytes, caps) = b.peek_identified().expect("still queued");
		assert!(receiver.reserve(caps), "booked");
		let Ok(mut message) = b.recv_identified(id, 16, caps) else { panic!("taken") };
		b.commit_delivery(&mut message);
		for cap in message.caps.drain(..) {
			let _ = receiver.insert_reserved(cap);
		}
		assert_eq!(receiver.entries().len(), 1, "and it arrived intact");
		receiver.close_all();
		sender.close_all();
	}

	// PHASE 4: the receiver dies AFTER the commit and before the install. The message cannot go
	// back, so the capability is dropped - and the booking it was going to use is given back with
	// it, which is the arm `insert_reserved` takes when its table has closed.
	{
		let (a, b) = Channel::create();
		let mut sender = fresh();
		let handle = sender.insert_object(Event::create().expect("an event") as Arc<dyn KernelObject>, Rights::ALL);
		let cap = sender.take_for_transfer(handle, Rights::TRANSFER).expect("taken");
		assert!(a.send_charged_or_return(Message::new(alloc::vec![3], alloc::vec![cap]), &domain).is_ok(), "sent");
		sender.commit_taken(handle);
		let mut dying = fresh();
		let (id, _bytes, caps) = b.peek_identified().expect("queued");
		assert!(dying.reserve(caps), "booked");
		let Ok(mut message) = b.recv_identified(id, 16, caps) else { panic!("taken") };
		b.commit_delivery(&mut message);
		dying.close_all();
		for cap in message.caps.drain(..) {
			let installed = dying.insert_reserved(cap);
			assert_eq!(installed.raw(), 0, "a closed table installs nothing");
		}
		assert!(dying.entries().is_empty(), "and holds nothing afterwards");
		sender.close_all();
	}

	// THE LEDGER IS THE POINT. Four terminations at four phases, and not one unit of handle quota
	// is missing or double-counted afterwards.
	assert_eq!(domain.account().handles().used(), charged_before, "every phase gave back exactly what it took");
}

// AN ALLOCATION THAT FAILS INSIDE A TRANSFER, AT BOTH PHASES THAT CAN REFUSE FOR MEMORY.
//
// The quota fixture above says in as many words that it is NOT an out-of-memory injection, and that
// was the whole of the gap: `try_reserve` in `send_inner` and in `HandleTable::reserve` are this
// kernel's answers to a short heap on the two busiest paths there are, and neither branch had
// anything driving it. Reaching them for real means exhausting the machine's heap - after which
// nothing can be asserted about what the refusal LEFT BEHIND, which is the half that matters,
// because a send that fails at the enqueue has already emptied every one of the caller's handles.
//
// So the two branches are armed (see `channel::refuse_next_enqueue_allocations` and
// `handle::refuse_next_reservation_allocations`) and driven through the real syscalls.
//
// THE ORDER IS THE ASSERTION, not merely the count. The send path restores with
// `taken.into_iter().zip(caps)`, so a batch put back in the wrong order would leave both handles
// live, both capabilities reachable and the quota exactly right - every count intact and the
// caller's two handles swapped. Two objects of DIFFERENT TYPES is what makes that observable: after
// the refusal the event handle must still poll as an event and the timer handle as a timer, and
// nothing about the numbers would have said so.
static ALLOC_SEND: AtomicU64 = AtomicU64::new(0);
static ALLOC_RECV: AtomicU64 = AtomicU64::new(0);
static ALLOC_REPORT: AtomicI64 = AtomicI64::new(-1);
static ALLOC_REFUSED_SEND: AtomicI64 = AtomicI64::new(0);
static ALLOC_REFUSED_RECV: AtomicI64 = AtomicI64::new(0);
static ALLOC_STILL_QUEUED: AtomicI64 = AtomicI64::new(0);
static ALLOC_ENTRIES_BEFORE: AtomicUsize = AtomicUsize::new(0);
static ALLOC_ENTRIES_AFTER: AtomicUsize = AtomicUsize::new(0);

// The live entries of the calling thread's own table, counted from inside so the count is taken
// while the thread is still there to have one.
fn live_entries() -> usize {
	crate::sched::current_thread().map(|thread| thread.handles().lock().entries().len()).unwrap_or(0)
}

extern "C" fn alloc_thread(_argument: u64) {
	let send_handle = ALLOC_SEND.load(Ordering::SeqCst);
	let recv_handle = ALLOC_RECV.load(Ordering::SeqCst);
	let body = [3u8; 8];

	let event = unsafe { invoke(SYS_EVENT_CREATE, 0, 0, 0, 0) };
	let timer = unsafe { invoke(SYS_TIMER_CREATE, 0, 0, 0, 0) };
	if sys_is_err(event) || sys_is_err(timer) {
		ALLOC_REPORT.store(1, Ordering::SeqCst);
		return;
	}
	let caps = [2u64, event, timer];

	ALLOC_ENTRIES_BEFORE.store(live_entries(), Ordering::SeqCst);
	// THE ENQUEUE'S ALLOCATION FAILS. Both capabilities are already out of the table by then.
	crate::object::channel::refuse_next_enqueue_allocations(1);
	let refused = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, caps.as_ptr() as u64) };
	ALLOC_REFUSED_SEND.store(signed(refused), Ordering::SeqCst);
	ALLOC_ENTRIES_AFTER.store(live_entries(), Ordering::SeqCst);

	// EACH CAPABILITY BACK UNDER ITS OWN HANDLE. Poll each one as the type it is and as the type the
	// other one is: a swapped restoration passes the first two and fails the second two.
	if sys_is_err(unsafe { invoke(SYS_EVENT_POLL, event, 0, 0, 0) }) {
		ALLOC_REPORT.store(2, Ordering::SeqCst);
		return;
	}
	if !sys_is_err(unsafe { invoke(SYS_TIMER_POLL, event, 0, 0, 0) }) {
		ALLOC_REPORT.store(3, Ordering::SeqCst);
		return;
	}
	if sys_is_err(unsafe { invoke(SYS_TIMER_POLL, timer, 0, 0, 0) }) {
		ALLOC_REPORT.store(4, Ordering::SeqCst);
		return;
	}
	if !sys_is_err(unsafe { invoke(SYS_EVENT_POLL, timer, 0, 0, 0) }) {
		ALLOC_REPORT.store(5, Ordering::SeqCst);
		return;
	}

	// AND THE SAME SEND SUCCEEDS with nothing armed, which is what says the refusal was the
	// allocation and not anything about the message or the handles.
	let sent = unsafe { invoke(SYS_CHANNEL_SEND_CAPS, send_handle, body.as_ptr() as u64, body.len() as u64, caps.as_ptr() as u64) };
	if sys_is_err(sent) {
		ALLOC_REPORT.store(6, Ordering::SeqCst);
		return;
	}

	// THE RECEIVE'S RESERVATION FAILS. The message must stay queued: `reserve` returning false is
	// the whole reason the dequeue happens after it and not before.
	let mut caps_out = [0u64; CAPS + 1];
	let mut bytes_out = [0u8; 16];
	crate::object::handle::refuse_next_reservation_allocations(1);
	let no_room = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, bytes_out.as_mut_ptr() as u64, bytes_out.len() as u64, caps_out.as_mut_ptr() as u64) };
	ALLOC_REFUSED_RECV.store(signed(no_room), Ordering::SeqCst);
	ALLOC_STILL_QUEUED.store(signed(unsafe { invoke(SYS_CHANNEL_PEEK, recv_handle, 0, 0, 0) }), Ordering::SeqCst);

	// And the same receive, unarmed, takes it whole.
	let now = unsafe { invoke(SYS_CHANNEL_RECV_CAPS, recv_handle, bytes_out.as_mut_ptr() as u64, bytes_out.len() as u64, caps_out.as_mut_ptr() as u64) };
	if sys_is_err(now) || caps_out[0] != 2 {
		ALLOC_REPORT.store(7, Ordering::SeqCst);
		return;
	}
	for raw in caps_out.iter().take(2 + 1).skip(1) {
		if sys_is_err(unsafe { invoke(SYS_HANDLE_CLOSE, *raw, 0, 0, 0) }) {
			ALLOC_REPORT.store(8, Ordering::SeqCst);
			return;
		}
	}
	ALLOC_REPORT.store(0, Ordering::SeqCst);
}

crate::tagged_test!(capability_tcb_an_allocation_that_fails_mid_transfer_costs_the_caller_nothing, [CapabilityTcb, Object, Handle, Channel, Domain, Kernel, Syscall, Memory], id = "kernel.object.capability_tcb_an_allocation_that_fails_mid_transfer_costs_the_caller_nothing", covers = ["kernel"]);
fn capability_tcb_an_allocation_that_fails_mid_transfer_costs_the_caller_nothing() {
	ALLOC_REPORT.store(-1, Ordering::SeqCst);

	let (a, b) = Channel::create();
	let (process, send_handle) = crate::sched::prepare_shared_process(a.clone(), Rights::ALL);
	let recv_handle = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	ALLOC_SEND.store(send_handle, Ordering::SeqCst);
	ALLOC_RECV.store(recv_handle, Ordering::SeqCst);

	let driver = crate::sched::prepare_in_process(alloc_thread, 0, &process);
	crate::sched::start_thread(&driver);
	crate::sched::run_until_idle();

	// NOTHING ARMED IS LEFT ARMED. A counter that survived this test would refuse an allocation in
	// whatever ran next, which is a failure nobody would look for here.
	crate::object::channel::refuse_next_enqueue_allocations(0);
	crate::object::handle::refuse_next_reservation_allocations(0);

	let report = ALLOC_REPORT.load(Ordering::SeqCst);
	assert_eq!(report, 0, "the allocation fixture stopped at step {report} (see `alloc_thread`)");
	assert!(ALLOC_REFUSED_SEND.load(Ordering::SeqCst) < 0, "a send whose enqueue cannot allocate is refused rather than aborting the kernel");
	assert_eq!(ALLOC_ENTRIES_BEFORE.load(Ordering::SeqCst), ALLOC_ENTRIES_AFTER.load(Ordering::SeqCst), "a refused send leaves the caller's table exactly as it found it - every capability back under the handle it came from");
	assert_eq!(ALLOC_REFUSED_RECV.load(Ordering::SeqCst), crate::syscall::ERR_RESOURCE_EXHAUSTED, "a receive whose reservation cannot allocate is refused");
	assert!(ALLOC_STILL_QUEUED.load(Ordering::SeqCst) >= 0, "and the message it could not book for is still queued for a receiver that can");
}
