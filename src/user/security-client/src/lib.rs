#![no_std]

use base_proto::generated::liber::base::v1::Error;
use process_proto::generated::liber::process::v1::StartResult;
use security_proto::generated::liber::security::v1::Manifest;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_security_permission_lookup"]
	fn permission_lookup(chan: u64, component: &str) -> Option<Result<Manifest, Error>>;
	#[link_name = "liber_channel_liber_security_permission_audit"]
	fn permission_audit(chan: u64) -> Option<u64>;
	#[link_name = "liber_channel_liber_security_permission_run"]
	fn permission_run(chan: u64, name: &str, args: &str, cwd: &str, stdout: &u64) -> Option<Result<StartResult, Error>>;
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

	#[inline(always)]
	pub fn run(&mut self, name: &str, args: &str, cwd: &str, stdout: &u64) -> Option<Result<StartResult, Error>> {
		unsafe { permission_run(self.chan, name, args, cwd, stdout) }
	}
}
