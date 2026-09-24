// midifail - the MIDI gate's launch that must fail. DEVELOPMENT-ONLY.
//
// Its manifest grants a receiver and an authority whose grant cannot be minted, so PermissionManager mints the
// receiver, fails the next grant and abandons the prepared launch. This program therefore never runs: if it
// does, the gate is told so. What the gate checks is that the receiver minted for a launch that never
// happened was retired with it, and its endpoint freed.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	print(b"midifail: FAIL the launch was not refused\n");
	exit();
}
