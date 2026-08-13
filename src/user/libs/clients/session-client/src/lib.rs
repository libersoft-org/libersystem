// The session operations a governed TOOL may perform, behind the concrete client boundary.
//
// Deliberately NOT the whole interface. A tool holding this can ask about the jobs it owns and ask
// the session to signal one; it cannot register a job, take one's Process handle, or change the
// working directory or environment - those belong to the shell, which holds the session client
// directly. The narrowing is by what this library exports, so a tool cannot reach the rest without
// a change that is visible here.

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
}
