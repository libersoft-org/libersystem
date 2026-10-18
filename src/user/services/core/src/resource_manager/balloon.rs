// THE MEMORY THE HOST MAY HAVE BACK: the policy over the machine's balloon, if it has one.
//
// THE RESERVE IS WHAT NO HOST IS GIVEN: an eighth of the machine and never under 64 MiB stays free for the machine's own
// programs, and the balloon is allowed what it holds plus whatever is free above that. When the machine's programs eat
// into the reserve, the allowance falls by what they took and the balloon gives it back - so the host never chooses a
// victim: the balloon only ever holds memory nobody here is using, and gives it up before anybody here is refused for
// long. The kernel's own floor (a sixteenth) stands under this whatever it says.
//
// Looked at every second, on the serve loop's housekeeping tick.

use alloc::string::String;
use core::fmt::Write as _;
use ipc_client::ChannelTransport;
use proto::generated::liber::balloon_device::v1::{BalloonStatus, balloon_device};
use proto::system::ProviderKind;
use rt::*;

use crate::provider_subscription::{ProviderWatch, open_provider};

// The reserve, in 4 KiB frames, for a machine of `total`.
pub fn reserve(total: u64) -> u64 {
	(total / 8).max(16384)
}

// THE ALLOWANCE for a balloon holding `held` on a machine with `free` of `total` free: what it holds and what is free
// above the reserve - less what the machine's programs have taken out of the reserve.
pub fn allowance(total: u64, free: u64, held: u64) -> u64 {
	let reserve = reserve(total);
	if free >= reserve { held.saturating_add(free - reserve) } else { held.saturating_sub(reserve - free) }
}

pub struct BalloonPolicy {
	catalogue: u64,
	watch: ProviderWatch,
	balloon: u64,
	allowed: Option<u64>,
	// Whether the last status held part of the host's target back - said once each time it changes.
	holding_back: bool,
}

impl BalloonPolicy {
	pub fn new(catalogue: u64) -> BalloonPolicy {
		BalloonPolicy { catalogue, watch: ProviderWatch::subscribe(catalogue, ProviderKind::MemoryBalloon), balloon: 0, allowed: None, holding_back: false }
	}

	// ONE LOOK: the balloon opened if one was published, the allowance worked out from the machine's memory and sent when
	// it changed, and a change in what the host is held back from said.
	pub fn tick(&mut self) {
		self.watch.poll();
		if self.balloon == 0 {
			let Some(info) = self.watch.entries.first().cloned() else { return };
			self.balloon = open_provider(self.catalogue, &info);
			self.allowed = None;
			if self.balloon == 0 {
				return;
			}
		}
		let mut client = balloon_device::Client::new(ChannelTransport { chan: self.balloon });
		let Some(Ok(status)) = client.status() else {
			// THE BALLOON WENT: closed here, and looked for again.
			close(self.balloon);
			self.balloon = 0;
			return;
		};
		let mut memory = MemoryStats::default();
		if memory_stats(&mut memory) <= 0 {
			return;
		}
		let allowed = allowance(memory.total_frames, memory.free_frames, status.held);
		let status = if self.allowed != Some(allowed) {
			match client.allow(&allowed) {
				Some(Ok(status)) => {
					self.allowed = Some(allowed);
					status
				}
				_ => return,
			}
		} else {
			status
		};
		self.say(&status, memory.total_frames);
	}

	fn say(&mut self, status: &BalloonStatus, total: u64) {
		let holding_back = status.held_back != 0 && status.held >= status.allowed;
		if holding_back == self.holding_back {
			return;
		}
		self.holding_back = holding_back;
		let mut line = String::new();
		if holding_back {
			let _ = write!(line, "ResourceManager: the host asks for {} pages back and the balloon holds {} - {} held back to keep {} pages free\n", status.target, status.held, status.held_back, reserve(total));
		} else {
			let _ = write!(line, "ResourceManager: the balloon holds {} of the {} pages the host asks for\n", status.held, status.target);
		}
		print(line.as_bytes());
	}
}
