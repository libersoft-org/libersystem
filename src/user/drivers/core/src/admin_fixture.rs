// admin_fixture - an in-guest administrative executor: a bounded probe write over two target generations,
// published as an `admin-executor` provider, and a witness endpoint for the gate that has to count what it
// did.
//
// DEVELOPMENT-ONLY. It binds to a QEMU test function at a pinned address only the administrative-path gate
// adds, and its witness is a `fixture-control` publication no scope minted for real hardware admits. It is
// not a claim about firmware update: it proves the generic executor seam - preparation that copies, one
// attempt under a start guard - and nothing about USB DFU, which the USB driver set owns.
//
// NO APPROVAL SHORTCUT. The witness observes and injects faults; nothing on it starts an effect. The only
// way to the write is `execute` on the executor's own connection, which only AdminService holds.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drivers::admin_operation::{MAX_PAYLOAD, Operations, Refusal};
use drivers::common;
use proto::system::{AdminAction, AdminDescriptor, AdminPrepared, AdminProbeEffects, AdminProbeFault, AdminResult, Error, admin_executor, admin_probe_witness};
use rt::*;

const NAME: &str = "org.libersystem.admin-probe";
const CONTROL_NAME: &[u8] = b"org.libersystem.admin-probe.witness";
const EXECUTOR_TOKEN: u16 = 0;
const CONTROL_TOKEN: u16 = 1;
// The one target this executor serves, as a selector names it and as the descriptor names the live instance.
const TARGET: &str = "probe-target";
// How long a preparation stays startable, and how long the write may take once started.
const LIFETIME_TICKS: u64 = 12000;
const DEADLINE_MS: u32 = 1000;

struct Fixture {
	operations: Operations,
	generation: u64,
	writes: u32,
	dispatches: u32,
	digest: Vec<u8>,
	fault: AdminProbeFault,
	// This execution's answer is lost: the effect was performed and nobody is told.
	silent: bool,
}

fn refusal(refusal: Refusal) -> Error {
	match refusal {
		Refusal::Bounds => Error::Invalid,
		Refusal::NotFound => Error::NotFound,
		Refusal::Busy => Error::Again,
		Refusal::Stale | Refusal::Expired => Error::Stale,
		Refusal::Cancelled | Refusal::Started => Error::Denied,
	}
}

struct ExecutorView<'a> {
	fixture: &'a mut Fixture,
}

impl admin_executor::Service for ExecutorView<'_> {
	// THE PAYLOAD IS COPIED HERE AND ONLY HERE, from the object's own size and never past it.
	fn prepare(&mut self, action: AdminAction, target: String, parameters: Vec<u8>, payload_length: u32, payload: u64) -> Result<AdminPrepared, Error> {
		let copied = copy_payload(payload, payload_length);
		close(payload);
		if action != AdminAction::ProbeWrite {
			return Err(Error::Unsupported);
		}
		if target != TARGET {
			return Err(Error::NotFound);
		}
		let copied = copied.ok_or(Error::Invalid)?;
		let fixture = &mut *self.fixture;
		let generation = fixture.generation;
		let epoch = fixture.operations.epoch;
		let prepared = fixture.operations.prepare(generation, &parameters, &copied, clock(), LIFETIME_TICKS).map_err(refusal)?;
		let descriptor = AdminDescriptor { version: 1, action, executor: String::from(NAME), executor_epoch: epoch, target: String::from(TARGET), target_generation: prepared.generation, parameters: prepared.parameters.clone(), payload_length, payload_digest: prepared.digest.to_vec() };
		Ok(AdminPrepared { operation: prepared.operation, descriptor, deadline_ms: DEADLINE_MS })
	}

	fn revalidate(&mut self, operation: u64) -> Result<(), Error> {
		self.fixture.operations.revalidate(operation, self.fixture.generation, clock()).map_err(refusal)
	}

	// EVERY CALL IS COUNTED as a dispatch, whatever becomes of it: a gate asserting that nothing reached the
	// executor asserts on this number.
	fn execute(&mut self, operation: u64, epoch: u64) -> Result<AdminResult, Error> {
		let fixture = &mut *self.fixture;
		fixture.dispatches += 1;
		if fixture.fault == AdminProbeFault::Fail {
			fixture.fault = AdminProbeFault::None;
			return Err(Error::Io);
		}
		let generation = fixture.generation;
		let started = fixture.operations.start(operation, epoch, generation, clock()).map_err(refusal)?;
		// THE EFFECT: the frozen copy, written once.
		fixture.writes += 1;
		fixture.digest = started.digest.to_vec();
		print(b"driver.admin-fixture: the probe write was performed\n");
		if fixture.fault == AdminProbeFault::LoseReply {
			fixture.fault = AdminProbeFault::None;
			fixture.silent = true;
		}
		Ok(AdminResult::Completed)
	}

	fn cancel(&mut self, operation: u64) -> Result<(), Error> {
		self.fixture.operations.cancel(operation).map_err(refusal)
	}
}

// The requester's bytes, copied out of its object: exactly `length` of them, from an object at least that
// long, and at most the bound.
fn copy_payload(payload: u64, length: u32) -> Option<Vec<u8>> {
	let length = length as usize;
	if length == 0 || length > MAX_PAYLOAD || object_info(payload).is_none_or(|info| (info.size as usize) < length) {
		return None;
	}
	let addr = unsafe { map_object(payload) }?;
	let copied = unsafe { core::slice::from_raw_parts(addr as *const u8, length) }.to_vec();
	unmap_object(payload);
	Some(copied)
}

struct WitnessView<'a> {
	fixture: &'a mut Fixture,
}

impl admin_probe_witness::Service for WitnessView<'_> {
	fn effects(&mut self) -> Result<AdminProbeEffects, Error> {
		let fixture = &*self.fixture;
		Ok(AdminProbeEffects { writes: fixture.writes, dispatches: fixture.dispatches, digest: fixture.digest.clone(), generation: fixture.generation })
	}

	// THE TARGET IS REPLACED: every preparation against the old generation fails revalidation and its start.
	fn replace_target(&mut self) -> Result<u64, Error> {
		self.fixture.generation += 1;
		Ok(self.fixture.generation)
	}

	fn inject(&mut self, fault: AdminProbeFault) -> Result<(), Error> {
		self.fixture.fault = fault;
		Ok(())
	}
}

fn serve(fixture: &mut Fixture, token: u16, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let mut reply_buf = [0u8; 1024];
	let mut reply_handles = wire::Handles::new();
	let written = if token == CONTROL_TOKEN { admin_probe_witness::dispatch(&mut WitnessView { fixture: &mut *fixture }, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) } else { admin_executor::dispatch(&mut ExecutorView { fixture: &mut *fixture }, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) };
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	if core::mem::take(&mut fixture.silent) {
		return true;
	}
	if let Some(written) = written {
		send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let (Some((executor, executor_far)), Some((control, control_far))) = (channel(), channel()) else { exit() };
	common::online_named(bootstrap, &bind, b"driver.admin-fixture: online (a bounded probe write over two target generations, for the administrative-path gate)", &[(driver_protocol::provider::ADMIN_EXECUTOR, executor_far, NAME.as_bytes()), (driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME)]);
	let mut serving = common::Serving::from_offers(&[(EXECUTOR_TOKEN, executor), (CONTROL_TOKEN, control)]);
	// AN EPOCH OF ITS OWN, so a preparation made before a restart of this executor cannot start after it.
	let mut fixture = Fixture { operations: Operations::new(clock_ns() | 1), generation: 1, writes: 0, dispatches: 0, digest: Vec::new(), fault: AdminProbeFault::None, silent: false };
	let mut buf = alloc::vec![0u8; 2048];
	loop {
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[]) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) | Some(common::ProviderReady::Device(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let token = serving.token_at(index);
				let chan = serving.at(index);
				if !serve(&mut fixture, token, chan, &mut buf) {
					let token = serving.close_at(index);
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
	}
}
