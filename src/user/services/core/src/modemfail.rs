// modemfail - the modem gate's launch that must fail. DEVELOPMENT-ONLY.
//
// Its manifest grants data and identity, and its identity grant names a modem nobody configured - so
// PermissionManager mints its data grant, fails to mint the identity one, and abandons the prepared
// launch. This program therefore never runs: if it does, the gate is told so. What the gate checks is
// the other side - that the data grant minted for a launch that never happened was retired with it.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	print(b"modemfail: FAIL the launch was not refused\n");
	exit();
}
