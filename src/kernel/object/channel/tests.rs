use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use super::super::handle::Capability;
use super::super::rights::Rights;
use super::{Channel, ChannelError, Message};
use crate::{arch, sched, syscall};

crate::tagged_test!(channel_message_and_capability_transfer, [Channel, Ipc], id = "kernel.object.channel.channel_message_and_capability_transfer", covers = ["kernel"]);
fn channel_message_and_capability_transfer() {
	static OK: AtomicBool = AtomicBool::new(false);
	static MARKER: AtomicU64 = AtomicU64::new(0);
	extern "C" fn sender(channel: u64) {
		unsafe {
			let memory = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			let mapped = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, memory, 0, 0, 0);
			(mapped as *mut u64).write_volatile(0x5151_5151);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, memory, 0, 0, 0);
			let payload = *b"hi";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, channel, payload.as_ptr() as u64, payload.len() as u64, memory);
			assert!(!syscall::sys_is_err(sent));
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, memory, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE);
		}
	}
	extern "C" fn receiver(channel: u64) {
		unsafe {
			let mut buf = [0u8; 8];
			let mut transferred = 0u64;
			let length = loop {
				let length = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, channel, buf.as_mut_ptr() as u64, buf.len() as u64, &mut transferred as *mut u64 as u64);
				if !syscall::sys_is_err(length) {
					break length;
				}
				assert_eq!(length as i64, syscall::ERR_WOULD_BLOCK);
				sched::yield_now();
			};
			assert_eq!(&buf[..length as usize], b"hi");
			assert_ne!(transferred, 0);
			let mapped = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, transferred, 0, 0, 0);
			MARKER.store((mapped as *const u64).read_volatile(), Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, transferred, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, transferred, 0, 0, 0);
			OK.store(true, Ordering::SeqCst);
		}
	}
	let (sender_end, receiver_end) = Channel::create();
	sched::spawn_with_object(sender, sender_end, Rights::ALL);
	sched::spawn_with_object(receiver, receiver_end, Rights::ALL);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(MARKER.load(Ordering::SeqCst), 0x5151_5151);
}

crate::tagged_test!(a_sender_on_a_full_channel_blocks_and_wakes_on_drain, [Channel, Ipc], id = "kernel.object.channel.a_sender_on_a_full_channel_blocks_and_wakes_on_drain", covers = ["kernel"]);
fn a_sender_on_a_full_channel_blocks_and_wakes_on_drain() {
	static SENDER_REFUSED: AtomicBool = AtomicBool::new(false);
	static SENDER_DONE: AtomicBool = AtomicBool::new(false);
	static RECEIVED: AtomicU64 = AtomicU64::new(0);
	extern "C" fn sender(channel: u64) {
		unsafe {
			for message in [b"m1", b"m2", b"m3"] {
				loop {
					let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, channel, message.as_ptr() as u64, message.len() as u64, 0);
					if sent as i64 == syscall::ERR_WOULD_BLOCK {
						SENDER_REFUSED.store(true, Ordering::SeqCst);
						let ready = arch::syscall::invoke(syscall::SYS_WAIT, channel, 0, abi::WAIT_WRITABLE, 0);
						assert_eq!(ready as i64, 0, "the writable wait returns ready");
						continue;
					}
					assert!(!syscall::sys_is_err(sent));
					break;
				}
			}
			SENDER_DONE.store(true, Ordering::SeqCst);
		}
	}
	extern "C" fn receiver(channel: u64) {
		unsafe {
			let mut buf = [0u8; 8];
			let mut transferred = 0u64;
			while RECEIVED.load(Ordering::SeqCst) < 3 {
				let length = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, channel, buf.as_mut_ptr() as u64, buf.len() as u64, &mut transferred as *mut u64 as u64);
				if length as i64 == syscall::ERR_WOULD_BLOCK {
					arch::syscall::invoke(syscall::SYS_WAIT, channel, 0, 0, 0);
					continue;
				}
				assert!(!syscall::sys_is_err(length), "recv failed");
				RECEIVED.fetch_add(1, Ordering::SeqCst);
			}
		}
	}
	let (sender_end, receiver_end) = Channel::try_create_with_depth(2).expect("a channel pair");
	sched::spawn_with_object(sender, sender_end, Rights::ALL);
	sched::run_until_idle();
	assert!(SENDER_REFUSED.load(Ordering::SeqCst), "the depth-2 queue refused the third send");
	sched::spawn_with_object(receiver, receiver_end, Rights::ALL);
	sched::run_until_idle();
	assert!(SENDER_DONE.load(Ordering::SeqCst), "the drain woke the blocked sender");
	assert_eq!(RECEIVED.load(Ordering::SeqCst), 3, "every message was delivered");
}

crate::tagged_test!(channel_endpoint_semantics, [Channel, Ipc], id = "kernel.object.channel.channel_endpoint_semantics", covers = ["kernel"]);
fn channel_endpoint_semantics() {
	let (sender, receiver) = Channel::create();
	assert!(matches!(receiver.recv(), Err(ChannelError::Empty)));
	sender.send(Message::new(alloc::vec![1, 2, 3], alloc::vec::Vec::new())).unwrap();
	let message = receiver.recv().unwrap();
	assert_eq!(message.bytes, alloc::vec![1, 2, 3]);
	drop(sender);
	assert!(receiver.is_peer_closed());
	assert!(matches!(receiver.recv(), Err(ChannelError::PeerClosed)));
}

crate::tagged_test!(channel_peek_reports_the_pending_length, [Channel, Ipc], id = "kernel.object.channel.channel_peek_reports_the_pending_length", covers = ["kernel"]);
fn channel_peek_reports_the_pending_length() {
	let (sender, receiver) = Channel::create();
	assert!(matches!(receiver.peek_len(), Err(ChannelError::Empty)));
	let big: alloc::vec::Vec<u8> = (0..20_000u32).map(|value| value as u8).collect();
	sender.send(Message::new(big.clone(), alloc::vec::Vec::new())).unwrap();
	sender.send(Message::new(alloc::vec![7u8; 3], alloc::vec::Vec::new())).unwrap();
	assert_eq!(receiver.peek_len().unwrap(), 20_000);
	assert_eq!(receiver.peek_len().unwrap(), 20_000, "peek does not dequeue");
	let first = receiver.recv().unwrap();
	assert_eq!(first.bytes, big, "the exactly-sized recv loses nothing");
	assert_eq!(receiver.peek_len().unwrap(), 3, "the next message's length follows");
	let _ = receiver.recv().unwrap();
	assert!(matches!(receiver.peek_len(), Err(ChannelError::Empty)));
	drop(sender);
	assert!(matches!(receiver.peek_len(), Err(ChannelError::PeerClosed)));
}

crate::tagged_test!(blocking_wait_wakes_on_message, [Channel, Ipc], id = "kernel.object.channel.blocking_wait_wakes_on_message", covers = ["kernel"]);
fn blocking_wait_wakes_on_message() {
	static OK: AtomicBool = AtomicBool::new(false);
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	// The server blocks in SYS_WAIT on its empty channel. The client then sends,
	// waking the server; it returns from wait and receives the message.
	extern "C" fn server(channel: u64) {
		unsafe {
			let result = arch::syscall::invoke(syscall::SYS_WAIT, channel, 0, 0, 0);
			WAIT_RET.store(result as i64, Ordering::SeqCst);
			let mut buffer = [0u8; 8];
			let mut transferred = 0u64;
			let length = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, channel, buffer.as_mut_ptr() as u64, buffer.len() as u64, &mut transferred as *mut u64 as u64);
			assert!(!syscall::sys_is_err(length));
			assert_eq!(&buffer[..length as usize], b"ping");
			OK.store(true, Ordering::SeqCst);
		}
	}
	extern "C" fn client(channel: u64) {
		unsafe {
			let payload = *b"ping";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, channel, payload.as_ptr() as u64, payload.len() as u64, 0);
			assert!(!syscall::sys_is_err(sent));
		}
	}
	let (server_end, client_end) = Channel::create();
	sched::spawn_with_object(server, server_end, Rights::ALL);
	sched::spawn_with_object(client, client_end, Rights::ALL);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 0);
}

crate::tagged_test!(wait_any_wakes_on_the_ready_handle, [Channel, Ipc], id = "kernel.object.channel.wait_any_wakes_on_the_ready_handle", covers = ["kernel"]);
fn wait_any_wakes_on_the_ready_handle() {
	static SECOND_HANDLE: AtomicU64 = AtomicU64::new(0);
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	static OK: AtomicBool = AtomicBool::new(false);
	// The server blocks in SYS_WAIT_ANY on two channels. Only the second receives a
	// message, so wait_any must return index 1 and remove the waiter's other entry.
	extern "C" fn server(first_handle: u64) {
		unsafe {
			let second_handle = SECOND_HANDLE.load(Ordering::SeqCst);
			let handles = [first_handle, second_handle];
			let result = arch::syscall::invoke(syscall::SYS_WAIT_ANY, handles.as_ptr() as u64, handles.len() as u64, 0, 0);
			WAIT_RET.store(result as i64, Ordering::SeqCst);
			let mut buffer = [0u8; 8];
			let mut transferred = 0u64;
			let length = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, second_handle, buffer.as_mut_ptr() as u64, buffer.len() as u64, &mut transferred as *mut u64 as u64);
			OK.store(!syscall::sys_is_err(length) && &buffer[..length as usize] == b"pong", Ordering::SeqCst);
		}
	}
	extern "C" fn client(channel: u64) {
		unsafe {
			let payload = *b"pong";
			let _ = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, channel, payload.as_ptr() as u64, payload.len() as u64, 0);
		}
	}
	let (first_server_end, first_client_end) = Channel::create();
	let (second_server_end, second_client_end) = Channel::create();
	// Spawn the server with the first channel, then install the second channel as a
	// second handle and record it for the server.
	let server = sched::spawn_with_object(server, first_server_end, Rights::ALL);
	let second_handle = server.handles().lock().insert(Capability::new(second_server_end, Rights::ALL)).raw();
	SECOND_HANDLE.store(second_handle, Ordering::SeqCst);
	sched::spawn_with_object(client, second_client_end, Rights::ALL);
	// Hold the first channel's peer open so that handle stays silent; otherwise its
	// peer-close would make it ready and wait_any could return 0.
	let _keep_first_client_end = first_client_end;
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 1);
	assert!(OK.load(Ordering::SeqCst));
}

crate::tagged_test!(channel_round_trip_delivers_request_and_reply, [Channel, Ipc], id = "kernel.object.channel.channel_round_trip_delivers_request_and_reply", covers = ["kernel"]);
fn channel_round_trip_delivers_request_and_reply() {
	// A request and a reply each deliver their exact bytes through the channel
	// primitive, the path the latency benchmark times.
	let (client, server) = Channel::create();
	client.send(Message::new(alloc::vec::Vec::from(*b"req"), alloc::vec::Vec::new())).unwrap();
	let request = server.recv().unwrap();
	assert_eq!(&request.bytes[..], b"req");
	server.send(Message::new(alloc::vec::Vec::from(*b"reply"), alloc::vec::Vec::new())).unwrap();
	let reply = client.recv().unwrap();
	assert_eq!(&reply.bytes[..], b"reply");
}

crate::tagged_test!(channel_queue_bytes_accounting_is_refunded_on_recv, [Channel, Domain, Ipc, Kernel, Syscall], id = "kernel.object.channel.channel_queue_bytes_accounting_is_refunded_on_recv", covers = ["kernel"]);
fn channel_queue_bytes_accounting_is_refunded_on_recv() {
	use crate::object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread in a Domain capped at 250 bytes of in-transit IPC. Each 100-byte
	// send charges the sender's queue, so two fit and the third is refused.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut handles = [0u64; 2];
			let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, handles.as_mut_ptr() as u64, handles.as_mut_ptr().add(1) as u64, 0, 0);
			assert_eq!(created as i64, 0, "channel create failed");
			let (sender, receiver) = (handles[0], handles[1]);
			let payload = [0u8; 100];
			let payload_ptr = payload.as_ptr() as u64;
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, sender, payload_ptr, 100, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, sender, payload_ptr, 100, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, sender, payload_ptr, 100, 0) as i64, syscall::ERR_WOULD_BLOCK, "the third send should hit the queue cap");
			// Receiving one message refunds the sender's queue, so a send fits again.
			let mut buffer = [0u8; 128];
			let length = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, receiver, buffer.as_mut_ptr() as u64, buffer.len() as u64, 0);
			assert_eq!(length as i64, 100);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, sender, payload_ptr, 100, 0) as i64, 0, "a send fits after a recv refund");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	domain.account().ipc_queue().set_limit(250);
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "channel queue test thread did not finish");
	// The thread and its channels are reaped, refunding every undelivered message.
	assert_eq!(domain.account().ipc_queue().used(), 0);
}

crate::tagged_test!(receives_in_flight_never_let_the_queue_pass_its_limit, [Channel, Ipc, Kernel], id = "kernel.object.channel.receives_in_flight_never_let_the_queue_pass_its_limit", covers = ["kernel"]);
fn receives_in_flight_never_let_the_queue_pass_its_limit() {
	// N receivers whose copies to userspace fail, against one endpoint, with a sender running.
	//
	// The queue bound is what stops a fast sender from turning a slow reader into unbounded kernel
	// memory. A receive takes its message off the queue BEFORE it knows whether it can deliver it,
	// and puts it back if it cannot - so between the take and the put-back the queue looks like it
	// has room it does not have. A sender that takes that room means the returned messages arrive
	// on top of it, and the endpoint ends up holding more than its limit: one message deeper per
	// failed copy, forever, because a receiver that keeps failing is a receiver passing a bad
	// buffer - which userspace decides.
	//
	// EXERCISED THROUGH THE SYSCALL'S OWN SEQUENCE - `peek_identified`, `recv_identified`, then
	// either `return_to_head` or the commit - with no test-only entry point, because a bound that
	// only holds against a helper written next to the test is not a bound. The interleaving is
	// deterministic rather than raced for: N concurrent receivers can produce this, and N receivers
	// stopped between their take and their put-back always do.
	use crate::object::channel::Channel;
	const DEPTH: usize = 4;
	let (a, b) = Channel::try_create_with_depth(DEPTH).expect("a channel pair");
	let limit = b.queue_limit();
	assert_eq!(limit, DEPTH, "the endpoint has the depth it was created with");

	let fill = |n: usize| {
		let mut sent = 0;
		while sent < n && a.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).is_ok() {
			sent += 1;
		}
		sent
	};
	assert_eq!(fill(limit), limit, "the queue takes messages up to its limit");
	assert_eq!(fill(1), 0, "and refuses the next one");

	// Three rounds, because one message over the limit is a race and a round that grows the queue
	// every time is the actual defect: the depth after each round is the number to watch.
	for round in 1..=3 {
		// Every receiver gets as far as having its message in hand - the point at which the copy
		// to userspace is about to fail.
		let mut in_flight = alloc::vec::Vec::new();
		while let Ok((id, _, _)) = b.peek_identified() {
			match b.recv_identified(id, usize::MAX, abi::MAX_MESSAGE_CAPS) {
				Ok(message) => in_flight.push(message),
				Err(_) => break,
			}
		}
		assert!(!in_flight.is_empty(), "round {round}: there were messages to take");

		// A sender runs while they are in flight. Whatever it is allowed to queue is room the
		// endpoint promised it had.
		let accepted = fill(limit);

		// And now every copy fails, so every message goes back where it came from.
		let returned = in_flight.len();
		for message in in_flight {
			b.return_to_head(message);
		}

		assert!(b.queued() <= limit, "round {round}: the queue is {} deep against a limit of {limit} ({returned} receives in flight, {accepted} sends accepted behind them)", b.queued());
		assert_eq!(b.in_flight(), 0, "round {round}: every receive ended, so no slot is still held");
	}

	// And a receive that SUCCEEDS gives the slot back too - otherwise the bound would drift shut
	// over a machine's uptime instead of open, which is the same defect wearing the other face.
	let (id, _, _) = b.peek_identified().expect("the queue is not empty");
	let Ok(mut delivered) = b.recv_identified(id, usize::MAX, abi::MAX_MESSAGE_CAPS) else {
		panic!("the head is takeable");
	};
	assert_eq!(b.in_flight(), 1, "a receive in flight holds its slot");
	b.commit_delivery(&mut delivered);
	assert_eq!(b.in_flight(), 0, "a committed delivery gives it back");
	assert_eq!(b.queued(), limit - 1, "and the message really left");
	assert_eq!(fill(1), 1, "the freed slot is usable");
	assert_eq!(fill(1), 0, "and it was exactly one");
}
