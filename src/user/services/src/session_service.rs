// SessionService - the userspace session-state service.
//
// ServiceManager starts this program from the init package and hands it a bootstrap
// channel. SessionService reports in, then waits for a "SERVE" message carrying the
// channel its clients reach it on. Over that channel clients speak the generated
// `liber:system` Session bindings.
//
// A session is a long-lived login context: it owns the working directory (and, in
// later phases, the environment and the job table). Each session is reached over its
// own dedicated channel - the session capability the spawner (ServiceManager for VT 1,
// ConsoleService for the other VTs) mints with `service_connect` and hands to the
// shell. The spawner keeps the channel and re-hands a duplicate to each shell it
// starts, so the session - and thus the cwd - outlives any one shell: a logout or a
// supervisor restart leaves it intact, because SessionService, not the shell, holds it.
//
// One session per connected channel: `serve_multi` hands every request the channel it
// arrived on, and the first request on a channel lazily creates a fresh session at the
// default cwd. The session is pure state - the caller resolves and validates a path
// against the volume before `chdir`, so SessionService reaches no filesystem itself.
//
// When the supervisor that started it drops the bootstrap channel, the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::session::{self, Service};
use proto::system::Error;
use rt::*;

// The default working directory a fresh session starts in: the root of the system
// volume. A session keeps it until a client `chdir`s elsewhere. Matches the shell's
// own DEFAULT_CWD, the one place a prompt starts.
const DEFAULT_CWD: &str = "vol://system";

// One login session's state: its working directory (later phases add the environment
// and the job table). Behind the generated Session contract.
struct Session {
	cwd: String,
}

impl Session {
	fn new() -> Session {
		Session { cwd: String::from(DEFAULT_CWD) }
	}
}

impl Service for Session {
	fn cwd(&mut self) -> Result<String, Error> {
		Ok(self.cwd.clone())
	}

	fn chdir(&mut self, path: String) -> Result<(), Error> {
		self.cwd = path;
		Ok(())
	}
}

// The live sessions, keyed by the serve channel each is reached over. A request on a
// channel with no session yet lazily creates one at the default cwd; the session then
// persists for the life of that channel (the spawner keeps it open across shell
// restarts), so the cwd is not lost when a shell exits.
struct Sessions {
	map: Vec<(u64, Session)>,
}

impl Sessions {
	fn new() -> Sessions {
		Sessions { map: Vec::new() }
	}

	// The session reached over `chan`, created at the default cwd on first use.
	fn for_channel(&mut self, chan: u64) -> &mut Session {
		let mut i: usize = 0;
		while i < self.map.len() {
			if self.map[i].0 == chan {
				return &mut self.map[i].1;
			}
			i += 1;
		}
		self.map.push((chan, Session::new()));
		let last: usize = self.map.len() - 1;
		&mut self.map[last].1
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"SessionService: online", 0);
	}

	// 2. wait for the serve channel clients reach us on. If the supervisor drops the
	//    bootstrap channel first (no clients this boot), we are done.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. serve generated session requests, one session per connected channel, until the
	//    root channel closes.
	let mut sessions: Sessions = Sessions::new();
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 1024] = [0u8; 1024];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { session::dispatch(sessions.for_channel(chan), req, handle, out, reply_handle) });
	}
	exit();
}
