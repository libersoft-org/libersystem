// WHAT `@rights` ON A HANDLE PARAMETER ACTUALLY DOES (IDL-004).
//
// `display.lsidl` declares `bind: func(@rights(manage) task: handle<task>)`, and the generator turns
// that into a call to `codec::handle_carries` ahead of the service, refusing with the schema's own
// `denied` when the answer is no. The value of that is entirely in whether the refusal HAPPENS: an
// annotation that reached the ABI signature and no generated code is precisely the state IDL-004
// reported, and reading the emitted source is how that state went unnoticed - the guard was there to
// read, in a comment.
//
// So these tests run the generated dispatch. They encode a `bind` request the way the generated
// client encodes one, hand it a task handle whose authority they choose, and require the service to
// be reached or not reached accordingly. A guard that stopped firing fails the second test; a guard
// that refused everything fails the first.
//
// The authority itself comes from `liber_handle_authority`, which the runtime exports and this
// binary does not have - the runtime issues a syscall, and there is no kernel here. The stub below
// is that symbol, backed by a per-thread cell so two tests in the same process cannot see each
// other's answer.

use crate::codec::Handles;
use crate::generated::liber::base::v1::Error;
use crate::generated::liber::display::v1::PresentationStats;
use crate::generated::liber::display::v1::display_admin::{OP_BIND, Service, dispatch};
use std::cell::Cell;

// The rights word the kernel reports for RIGHT_MANAGE; `abi::RIGHT_MANAGE` is `1 << 10` and the
// generator emits that same 1024 into the guard. Written out rather than imported so a change to
// either side is a failure here instead of two constants moving together.
const RIGHT_MANAGE: u32 = 1 << 10;
const RIGHT_READ: u32 = 1 << 0;

// The stable object-type codes. `liber:process@1` declares `@kernel(process)` on its `task`
// resource, so the generator now emits the Process code into this parameter's guard and a handle of
// any other kind is refused before the service is reached. Written out rather than imported, like
// the rights above and for the same reason.
const TYPE_PROCESS: u64 = 1;
const TYPE_CHANNEL: u64 = 5;

thread_local! {
	static AUTHORITY: Cell<u64> = const { Cell::new(u64::MAX) };
}

fn packed(object_type: u64, rights: u32) -> u64 {
	(object_type << 32) | u64::from(rights)
}

#[unsafe(no_mangle)]
pub extern "C" fn liber_handle_authority(_handle: u64) -> u64 {
	AUTHORITY.with(|a| a.get())
}

#[derive(Default)]
struct Recording {
	binds: u32,
}

impl Service for Recording {
	fn bind(&mut self, _task: u64) -> Result<u64, Error> {
		self.binds += 1;
		Ok(0x5eed)
	}

	fn stats(&mut self) -> PresentationStats {
		unimplemented!("no test in this module reaches stats")
	}
}

// The bytes a generated client writes for `bind`: the opcode, the correlation id, and the u32
// placeholder that stands where the handle is - the handle itself travels beside the message.
fn bind_request(corr: u32) -> Vec<u8> {
	let mut request = Vec::new();
	request.extend_from_slice(&OP_BIND.to_le_bytes());
	request.extend_from_slice(&corr.to_le_bytes());
	request.extend_from_slice(&0u32.to_le_bytes());
	request
}

// Run one `bind` against a service, with `task` presented as carrying `authority`.
fn bind_with(authority: u64) -> (Recording, Result<u64, Error>) {
	AUTHORITY.with(|a| a.set(authority));
	let mut service = Recording::default();
	let mut request_handles = Handles::try_from_slice(&[0x11]).expect("one handle fits");
	let mut reply_handles = Handles::try_from_slice(&[]).expect("an empty list is a list");
	let mut out = [0u8; 256];
	let corr = 0xabcd_1234;
	let written = dispatch(&mut service, &bind_request(corr), &mut request_handles, &mut out, &mut reply_handles).expect("the dispatch encoded a reply");

	let mut reader = crate::codec::Reader::with_handle_list(&out[..written], &reply_handles);
	assert_eq!(reader.u32(), Some(corr), "the reply is for this request");
	let outcome = if reader.tag().expect("a result tag") {
		let _ = reader.u32().expect("the handle placeholder");
		Ok(reader.take_handle().expect("the bound channel"))
	} else {
		Err(Error::read(&mut reader).expect("an error variant"))
	};
	// NOTHING MAY BE LEFT OPEN BY A REFUSAL. The guard takes every capability the message carried
	// out of the list before it answers, so a caller's handle is not still sitting in a list nobody
	// will drain - which on a real system is a handle leaked once per refused request.
	assert!(request_handles.as_slice().is_empty(), "the request's handles were taken, whatever the answer");
	(service, outcome)
}

#[test]
fn a_task_handle_carrying_manage_reaches_the_service() {
	let (service, outcome) = bind_with(packed(TYPE_PROCESS, RIGHT_MANAGE | RIGHT_READ));
	assert_eq!(outcome, Ok(0x5eed), "the service answered");
	assert_eq!(service.binds, 1, "the service was called exactly once");
}

#[test]
fn a_task_handle_without_manage_is_refused_before_the_service_sees_it() {
	let (service, outcome) = bind_with(packed(TYPE_PROCESS, RIGHT_READ));
	assert_eq!(outcome, Err(Error::Denied), "the schema's own error, not the service's");
	assert_eq!(service.binds, 0, "the service was never reached");
}

// A handle the caller narrowed to everything BUT manage is the realistic version of the above: the
// rights word is large, and the one bit the signature asked for is the one that is missing.
#[test]
fn every_right_except_manage_is_still_a_refusal() {
	let all_but_manage = u32::MAX & !RIGHT_MANAGE;
	let (service, outcome) = bind_with(packed(TYPE_PROCESS, all_but_manage));
	assert_eq!(outcome, Err(Error::Denied));
	assert_eq!(service.binds, 0, "the service was never reached");
}

// An unknown handle - one the process does not hold at all - reports `u64::MAX`, and that is refused
// for the same reason as insufficient rights rather than being read as a rights word of all ones.
#[test]
fn an_unknown_handle_is_refused() {
	let (service, outcome) = bind_with(u64::MAX);
	assert_eq!(outcome, Err(Error::Denied));
	assert_eq!(service.binds, 0, "the service was never reached");
}

#[test]
fn a_handle_of_the_wrong_kernel_object_is_refused_however_wide_its_rights() {
	// `bind` takes a `handle<task>`, and `task` declares `@kernel(process)`. A Channel carrying
	// every right in the kernel is still not a process, and the guard says so before the service is
	// reached - which is the half `@rights` alone could never check: rights are about what may be
	// done to an object, not about which object it is.
	let (service, outcome) = bind_with(packed(TYPE_CHANNEL, u32::MAX));
	assert_eq!(outcome, Err(Error::Denied), "the schema's own error, not the service's");
	assert_eq!(service.binds, 0, "the service was never reached");
}
