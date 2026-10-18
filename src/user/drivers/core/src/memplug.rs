// VIRTIO-MEM'S WIRE AND DECISIONS (virtio 1.2, device 24): the requests a driver plugs and unplugs blocks with, the
// answers it reads back, the configuration, and which blocks one step moves - the parts of the driver a host can test.
//
// THE ORDER OF A STEP. Below the target - the smallest of what the host requests, what the policy allows and what the
// region can use - the lowest offline blocks are plugged, a run at a time. Above what the host requests, the highest
// online blocks are offered back, one at a time, and the kernel takes back only those with no frame in use; the rest
// stay plugged and are counted as in use.

// The request and answer types.
pub const REQ_PLUG: u16 = 0;
pub const REQ_UNPLUG: u16 = 1;
pub const REQ_UNPLUG_ALL: u16 = 2;
pub const REQ_STATE: u16 = 3;
pub const RESP_ACK: u16 = 0;
pub const RESP_NACK: u16 = 1;
pub const RESP_BUSY: u16 = 2;
pub const RESP_ERROR: u16 = 3;
// What a STATE request answers for a range.
pub const STATE_PLUGGED: u16 = 0;
pub const STATE_UNPLUGGED: u16 = 1;
pub const STATE_MIXED: u16 = 2;

// `VIRTIO_MEM_F_ACPI_PXM` and `VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE`: this driver never touches an unplugged block, so it
// takes the second whenever it is offered.
pub const FEATURE_ACPI_PXM: u32 = 1 << 0;
pub const FEATURE_UNPLUGGED_INACCESSIBLE: u32 = 1 << 1;

pub const REQUEST_LEN: usize = 24;
pub const RESPONSE_LEN: usize = 10;

// The most blocks one plug moves.
pub const PLUG_BATCH: usize = 16;

// The configuration's layout.
pub const CONFIG_BLOCK_SIZE: u64 = 0;
pub const CONFIG_ADDR: u64 = 16;
pub const CONFIG_REGION_SIZE: u64 = 24;
pub const CONFIG_USABLE_REGION_SIZE: u64 = 32;
pub const CONFIG_PLUGGED_SIZE: u64 = 40;
pub const CONFIG_REQUESTED_SIZE: u64 = 48;
pub const CONFIG_LEN: usize = 56;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
	pub block_size: u64,
	pub addr: u64,
	pub region_size: u64,
	pub usable_region_size: u64,
	pub plugged_size: u64,
	pub requested_size: u64,
}

impl Config {
	pub fn parse(bytes: &[u8]) -> Option<Config> {
		if bytes.len() < CONFIG_LEN {
			return None;
		}
		let u64_at = |at: u64| u64::from_le_bytes(bytes[at as usize..at as usize + 8].try_into().unwrap_or([0; 8]));
		Some(Config { block_size: u64_at(CONFIG_BLOCK_SIZE), addr: u64_at(CONFIG_ADDR), region_size: u64_at(CONFIG_REGION_SIZE), usable_region_size: u64_at(CONFIG_USABLE_REGION_SIZE), plugged_size: u64_at(CONFIG_PLUGGED_SIZE), requested_size: u64_at(CONFIG_REQUESTED_SIZE) })
	}

	// A region this driver can drive: a block of at least a page and a power of two, a base and size of whole blocks.
	pub fn usable(&self) -> bool {
		self.block_size >= 4096 && self.block_size.is_power_of_two() && self.region_size != 0 && self.addr % self.block_size == 0 && self.region_size % self.block_size == 0
	}

	pub fn blocks(&self) -> usize {
		(self.region_size / self.block_size) as usize
	}

	// The blocks the host lets this machine plug now: the usable part of the region, whole blocks.
	pub fn usable_blocks(&self) -> usize {
		((self.usable_region_size.min(self.region_size)) / self.block_size) as usize
	}
}

// ONE REQUEST: its type, and the address and block count it names (none for UNPLUG_ALL).
pub fn request(kind: u16, addr: u64, blocks: u16) -> [u8; REQUEST_LEN] {
	let mut out = [0u8; REQUEST_LEN];
	out[0..2].copy_from_slice(&kind.to_le_bytes());
	if kind != REQ_UNPLUG_ALL {
		out[8..16].copy_from_slice(&addr.to_le_bytes());
		out[16..18].copy_from_slice(&blocks.to_le_bytes());
	}
	out
}

// ONE ANSWER: its type, and for a STATE request the state.
pub fn response(bytes: &[u8]) -> Option<(u16, u16)> {
	if bytes.len() < RESPONSE_LEN {
		return None;
	}
	let kind = u16::from_le_bytes([bytes[0], bytes[1]]);
	if kind > RESP_ERROR {
		return None;
	}
	Some((kind, u16::from_le_bytes([bytes[8], bytes[9]])))
}

// One step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
	Hold,
	// A run of offline blocks to plug: the first and how many.
	Plug(usize, usize),
	// The highest online block to offer back.
	Unplug(usize),
}

// THE NEXT STEP for a region whose `online` blocks are as given, the host requesting `requested` blocks, the policy
// allowing `allowed` and `usable` blocks plugable; `held` are blocks offered back this round whose frames were in use,
// and are not offered again until the next round.
pub fn step(online: &[bool], requested: usize, allowed: usize, usable: usize, held: &[bool]) -> Step {
	let plugged = online.iter().filter(|online| **online).count();
	let target = requested.min(allowed).min(usable).min(online.len());
	if plugged < target {
		let Some(first) = online[..usable.min(online.len())].iter().position(|online| !*online) else { return Step::Hold };
		let run = online[first..usable.min(online.len())].iter().take_while(|online| !**online).count();
		return Step::Plug(first, run.min(target - plugged).min(PLUG_BATCH));
	}
	if plugged > requested.min(allowed) {
		// FROM THE TOP DOWN, skipping what was in use this round.
		if let Some(block) = (0..online.len()).rev().find(|&block| online[block] && !held.get(block).copied().unwrap_or(false)) {
			return Step::Unplug(block);
		}
	}
	Step::Hold
}

#[cfg(test)]
mod tests;
