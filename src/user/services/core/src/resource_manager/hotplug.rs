// THE MEMORY A HOST PLUGS IN: the policy over the machine's plugged memory, if it has a device that offers some.
//
// MORE MEMORY IS NEVER REFUSED, so the allowance is the whole region the device offers: what the host plugs in, this
// machine takes. What the host asks back is the kernel's to give - a block goes back only once nothing holds a frame of
// it - and this policy says when the host is asking for memory something here is still using.
//
// Looked at every second, on the serve loop's housekeeping tick, beside the balloon's.

use alloc::string::String;
use core::fmt::Write as _;
use ipc_client::ChannelTransport;
use proto::generated::liber::hotplug_device::v1::hotplug_device;
use proto::system::ProviderKind;
use rt::*;

use crate::provider_subscription::{ProviderWatch, open_provider};

pub struct HotplugPolicy {
	catalogue: u64,
	watch: ProviderWatch,
	device: u64,
	allowed: Option<u64>,
	// What was last said about memory the host asked back that is in use.
	in_use: u64,
}

impl HotplugPolicy {
	pub fn new(catalogue: u64) -> HotplugPolicy {
		HotplugPolicy { catalogue, watch: ProviderWatch::subscribe(catalogue, ProviderKind::MemoryHotplug), device: 0, allowed: None, in_use: 0 }
	}

	pub fn tick(&mut self) {
		self.watch.poll();
		if self.device == 0 {
			let Some(info) = self.watch.entries.first().cloned() else { return };
			self.device = open_provider(self.catalogue, &info);
			self.allowed = None;
			if self.device == 0 {
				return;
			}
		}
		let mut client = hotplug_device::Client::new(ChannelTransport { chan: self.device });
		let Some(Ok(mut status)) = client.status() else {
			close(self.device);
			self.device = 0;
			return;
		};
		if self.allowed != Some(status.region_size) {
			match client.allow(&status.region_size) {
				Some(Ok(answer)) => {
					self.allowed = Some(status.region_size);
					status = answer;
				}
				_ => return,
			}
		}
		if status.in_use == self.in_use {
			return;
		}
		self.in_use = status.in_use;
		let mut line = String::new();
		if status.in_use != 0 {
			let _ = write!(line, "ResourceManager: the host asks for plugged memory back and {} MiB of it is in use here - it stays plugged until it is freed\n", status.in_use >> 20);
		} else {
			let _ = write!(line, "ResourceManager: {} MiB plugged, as the host asks\n", status.plugged >> 20);
		}
		print(line.as_bytes());
	}
}
