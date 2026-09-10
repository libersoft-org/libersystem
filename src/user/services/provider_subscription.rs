// The snapshot and live updates are one subscription. Keep identities until withdrawal so an
// idle consumer can discover every publication without holding a connection to every device.
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{ProviderInfo, ProviderKind, provider_catalogue};
use rt::*;

pub struct ProviderWatch {
	pub channel: u64,
	pub entries: Vec<ProviderInfo>,
}

pub fn same_provider(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	same_binding(a, b) && a.slot == b.slot && a.provider_generation == b.provider_generation
}

pub fn same_binding(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.bus == b.bus && a.dev == b.dev && a.func == b.func && a.binding_generation == b.binding_generation
}

impl ProviderWatch {
	pub fn subscribe(catalogue: u64, kind: ProviderKind) -> Self {
		let channel = if catalogue == 0 { 0 } else { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&kind).unwrap_or(0) };
		let mut watch = Self { channel, entries: Vec::new() };
		watch.poll();
		watch
	}

	pub fn poll(&mut self) {
		if self.channel == 0 {
			return;
		}
		let mut frame = [0u8; 256];
		loop {
			match try_recv_caps(self.channel, &mut frame) {
				PolledCaps::Message { len, mut handles } => {
					let info = provider_catalogue::subscribe_read(&frame[..len], &mut handles);
					for handle in handles.as_slice() {
						close(*handle);
					}
					let Some(info) = info else { continue };
					self.entries.retain(|held| !same_provider(held, &info));
					if info.live {
						self.entries.push(info);
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.channel);
					self.channel = 0;
					self.entries.clear();
					break;
				}
			}
		}
	}
}

pub fn open_provider(catalogue: u64, info: &ProviderInfo) -> u64 {
	match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(info) {
		Some(Ok(handle)) => handle,
		_ => 0,
	}
}
