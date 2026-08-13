#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::EnvVar;
use base_proto::generated::liber::base::v1::Error;
use process_proto::generated::liber::process::v1::StartResult;
use security_proto::generated::liber::security::v1::Manifest;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_security_permission_lookup"]
	fn permission_lookup(chan: u64, component: &str) -> Option<Result<Manifest, Error>>;
	#[link_name = "liber_channel_liber_security_permission_audit"]
	fn permission_audit(chan: u64) -> Option<u64>;
	#[link_name = "liber_channel_liber_security_permission_run"]
	fn permission_run(chan: u64, name: &str, args: &str, cwd: &str, environment: &Vec<EnvVar>, stdout: &u64) -> Option<Result<StartResult, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PermissionClient {
	chan: u64,
}

impl PermissionClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn lookup(&mut self, component: &str) -> Option<Result<Manifest, Error>> {
		unsafe { permission_lookup(self.chan, component) }
	}

	#[inline(always)]
	pub fn audit(&mut self) -> Option<u64> {
		unsafe { permission_audit(self.chan) }
	}

	/// Launch a governed command. The environment is a SNAPSHOT the launcher validates and passes
	/// to the child; the caller proposes it and cannot make the launcher accept it.
	///
	/// The signature carries `environment` because the op does. It did not, and nothing called this
	/// wrapper - so a caller that started using it would have passed five arguments to a symbol
	/// that takes six, which links by name and goes wrong at the call rather than at the build.
	#[inline(always)]
	pub fn run(&mut self, name: &str, args: &str, cwd: &str, environment: &Vec<EnvVar>, stdout: &u64) -> Option<Result<StartResult, Error>> {
		unsafe { permission_run(self.chan, name, args, cwd, environment, stdout) }
	}
}
