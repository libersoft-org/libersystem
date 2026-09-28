//! GENERAL-PURPOSE EVENTS, as the kernel keeps them: the FADT's GPE0 and GPE1 blocks - each a status half and an
//! enable half, one bit per event - driven by the SCI handler and by the ACPI service's requests.
//!
//! THE SPLIT. The kernel owns the registers and the two masks - which events are enabled for runtime, which arm the
//! machine's wake - and changes them only at the service's request. The SCI handler MASKS each asserted enabled
//! event and LATCHES its number for the service; the service runs `_Lxx` or `_Exx` and asks for the event to be
//! acknowledged (an edge event's status before the method, a level event's after) and enabled again. STORMS ARE
//! PER EVENT: one that asserts again past the bound within the window is left disabled and reported, and the SCI
//! and every other event go on.
//!
//! Pure over a register trait, so the whole state machine is host-tested over simulated blocks.

use alloc::vec::Vec;

/// The registers, by block (0 or 1) and byte offset within the block.
pub trait Registers {
	fn read(&mut self, block: usize, offset: u16) -> u8;
	fn write(&mut self, block: usize, offset: u16, value: u8);
}

/// One block: how many bytes each half has, and the number of its first event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
	pub half_bytes: u16,
	pub base: u16,
}

/// Deliveries within the window past which an event is a storm.
pub const STORM_BOUND: u32 = 256;
/// The storm window, in the caller's clock units (ticks).
pub const STORM_WINDOW: u64 = 100;

#[derive(Clone, Copy, Debug, Default)]
struct Count {
	window_start: u64,
	deliveries: u32,
}

/// Why a request about one event was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// No block holds that event number.
	NoSuchEvent,
	/// The event stormed and stays disabled for the rest of the boot.
	Stormed,
	/// The event is not enabled for runtime.
	NotRuntime,
}

pub struct Gpes {
	blocks: Vec<Layout>,
	/// Runtime-enabled events, by number.
	runtime: Vec<bool>,
	/// Wake-armed events, by number.
	wake: Vec<bool>,
	/// Masked by the handler and waiting for the service's re-enable.
	latched: Vec<bool>,
	stormed: Vec<bool>,
	counts: Vec<Count>,
	count: usize,
}

impl Gpes {
	/// The blocks a FADT declares. `half_bytes` is half the declared length of each.
	pub fn new(blocks: &[Layout]) -> Gpes {
		let count = blocks.iter().map(|block| block.base as usize + 8 * block.half_bytes as usize).max().unwrap_or(0);
		Gpes { blocks: blocks.to_vec(), runtime: alloc::vec![false; count], wake: alloc::vec![false; count], latched: alloc::vec![false; count], stormed: alloc::vec![false; count], counts: alloc::vec![Count::default(); count], count }
	}

	/// How many event numbers the blocks cover.
	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	/// Where an event's bit is: its block, the byte within a half, and the bit.
	fn locate(&self, gpe: u16) -> Option<(usize, u16, u8)> {
		for (index, block) in self.blocks.iter().enumerate() {
			if gpe >= block.base && ((gpe - block.base) as usize) < 8 * block.half_bytes as usize {
				let within = gpe - block.base;
				return Some((index, within / 8, (within % 8) as u8));
			}
		}
		None
	}

	fn set_enable(&self, registers: &mut dyn Registers, gpe: u16, on: bool) {
		let Some((block, byte, bit)) = self.locate(gpe) else { return };
		let offset = self.blocks[block].half_bytes + byte;
		let current = registers.read(block, offset);
		let next = if on { current | 1 << bit } else { current & !(1 << bit) };
		registers.write(block, offset, next);
	}

	/// AT INIT, before the SCI is routed: every enable bit cleared and every status bit acknowledged.
	pub fn initialize(&mut self, registers: &mut dyn Registers) {
		for (index, block) in self.blocks.iter().enumerate() {
			for byte in 0..block.half_bytes {
				registers.write(index, block.half_bytes + byte, 0);
				registers.write(index, byte, 0xFF);
			}
		}
		for flag in self.runtime.iter_mut().chain(self.wake.iter_mut()).chain(self.latched.iter_mut()) {
			*flag = false;
		}
	}

	/// THE SCI HANDLER'S PART: every asserted ENABLED event masked and latched; answered in order, for the service.
	/// An event past its storm bound is masked, marked, and answered in the second list instead.
	pub fn handle(&mut self, registers: &mut dyn Registers, now: u64) -> (Vec<u16>, Vec<u16>) {
		let mut delivered = Vec::new();
		let mut stormed = Vec::new();
		self.handle_into(registers, now, &mut |gpe, storm| if storm { stormed.push(gpe) } else { delivered.push(gpe) });
		(delivered, stormed)
	}

	/// The same, each event handed to `out` as it is found - `true` for a storm - so an interrupt handler allocates
	/// nothing.
	pub fn handle_into(&mut self, registers: &mut dyn Registers, now: u64, out: &mut dyn FnMut(u16, bool)) {
		for index in 0..self.blocks.len() {
			let block = self.blocks[index];
			for byte in 0..block.half_bytes {
				let status = registers.read(index, byte);
				let enable = registers.read(index, block.half_bytes + byte);
				let pending = status & enable;
				if pending == 0 {
					continue;
				}
				// MASKED FIRST: a level event still asserting would otherwise raise the SCI again at once.
				registers.write(index, block.half_bytes + byte, enable & !pending);
				for bit in 0..8u16 {
					if pending & 1 << bit == 0 {
						continue;
					}
					let gpe = block.base + byte * 8 + bit;
					let slot = gpe as usize;
					let count = &mut self.counts[slot];
					if now.saturating_sub(count.window_start) > STORM_WINDOW {
						*count = Count { window_start: now, deliveries: 0 };
					}
					count.deliveries += 1;
					if count.deliveries > STORM_BOUND {
						self.stormed[slot] = true;
						self.latched[slot] = false;
						out(gpe, true);
						continue;
					}
					self.latched[slot] = true;
					out(gpe, false);
				}
			}
		}
	}

	/// ACKNOWLEDGE one event's status - an edge event's before its method, a level event's after.
	pub fn acknowledge(&self, registers: &mut dyn Registers, gpe: u16) -> Result<(), Refusal> {
		let (block, byte, bit) = self.locate(gpe).ok_or(Refusal::NoSuchEvent)?;
		registers.write(block, byte, 1 << bit);
		Ok(())
	}

	/// ENABLE AGAIN after the service ran the event's method: refused for a stormed event and one not enabled for
	/// runtime.
	pub fn rearm(&mut self, registers: &mut dyn Registers, gpe: u16) -> Result<(), Refusal> {
		self.locate(gpe).ok_or(Refusal::NoSuchEvent)?;
		let slot = gpe as usize;
		if self.stormed[slot] {
			return Err(Refusal::Stormed);
		}
		if !self.runtime[slot] {
			return Err(Refusal::NotRuntime);
		}
		self.latched[slot] = false;
		self.set_enable(registers, gpe, true);
		Ok(())
	}

	/// The service asks: enable (`true`) or disable an event for runtime.
	pub fn set_runtime(&mut self, registers: &mut dyn Registers, gpe: u16, on: bool) -> Result<(), Refusal> {
		self.locate(gpe).ok_or(Refusal::NoSuchEvent)?;
		let slot = gpe as usize;
		if on && self.stormed[slot] {
			return Err(Refusal::Stormed);
		}
		self.runtime[slot] = on;
		if !self.latched[slot] {
			self.set_enable(registers, gpe, on);
		}
		Ok(())
	}

	/// The service asks: arm (`true`) or disarm an event for wake. Applied to the enable register when the machine
	/// goes to sleep, by the sleep path; recorded here.
	pub fn set_wake(&mut self, gpe: u16, on: bool) -> Result<(), Refusal> {
		self.locate(gpe).ok_or(Refusal::NoSuchEvent)?;
		self.wake[gpe as usize] = on;
		Ok(())
	}

	/// THE INSTANCE THAT ENABLED THEM IS GONE: every runtime enable cleared in the register and in the mask, and every
	/// latch dropped - the next instance enables what its namespace handles. A stormed event stays stormed, and the
	/// wake mask stays as the sleep path armed it.
	pub fn reset_runtime(&mut self, registers: &mut dyn Registers) {
		for gpe in 0..self.count {
			if self.runtime[gpe] || self.latched[gpe] {
				self.set_enable(registers, gpe as u16, false);
			}
			self.runtime[gpe] = false;
			self.latched[gpe] = false;
		}
	}

	/// The events enabled for runtime.
	pub fn runtime_enabled(&self) -> Vec<u16> {
		(0..self.count).filter(|&gpe| self.runtime[gpe]).map(|gpe| gpe as u16).collect()
	}

	pub fn wake_armed(&self) -> Vec<u16> {
		(0..self.count).filter(|&gpe| self.wake[gpe]).map(|gpe| gpe as u16).collect()
	}

	pub fn is_stormed(&self, gpe: u16) -> bool {
		self.stormed.get(gpe as usize).copied().unwrap_or(false)
	}

	pub fn is_latched(&self, gpe: u16) -> bool {
		self.latched.get(gpe as usize).copied().unwrap_or(false)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::vec;
	use std::vec::Vec;

	/// Two blocks as byte arrays: status then enable, with status bits cleared by writing ones.
	struct Blocks {
		bytes: Vec<Vec<u8>>,
		halves: Vec<u16>,
		writes: Vec<(usize, u16, u8)>,
	}

	impl Blocks {
		fn new(halves: &[u16]) -> Blocks {
			Blocks { bytes: halves.iter().map(|half| vec![0u8; 2 * *half as usize]).collect(), halves: halves.to_vec(), writes: Vec::new() }
		}
		fn assert_event(&mut self, block: usize, byte: u16, bit: u8) {
			self.bytes[block][byte as usize] |= 1 << bit;
		}
		fn enable(&self, block: usize, byte: u16) -> u8 {
			self.bytes[block][(self.halves[block] + byte) as usize]
		}
	}

	impl Registers for Blocks {
		fn read(&mut self, block: usize, offset: u16) -> u8 {
			self.bytes[block][offset as usize]
		}
		fn write(&mut self, block: usize, offset: u16, value: u8) {
			self.writes.push((block, offset, value));
			if offset < self.halves[block] {
				self.bytes[block][offset as usize] &= !value;
			} else {
				self.bytes[block][offset as usize] = value;
			}
		}
	}

	fn setup() -> (Gpes, Blocks) {
		let layouts = [Layout { half_bytes: 2, base: 0 }, Layout { half_bytes: 1, base: 0x20 }];
		(Gpes::new(&layouts), Blocks::new(&[2, 1]))
	}

	#[test]
	fn init_clears_every_enable_and_acknowledges_every_status() {
		let (mut gpes, mut blocks) = setup();
		blocks.bytes[0] = vec![0xFF, 0x0F, 0xAA, 0x55];
		gpes.initialize(&mut blocks);
		assert_eq!(blocks.bytes[0], vec![0, 0, 0, 0]);
		assert_eq!(gpes.len(), 0x28);
	}

	#[test]
	fn an_enabled_event_is_masked_latched_and_enabled_again_only_when_asked() {
		let (mut gpes, mut blocks) = setup();
		gpes.initialize(&mut blocks);
		gpes.set_runtime(&mut blocks, 0x02, true).unwrap();
		gpes.set_runtime(&mut blocks, 0x21, true).unwrap();
		assert_eq!(blocks.enable(0, 0), 0b100);
		// Asserted: 0x02 enabled, 0x03 not, 0x21 enabled.
		blocks.assert_event(0, 0, 2);
		blocks.assert_event(0, 0, 3);
		blocks.assert_event(1, 0, 1);
		let (delivered, stormed) = gpes.handle(&mut blocks, 0);
		assert_eq!(delivered, vec![0x02, 0x21]);
		assert!(stormed.is_empty());
		assert_eq!(blocks.enable(0, 0), 0, "masked until the service asks");
		assert!(gpes.is_latched(0x02));
		// A runtime change while latched does not unmask it behind the service's back.
		gpes.set_runtime(&mut blocks, 0x02, true).unwrap();
		assert_eq!(blocks.enable(0, 0), 0);
		// The level event's method ran: acknowledged after, then enabled again.
		gpes.acknowledge(&mut blocks, 0x02).unwrap();
		assert_eq!(blocks.bytes[0][0] & 0b100, 0);
		gpes.rearm(&mut blocks, 0x02).unwrap();
		assert_eq!(blocks.enable(0, 0), 0b100);
		assert_eq!(gpes.rearm(&mut blocks, 0x03), Err(Refusal::NotRuntime), "an event never enabled for runtime is not rearmed");
		assert_eq!(gpes.acknowledge(&mut blocks, 0x30), Err(Refusal::NoSuchEvent));
	}

	#[test]
	fn an_ended_instance_leaves_nothing_enabled_and_nothing_latched() {
		let (mut gpes, mut blocks) = setup();
		gpes.initialize(&mut blocks);
		gpes.set_runtime(&mut blocks, 0x02, true).unwrap();
		gpes.set_runtime(&mut blocks, 0x09, true).unwrap();
		gpes.set_wake(0x21, true).unwrap();
		blocks.assert_event(0, 0, 2);
		let (delivered, _) = gpes.handle(&mut blocks, 0);
		assert_eq!(delivered, vec![0x02]);
		assert_eq!(gpes.runtime_enabled(), vec![0x02, 0x09]);
		gpes.reset_runtime(&mut blocks);
		assert_eq!((blocks.enable(0, 0), blocks.enable(0, 1)), (0, 0), "every runtime enable cleared");
		assert!(!gpes.is_latched(0x02), "the latch dropped, so the next instance's enable is not held back by it");
		assert!(gpes.runtime_enabled().is_empty());
		assert_eq!(gpes.wake_armed(), vec![0x21], "the wake mask is the sleep path's and stays");
		// The next instance enables it, and the register follows at once.
		gpes.set_runtime(&mut blocks, 0x02, true).unwrap();
		assert_eq!(blocks.enable(0, 0), 0b100);
	}

	#[test]
	fn a_storm_is_per_event_and_leaves_the_others_running() {
		let (mut gpes, mut blocks) = setup();
		gpes.initialize(&mut blocks);
		gpes.set_runtime(&mut blocks, 0x05, true).unwrap();
		gpes.set_runtime(&mut blocks, 0x06, true).unwrap();
		let mut storm_seen = false;
		for _ in 0..=STORM_BOUND {
			blocks.assert_event(0, 0, 5);
			let (_, stormed) = gpes.handle(&mut blocks, 10);
			if stormed.contains(&0x05) {
				storm_seen = true;
				break;
			}
			gpes.acknowledge(&mut blocks, 0x05).unwrap();
			gpes.rearm(&mut blocks, 0x05).unwrap();
		}
		assert!(storm_seen, "the storm was named");
		assert!(gpes.is_stormed(0x05));
		assert_eq!(gpes.rearm(&mut blocks, 0x05), Err(Refusal::Stormed));
		assert_eq!(gpes.set_runtime(&mut blocks, 0x05, true), Err(Refusal::Stormed));
		assert_eq!(blocks.enable(0, 0) & 0b10_0000, 0, "left disabled");
		// The neighbour is untouched and still delivers.
		blocks.assert_event(0, 0, 6);
		let (delivered, _) = gpes.handle(&mut blocks, 11);
		assert_eq!(delivered, vec![0x06]);
	}

	#[test]
	fn deliveries_spread_over_windows_are_no_storm() {
		let (mut gpes, mut blocks) = setup();
		gpes.initialize(&mut blocks);
		gpes.set_runtime(&mut blocks, 0x01, true).unwrap();
		for round in 0..(3 * STORM_BOUND as u64) {
			blocks.assert_event(0, 0, 1);
			let now = round * (STORM_WINDOW + 1) / 100;
			let (_, stormed) = gpes.handle(&mut blocks, now * 200);
			assert!(stormed.is_empty(), "round {round}");
			gpes.acknowledge(&mut blocks, 0x01).unwrap();
			gpes.rearm(&mut blocks, 0x01).unwrap();
		}
	}

	#[test]
	fn wake_arming_is_recorded_per_event() {
		let (mut gpes, _) = setup();
		gpes.set_wake(0x03, true).unwrap();
		gpes.set_wake(0x22, true).unwrap();
		gpes.set_wake(0x03, false).unwrap();
		assert_eq!(gpes.wake_armed(), vec![0x22]);
		assert_eq!(gpes.set_wake(0x40, true), Err(Refusal::NoSuchEvent));
	}
}
