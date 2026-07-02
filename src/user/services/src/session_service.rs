// SessionService - the userspace session-state service.
//
// ServiceManager starts this program from the init package and hands it a bootstrap
// channel. SessionService reports in, then waits for a "SERVE" message carrying the
// channel its clients reach it on. Over that channel clients speak the generated
// `liber:system` Session bindings.
//
// A session is a long-lived login context: it owns the working directory, the
// environment variables, and the job table. Each session is reached over its
// own dedicated channel - the session capability the spawner (ServiceManager for VT 1,
// ConsoleService for the other VTs) mints with `service_connect` and hands to the
// shell. The spawner keeps the channel and re-hands a duplicate to each shell it
// starts, so the session - and thus the cwd, the environment and the job table -
// outlives any one shell: a logout or a supervisor restart leaves it intact, because
// SessionService, not the shell, holds it. Background jobs in particular survive a shell
// restart because their live Process handles live here, not in the shell that started
// them.
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
use proto::system::{EnvVar, Error, JobEntry, JobInfo};
use rt::*;

// The default working directory a fresh session starts in: the root of the system
// volume. A session keeps it until a client `chdir`s elsewhere. Matches the shell's
// own DEFAULT_CWD, the one place a prompt starts.
const DEFAULT_CWD: &str = "vol://system";

// The default `PATH` a fresh session starts with: the directory the tool binaries live in
// on the system volume. A client `PATH=...` overwrites it. Command lookup by search path
// arrives with the binaries-on-volume phase; today the value is stored, expanded (`$PATH`)
// and inherited like any other variable.
const DEFAULT_PATH: &str = "vol://system/bin";

// One tracked background / stopped job in a session's job table: the live Process handle
// (the shell transferred it here, held with WAIT so the session can poll it and MANAGE so
// it can signal it), the program name, the session-assigned id, and whether it is
// currently stopped (suspended by Ctrl+Z).
struct Job {
	id: u32,
	proc: u64,
	name: String,
	stopped: bool,
}

// One login session's state: its working directory, its environment variables (seeded
// with `PATH`), and its job table - the tracked jobs and the next id to assign. Behind
// the generated Session contract. The job ids are session-assigned, so they stay stable
// across a shell restart, and the environment persists there too.
struct Session {
	cwd: String,
	vars: Vec<(String, String)>,
	jobs: Vec<Job>,
	next_id: u32,
}

impl Session {
	fn new() -> Session {
		Session { cwd: String::from(DEFAULT_CWD), vars: alloc::vec![(String::from("PATH"), String::from(DEFAULT_PATH))], jobs: Vec::new(), next_id: 1 }
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

	// Register a background or stopped job: the request transferred us its live Process
	// handle, which we keep so the job outlives the shell that started it. We assign and
	// return the small id the shell shows the user.
	fn job_register(&mut self, name: String, stopped: bool, proc: u64) -> Result<u32, Error> {
		let id: u32 = self.next_id;
		self.next_id = self.next_id.wrapping_add(1);
		self.jobs.push(Job { id, proc, name, stopped });
		Ok(id)
	}

	// Take a job back out for foregrounding: remove it and transfer its Process handle to
	// the caller (the reply carries the handle out-of-band). NotFound if no such id.
	fn job_take(&mut self, id: u32) -> Result<JobEntry, Error> {
		let pos: usize = self.jobs.iter().position(|j: &Job| j.id == id).ok_or(Error::NotFound)?;
		let job: Job = self.jobs.remove(pos);
		Ok(JobEntry { info: JobInfo { id: job.id, name: job.name, stopped: job.stopped }, proc: job.proc })
	}

	// List the tracked jobs for the shell's `jobs` command.
	fn job_list(&mut self) -> Result<Vec<JobInfo>, Error> {
		Ok(self.jobs.iter().map(|j: &Job| JobInfo { id: j.id, name: j.name.clone(), stopped: j.stopped }).collect())
	}

	// Reap finished jobs: poll each running (not stopped) job's Process handle - once it
	// reads ready the process has terminated - then drop it (closing the handle) and report
	// it, the way a shell announces a background job's completion before the next prompt.
	fn job_reap(&mut self) -> Result<Vec<JobInfo>, Error> {
		let mut done: Vec<JobInfo> = Vec::new();
		let mut i: usize = 0;
		while i < self.jobs.len() {
			if !self.jobs[i].stopped && unsafe { poll_ready(self.jobs[i].proc) } {
				let job: Job = self.jobs.remove(i);
				unsafe { close(job.proc) };
				done.push(JobInfo { id: job.id, name: job.name, stopped: job.stopped });
			} else {
				i += 1;
			}
		}
		Ok(done)
	}

	// Resume a stopped job in the background (SIG_CONT), leaving it tracked. NotFound if no
	// such id; a job that is already running is left as is.
	fn job_resume(&mut self, id: u32) -> Result<JobInfo, Error> {
		let pos: usize = self.jobs.iter().position(|j: &Job| j.id == id).ok_or(Error::NotFound)?;
		if self.jobs[pos].stopped {
			unsafe { signal(self.jobs[pos].proc, SIG_CONT) };
			self.jobs[pos].stopped = false;
		}
		Ok(JobInfo { id: self.jobs[pos].id, name: self.jobs[pos].name.clone(), stopped: self.jobs[pos].stopped })
	}

	// Read one environment variable. NotFound if the session has no variable by that name.
	fn env_get(&mut self, name: String) -> Result<String, Error> {
		self.vars.iter().find(|(n, _): &&(String, String)| *n == name).map(|(_, v): &(String, String)| v.clone()).ok_or(Error::NotFound)
	}

	// Create or overwrite an environment variable, so the value persists in the session.
	fn env_set(&mut self, name: String, value: String) -> Result<(), Error> {
		match self.vars.iter_mut().find(|(n, _): &&mut (String, String)| *n == name) {
			Some(entry) => entry.1 = value,
			None => self.vars.push((name, value)),
		}
		Ok(())
	}

	// Remove an environment variable if present (idempotent - unsetting an absent one is ok).
	fn env_unset(&mut self, name: String) -> Result<(), Error> {
		self.vars.retain(|(n, _): &(String, String)| *n != name);
		Ok(())
	}

	// List the environment for the shell's `env` command and its startup variable cache.
	fn env_list(&mut self) -> Result<Vec<EnvVar>, Error> {
		Ok(self.vars.iter().map(|(n, v): &(String, String)| EnvVar { name: n.clone(), value: v.clone() }).collect())
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
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { session::dispatch(sessions.for_channel(chan), req, handle, out, reply_handle) });
	}
	exit();
}
