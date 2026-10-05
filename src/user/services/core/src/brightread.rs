// brightread - an ORDINARY brightness client, for the gate's denial check. DEVELOPMENT-ONLY.
//
// Its permission row grants DisplayService's brightness read and nothing else. It proves what that means from the
// client's side: the read answers, no control was handed over at all, and a set framed exactly as the control root
// frames it, sent down the read connection, is not something the connection carries - the service closes it rather
// than interpreting the bytes as anything.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use ipc_client::ChannelTransport;
use proto::system::{BrightnessTarget, display_brightness, display_brightness_control};
use rt::*;
use services::capability_names::*;
use wire::Sink;

fn say(line: &str) {
	print(b"brightread: ");
	print(line.as_bytes());
	print(b"\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	// THE LAUNCH CONTEXT COMES FIRST on the bootstrap, before any granted connection; it carries nothing this needs.
	let _ = recv_launch_bytes(bootstrap);
	let read = recv_tagged(bootstrap, &mut buf, CAP_BRIGHTNESS).unwrap_or(0);
	// NO CONTROL TAG FOLLOWS. A grant that is not in this program's row is not sent, so the probe does not wait for one.
	if read == 0 {
		say("FAIL the read authority was not granted");
		exit();
	}
	match display_brightness::Client::with_deadline(ChannelTransport { chan: read }, clock() + TICKS_PER_SECOND * 3).backlights() {
		Some(Ok(backlights)) => say(&format!("read {} backlight(s)", backlights.len())),
		other => {
			say(&format!("FAIL the read did not answer: {other:?}"));
			exit();
		}
	}
	let mut writer = wire::VecWriter::new();
	let framed = (|| {
		writer.u16(display_brightness_control::OP_SET)?;
		writer.u32(1)?;
		writer.bytes_lp(b"")?;
		BrightnessTarget::Level(0).write(&mut writer)?;
		writer.boolean(true)
	})();
	let Some(frame) = framed.and_then(|()| writer.into_inner()) else {
		say("FAIL the set could not be framed");
		exit();
	};
	let refused = !send_caps_blocking(read, &frame, &[]) || {
		let mut handles = wire::Handles::new();
		matches!(recv_vec_caps_deadline(read, &mut handles, clock() + 300), ReceivedVecCaps::Closed)
	};
	say(if refused { "set refused - the read connection carries no set" } else { "FAIL the read connection answered a set" });
	exit();
}
