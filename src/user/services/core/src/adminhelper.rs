// adminhelper - the administrative-path gate's second process. DEVELOPMENT-ONLY.
//
// It holds an `admin-request` connection of its own, for contention, and AdminService's test controls, to
// release acknowledgments another requester had held. What it is handed down stdin is another launch's
// request connection and grant: holding them is not owning them, which is what it is here to show.
//
//   adminhelper contend        asks while another request holds the confirmation: declined, nobody asked
//   adminhelper redeem         takes a delegated connection and grant, waits for their owner to end, and
//                              redeems: refused, and nothing is dispatched
//   adminhelper redeem-live    takes them while their owner lives, and redeems: one attempt, completed
//   adminhelper release        waits for the requester before it to end, then releases the held acknowledgments

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AdminAction, AdminAnswer, AdminJournalFault, AdminRequestArgs, AdminResult, LaunchContext, admin_authority, admin_request, admin_test};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;

fn say(line: &[u8]) {
	print(b"adminhelper: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"adminhelper: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

// What the requester before this one sent down the pipe: its request connection and its grant.
fn delegated() -> (u64, u64) {
	let mut buf = [0u8; 64];
	let until = clock() + 180 * TICKS;
	loop {
		if wait(stdin(), until) < 0 {
			fail(b"nothing was delegated");
		}
		match try_recv_caps(stdin(), &mut buf) {
			PolledCaps::Message { handles, .. } if handles.as_slice().len() == 2 => return (handles.as_slice()[0], handles.as_slice()[1]),
			PolledCaps::Message { handles, .. } => {
				for &handle in handles.as_slice() {
					close(handle);
				}
			}
			PolledCaps::Empty => {}
			PolledCaps::Closed => fail(b"the requester ended without delegating anything"),
		}
	}
}

// The requester before this one has ended: its end of the pipe is gone.
fn await_end() {
	let mut buf = [0u8; 64];
	let until = clock() + 180 * TICKS;
	loop {
		if wait(stdin(), until) < 0 {
			fail(b"the requester before this one never ended");
		}
		match try_recv_caps(stdin(), &mut buf) {
			PolledCaps::Closed => return,
			PolledCaps::Message { handles, .. } => {
				for &handle in handles.as_slice() {
					close(handle);
				}
			}
			PolledCaps::Empty => {}
		}
	}
}

fn execute(grant: u64) -> Option<Result<AdminResult, proto::system::Error>> {
	admin_authority::Client::with_deadline(ChannelTransport { chan: grant }, clock() + 10 * TICKS).execute()
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let own = recv_tagged(bootstrap, &mut buf, b"ADMINREQUEST").unwrap_or(0);
	let test = recv_tagged(bootstrap, &mut buf, CAP_ADMIN_TEST).unwrap_or(0);
	if own == 0 || test == 0 {
		fail(b"a grant this helper needs was not delivered");
	}
	match context.arguments.as_str() {
		"contend" => {
			let payload = memory_object_create(4096);
			if payload < 0 {
				fail(b"no payload");
			}
			let args = AdminRequestArgs { action: AdminAction::ProbeWrite, target: String::from("probe-target"), parameters: Vec::new(), payload_length: 16, label: String::from("a competing request") };
			match admin_request::Client::with_deadline(ChannelTransport { chan: own }, clock() + 30 * TICKS).request(&args, &(payload as u64)) {
				Some(Ok(AdminAnswer::Declined)) => say(b"PASS contend: a competing request was declined, not queued"),
				Some(Ok(AdminAnswer::Granted(_))) => fail(b"contend: a competing request was granted"),
				_ => fail(b"contend: the request failed"),
			}
		}
		"redeem" => {
			let (connection, grant) = delegated();
			if object_info(connection).is_none() || object_info(grant).is_none() {
				fail(b"redeem: the delegated endpoints are not held");
			}
			say(b"holds the delegated request connection and grant");
			// THE OWNER ENDS, and then the grant is tried - from a process that still holds both endpoints.
			await_end();
			say(b"the requester that owned them has ended");
			if object_info(connection).is_none() || object_info(grant).is_none() {
				fail(b"redeem: the endpoints were taken away rather than refused");
			}
			if matches!(execute(grant), Some(Ok(_))) {
				fail(b"redeem: a grant whose owner ended was redeemed");
			}
			say(b"PASS the grant died with its owner, though this process still held it");
		}
		"redeem-live" => {
			let (connection, grant) = delegated();
			say(b"holds the delegated request connection and grant");
			if !matches!(execute(grant), Some(Ok(AdminResult::Completed))) {
				fail(b"redeem-live: a grant redeemed while its owner lived did not complete");
			}
			if matches!(execute(grant), Some(Ok(_))) {
				fail(b"redeem-live: the grant was redeemed twice");
			}
			close(connection);
			say(b"PASS redeemed once while the owner lived, and never again");
		}
		"release" => {
			await_end();
			sleep_until(clock() + TICKS / 3);
			if !matches!(admin_test::Client::with_deadline(ChannelTransport { chan: test }, clock() + 5 * TICKS).journal(&AdminJournalFault::None), Some(Ok(()))) {
				fail(b"release: the held acknowledgments could not be released");
			}
			say(b"released the held acknowledgments after their requester ended");
		}
		_ => fail(b"usage: adminhelper contend | redeem | redeem-live | release"),
	}
	exit();
}
