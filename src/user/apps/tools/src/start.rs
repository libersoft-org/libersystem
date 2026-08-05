// start - start a service that was stopped, via the ServiceManager admin channel, run as its
// own sandboxed ELF.
//
// The inverse of `stop`, and deliberately a separate command rather than a flag on it: the two
// are opposites, not modes, and a system shell is where an operator expects to find both by
// name. It is granted exactly the same one capability - the supervisor admin channel - and
// reaches the supervisor only through it.
//
// The request is the service name with a `+` in front, which is how the admin protocol
// distinguishes the verbs: a real service name can never begin with a reserved character, so
// the verb costs no extra field and an older client cannot stumble into it.
//
// The supervisor answers no for a service it cannot bring back, and that is not a limitation
// of this command. A service can be restarted only when the supervisor holds its serve root
// and its clients resolve it by name; a replacement nobody can re-resolve would be a service
// its clients cannot reach, which is worse than one that is honestly down.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the name of the service to start.
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let name: Vec<u8> = context.arguments.clone().into_bytes();
		// 3. receive the one capability the manifest grants: a ServiceManager admin channel.
		let admin: u64 = recv_tagged(bootstrap, &mut buf, b"SUPERVISOR").unwrap_or_else(|| exit());
		start_service(admin, &name[..]);
	}
	exit();
}

// Ask ServiceManager to start a stopped service: send `+name` and print the reply - the name
// that came back up, or a notice that it did not.
unsafe fn start_service(admin: u64, name: &[u8]) {
	unsafe {
		if name.is_empty() {
			print(b"start: usage: start <service>\n");
			return;
		}
		let mut request: Vec<u8> = Vec::with_capacity(name.len() + 1);
		request.push(b'+');
		request.extend_from_slice(name);
		if !send_blocking(admin, &request, 0) {
			print(b"start: request failed\n");
			return;
		}
		let mut reply: [u8; 512] = [0u8; 512];
		match recv_blocking(admin, &mut reply) {
			Received::Message { len, .. } => {
				if reply[..len].starts_with(b"STARTED\n") {
					print(b"started: ");
					print(&reply[8..len]);
					print(b"\n");
				} else if len >= 10 && &reply[..10] == b"NOTSTARTED" {
					print(b"start: not a stopped service this supervisor can bring back\n");
				} else {
					print(&reply[..len]);
					print(b"\n");
				}
			}
			Received::Closed => print(b"start: supervisor gone\n"),
		}
	}
}
