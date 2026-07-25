use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use super::super::handle::Capability;
use super::super::rights::Rights;
use super::{Channel, ChannelError, Message};
use crate::{arch, sched, syscall};

crate::tagged_test!(channel_message_and_capability_transfer, [Channel, Ipc]);
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
	sched::spawn_with_object(sender, sender_end, Rights::ALL, 0);
	sched::spawn_with_object(receiver, receiver_end, Rights::ALL, 0);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(MARKER.load(Ordering::SeqCst), 0x5151_5151);
}

crate::tagged_test!(a_sender_on_a_full_channel_blocks_and_wakes_on_drain, [Channel, Ipc]);
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
	let (sender_end, receiver_end) = Channel::create_with_depth(2);
	sched::spawn_with_object(sender, sender_end, Rights::ALL, 0);
	sched::run_until_idle();
	assert!(SENDER_REFUSED.load(Ordering::SeqCst), "the depth-2 queue refused the third send");
	sched::spawn_with_object(receiver, receiver_end, Rights::ALL, 0);
	sched::run_until_idle();
	assert!(SENDER_DONE.load(Ordering::SeqCst), "the drain woke the blocked sender");
	assert_eq!(RECEIVED.load(Ordering::SeqCst), 3, "every message was delivered");
}

crate::tagged_test!(channel_endpoint_semantics, [Channel, Ipc]);
fn channel_endpoint_semantics() {
	let (sender, receiver) = Channel::create();
	assert!(matches!(receiver.recv(), Err(ChannelError::Empty)));
	sender.send(Message::new(alloc::vec![1, 2, 3], alloc::vec::Vec::new(), 0x99)).unwrap();
	let message = receiver.recv().unwrap();
	assert_eq!(message.bytes, alloc::vec![1, 2, 3]);
	assert_eq!(message.badge, 0x99);
	drop(sender);
	assert!(receiver.is_peer_closed());
	assert!(matches!(receiver.recv(), Err(ChannelError::PeerClosed)));
}

crate::tagged_test!(channel_peek_reports_the_pending_length, [Channel, Ipc]);
fn channel_peek_reports_the_pending_length() {
	let (sender, receiver) = Channel::create();
	assert!(matches!(receiver.peek_len(), Err(ChannelError::Empty)));
	let big: alloc::vec::Vec<u8> = (0..20_000u32).map(|value| value as u8).collect();
	sender.send(Message::new(big.clone(), alloc::vec::Vec::new(), 0)).unwrap();
	sender.send(Message::new(alloc::vec![7u8; 3], alloc::vec::Vec::new(), 0)).unwrap();
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

crate::tagged_test!(blocking_wait_wakes_on_message, [Channel, Ipc]);
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
	sched::spawn_with_object(server, server_end, Rights::ALL, 0);
	sched::spawn_with_object(client, client_end, Rights::ALL, 0);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 0);
}

crate::tagged_test!(wait_any_wakes_on_the_ready_handle, [Channel, Ipc]);
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
	let server = sched::spawn_with_object(server, first_server_end, Rights::ALL, 0);
	let second_handle = server.handles().lock().insert(Capability::new(second_server_end, Rights::ALL, 0)).raw();
	SECOND_HANDLE.store(second_handle, Ordering::SeqCst);
	sched::spawn_with_object(client, second_client_end, Rights::ALL, 0);
	// Hold the first channel's peer open so that handle stays silent; otherwise its
	// peer-close would make it ready and wait_any could return 0.
	let _keep_first_client_end = first_client_end;
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 1);
	assert!(OK.load(Ordering::SeqCst));
}

crate::tagged_test!(channel_round_trip_delivers_request_and_reply, [Channel, Ipc]);
fn channel_round_trip_delivers_request_and_reply() {
	// A request and a reply each deliver their exact bytes through the channel
	// primitive, the path the latency benchmark times.
	let (client, server) = Channel::create();
	client.send(Message::new(alloc::vec::Vec::from(*b"req"), alloc::vec::Vec::new(), 0)).unwrap();
	let request = server.recv().unwrap();
	assert_eq!(&request.bytes[..], b"req");
	server.send(Message::new(alloc::vec::Vec::from(*b"reply"), alloc::vec::Vec::new(), 0)).unwrap();
	let reply = client.recv().unwrap();
	assert_eq!(&reply.bytes[..], b"reply");
}

crate::tagged_test!(channel_queue_bytes_accounting_is_refunded_on_recv, [Channel, Domain, Ipc, Kernel, Syscall]);
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
