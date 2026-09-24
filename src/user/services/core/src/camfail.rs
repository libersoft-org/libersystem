// camfail - the camera gate's launch that must fail. DEVELOPMENT-ONLY.
//
// Its manifest grants capture and an authority whose grant cannot be minted, so PermissionManager mints the
// capture grant, fails the next one and abandons the prepared launch. This program therefore never runs:
// if it does, the gate is told so. What the gate checks is that the capture grant minted for a launch that
// never happened was retired with it.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	print(b"camfail: FAIL the launch was not refused\n");
	exit();
}
