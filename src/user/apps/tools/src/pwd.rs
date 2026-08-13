// pwd - print the working directory the launcher handed us, run as its own sandboxed ELF.
//
// THE POINT IS THAT IT HAS NO CAPABILITY. `pwd` answers from the launch context alone - a working
// directory is data, not authority - so PermissionManager grants it nothing at all. A `pwd` that
// asked a volume or the session where it was would be a `pwd` that could be refused, and would
// disagree with the shell prompt whenever the two asked at different moments.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::codec::{JsonMode, json_escape};
use proto::system::LaunchContext;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. adopt the forwarded stdout console, so our output renders on the shell's terminal.
		inherit_stdout(bootstrap);
		// 2. the launch context carries both the arguments and the cwd this tool exists to print.
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arguments: Vec<u8> = context.arguments.clone().into_bytes();
		let cwd: &str = context.cwd.as_str();
		match JsonMode::parse(tools::trim(&arguments)) {
			Some(mode) => {
				let mut out = String::from("{\"cwd\":");
				json_escape(cwd, &mut out);
				out.push('}');
				let rendered = mode.render(out);
				print(rendered.as_bytes());
				print(b"\n");
			}
			None => {
				print(cwd.as_bytes());
				print(b"\n");
			}
		}
	}
	exit();
}
