// clear - erase the display and home the cursor, run as its own sandboxed ELF.
//
// It was a shell builtin, and it moved out here because it changes NO SESSION STATE: it writes two
// escape sequences to whatever terminal it inherited and exits. The shell keeps the builtins that
// have to touch what only the shell holds - the working directory, the variable table, the jobs -
// and everything else is a program, which is the split P02M0031 and P02M0095 are about.
//
// It needs no capability for the same reason `pwd` does not: stdout is inherited, not granted.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arguments: Vec<u8> = context.arguments.clone().into_bytes();
		// AN UNKNOWN OPTION FAILS BEFORE THE ESCAPE SEQUENCE. Half a control sequence on a terminal
		// is worse than no output at all - the terminal is left interpreting what follows as part
		// of it - so the check happens before anything is written.
		if !tools::trim(&arguments).is_empty() {
			eprint(b"clear: usage: clear\n");
			exit();
		}
		// ED (erase the whole display) then CUP (home the cursor), written as ONE message so a
		// terminal cannot be left erased with the cursor where it was.
		print(b"\x1b[2J\x1b[H");
	}
	exit();
}
