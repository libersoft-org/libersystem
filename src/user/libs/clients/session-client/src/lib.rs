// The session operations a governed TOOL may perform, behind the concrete client boundary.
//
// Deliberately NOT the whole interface. A tool holding this can ask about the jobs it owns, ask the
// session to signal one, and REGISTER a program it has already started; it cannot take a job's
// Process handle, resume or reap one, or change the working directory or environment - those belong
// to the shell, which holds the session client directly. The narrowing is by what this library
// exports, so a tool cannot reach the rest without a change that is visible here.
//
// `job_register` was added 2026-08-16 for LiberCommander's command bar, whose `&` has to produce a
// job the session knows about rather than a process nobody is tracking. It HANDS OVER a task handle
// the caller already holds and receives an id; it grants the caller nothing it did not have, which
// is why it belongs on this side of the line and `job_take` does not.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use session_proto::generated::liber::session::v1::{JobInfo, JobSignalKind};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_session_session_job_list"]
	fn session_job_list(chan: u64) -> Option<Result<Vec<JobInfo>, Error>>;
	#[link_name = "liber_channel_liber_session_session_job_signal"]
	fn session_job_signal(chan: u64, id: &u32, signal: &JobSignalKind) -> Option<Result<JobInfo, Error>>;
	#[link_name = "liber_channel_liber_session_session_job_register"]
	fn session_job_register(chan: u64, name: &str, stopped: &bool, group: &bool, proc: &u64) -> Option<Result<u32, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct SessionClient {
	chan: u64,
}

impl SessionClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	/// The jobs this session tracks.
	#[inline(always)]
	pub fn job_list(&mut self) -> Option<Result<Vec<JobInfo>, Error>> {
		unsafe { session_job_list(self.chan) }
	}

	/// Ask the session to signal one of its jobs. The Process handle stays with the session.
	#[inline(always)]
	pub fn job_signal(&mut self, id: u32, signal: JobSignalKind) -> Option<Result<JobInfo, Error>> {
		unsafe { session_job_signal(self.chan, &id, &signal) }
	}

	/// Register a program this caller has already started as a job of the session, handing over the
	/// task handle. Answers the job id.
	///
	/// It gives the session something rather than taking something from it, which is what makes it
	/// safe to hand a tool: a caller that could not start a program has nothing to register.
	#[inline(always)]
	pub fn job_register(&mut self, name: &str, stopped: bool, group: bool, task: u64) -> Option<Result<u32, Error>> {
		unsafe { session_job_register(self.chan, name, &stopped, &group, &task) }
	}
}
