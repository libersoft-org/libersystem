// powerread - an ORDINARY power-state client, for the gate's denial checks. DEVELOPMENT-ONLY.
//
// Its permission row grants the read authority and nothing else. It proves what that means from the
// client's side: the read verbs answer, no control authority was handed over at all, and a control or
// a publication sent down the read connection is not something the connection can carry - the
// service closes it rather than interpreting the bytes as anything.
//
//   powerread control   a set-output request, framed exactly as the control interface frames it
//   powerread publish   a provider update, framed exactly as a provider's stream frames it
//
// Neither reaches the fixture: the gate runs this before any operator command, and `powercheck
// control` then finds the fixture's command log still empty.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{LaunchContext, ProviderUpdate, ProviderUpdateKind, SourceKind, power, power_control, power_provider};
use rt::*;
use services::capability_names::*;
use wire::Sink;

fn fail(line: &[u8]) -> ! {
	print(b"powerread: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

// Send one frame down the read connection and see what the service does with it.
fn refused(read: u64, frame: &[u8]) -> bool {
	if !send_caps_blocking(read, frame, &[]) {
		return true;
	}
	let mut handles = wire::Handles::new();
	matches!(recv_vec_caps_deadline(read, &mut handles, clock() + 300), ReceivedVecCaps::Closed)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let read = recv_tagged(bootstrap, &mut buf, CAP_POWER_STATE).unwrap_or(0);
	// NO CONTROL TAG FOLLOWS. A grant that is not in this program's row is not sent, so the next message
	// on the bootstrap is not a control channel - and the probe does not wait for one.
	if read == 0 {
		fail(b"the read authority was not granted");
	}
	let deadline = clock() + 2000;
	let ups = loop {
		if let Some(Ok(sources)) = power::Client::new(ChannelTransport { chan: read }).sources()
			&& let Some(ups) = sources.iter().find(|source| source.state.kind == SourceKind::Ups)
		{
			break ups.clone();
		}
		if clock() >= deadline {
			fail(b"the sources could not be listed");
		}
		sleep_until(clock() + 25);
	};
	let mut writer = wire::VecWriter::new();
	let framed = match context.arguments.as_str() {
		"control" => (|| {
			writer.u16(power_control::OP_SET_OUTPUT)?;
			writer.u32(1)?;
			ups.id.write(&mut writer)?;
			writer.u8(1)?;
			writer.boolean(false)
		})(),
		"publish" => (|| {
			writer.u16(power_provider::OP_UPDATES + 2)?;
			writer.u32(1)?;
			ProviderUpdate { revision: u64::MAX, kind: ProviderUpdateKind::Added, source: None, gone: None }.write(&mut writer)
		})(),
		_ => fail(b"usage: powerread control | publish"),
	};
	let frame: Vec<u8> = match framed.and_then(|()| writer.into_inner()) {
		Some(frame) => frame,
		None => fail(b"the frame could not be written"),
	};
	if !refused(read, &frame) {
		fail(b"the read connection carried a request its interface does not have");
	}
	match context.arguments.as_str() {
		"control" => print(b"powerread: PASS a read client lists sources, holds no control authority, and a control sent on its connection closed it\n"),
		_ => print(b"powerread: PASS a publication sent on a read connection closed it\n"),
	}
	exit();
}
