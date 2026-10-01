#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use process_proto::generated::liber::process::v1::{ProcessInfo, SleepReason, SleepRecord, SleepState, SleepStatus, StartResult};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_process_process_start"]
	fn process_start(chan: u64, name: &str) -> Option<Result<ProcessInfo, Error>>;
	#[link_name = "liber_channel_liber_process_process_list"]
	fn process_list(chan: u64) -> Option<Result<Vec<ProcessInfo>, Error>>;
	#[link_name = "liber_channel_liber_process_process_launch"]
	fn process_launch(chan: u64, name: &str, bootstrap: &u64) -> Option<Result<StartResult, Error>>;
	#[link_name = "liber_channel_liber_process_process_launch_bounded"]
	fn process_launch_bounded(chan: u64, name: &str, memory_limit: &u64, bootstrap: &u64) -> Option<Result<StartResult, Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_suspend"]
	fn sleep_suspend(chan: u64, state: &SleepState, timed_wake_ms: &u64, reason: &SleepReason) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_hibernate"]
	fn sleep_hibernate(chan: u64, hybrid: &bool, reason: &SleepReason) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_inhibit"]
	fn sleep_inhibit(chan: u64, milliseconds: &u32, reason: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_release"]
	fn sleep_release(chan: u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_last_sleep"]
	fn sleep_last(chan: u64) -> Option<Result<SleepRecord, Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_status"]
	fn sleep_status(chan: u64) -> Option<Result<SleepStatus, Error>>;
	#[link_name = "liber_channel_liber_process_system_sleep_schedule_wake"]
	fn sleep_schedule_wake(chan: u64, unix_seconds: &u64) -> Option<Result<(), Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ProcessClient {
	chan: u64,
}

impl ProcessClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn start(&mut self, name: &str) -> Option<Result<ProcessInfo, Error>> {
		unsafe { process_start(self.chan, name) }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<ProcessInfo>, Error>> {
		unsafe { process_list(self.chan) }
	}

	#[inline(always)]
	pub fn launch(&mut self, name: &str, bootstrap: &u64) -> Option<Result<StartResult, Error>> {
		unsafe { process_launch(self.chan, name, bootstrap) }
	}

	#[inline(always)]
	pub fn launch_bounded(&mut self, name: &str, memory_limit: &u64, bootstrap: &u64) -> Option<Result<StartResult, Error>> {
		unsafe { process_launch_bounded(self.chan, name, memory_limit, bootstrap) }
	}
}

/// SERVICEMANAGER'S `system-sleep`: a sleep asked for - answered at acceptance, never at the resume - an idle sleep's
/// bounded inhibition, and the last sleep's record.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct SleepClient {
	chan: u64,
}

impl SleepClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn suspend(&mut self, state: &SleepState, timed_wake_ms: &u64, reason: &SleepReason) -> Option<Result<(), Error>> {
		unsafe { sleep_suspend(self.chan, state, timed_wake_ms, reason) }
	}

	#[inline(always)]
	pub fn hibernate(&mut self, hybrid: &bool, reason: &SleepReason) -> Option<Result<(), Error>> {
		unsafe { sleep_hibernate(self.chan, hybrid, reason) }
	}

	#[inline(always)]
	pub fn inhibit(&mut self, milliseconds: &u32, reason: &str) -> Option<Result<(), Error>> {
		unsafe { sleep_inhibit(self.chan, milliseconds, reason) }
	}

	#[inline(always)]
	pub fn release(&mut self) -> Option<Result<(), Error>> {
		unsafe { sleep_release(self.chan) }
	}

	#[inline(always)]
	pub fn last_sleep(&mut self) -> Option<Result<SleepRecord, Error>> {
		unsafe { sleep_last(self.chan) }
	}

	#[inline(always)]
	pub fn status(&mut self) -> Option<Result<SleepStatus, Error>> {
		unsafe { sleep_status(self.chan) }
	}

	#[inline(always)]
	pub fn schedule_wake(&mut self, unix_seconds: &u64) -> Option<Result<(), Error>> {
		unsafe { sleep_schedule_wake(self.chan, unix_seconds) }
	}
}
