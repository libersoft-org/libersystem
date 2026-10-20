// Balloon: FREE FRAMES HELD FOR A VIRTUAL MACHINE'S HOST, so that nothing in this kernel reuses them while the host has
// them. Minted by `SYS_BALLOON_CREATE` for the binding of a virtio-balloon device, and only for that: its driver tells
// the host which frames it may take back, and nobody else has a reason to hold one.
//
// WHAT IT NEVER TAKES. Only a frame the allocator has free - never one a process, a buffer, a mapping or a device holds -
// and never below the floor the allocator keeps for itself (`frame::balloon_floor`). So whatever the host asks for, it
// is given memory nobody is using, and no Domain is ever made to give anything up: a pressure policy above this decides
// how much the balloon may hold, and this object holds no more than the policy's driver asks for and the floor allows.
//
// WHAT IT GIVES BACK. A frame it holds, on request - and every one it holds when its last handle closes, so a driver
// that dies or is stopped returns them all.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sync::SpinLock;

// THE BINDING IT WAS MINTED FOR is recorded where every derived capability is (`device::register_derived`): ending it
// revokes the capability, and closing the last handle returns the frames.
pub struct Balloon {
	header: ObjectHeader,
	// THE FRAMES IT HOLDS, sorted by address so a give can be checked against them.
	frames: SpinLock<Vec<u64>>,
}

// Why a give was refused: nothing is given then.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GiveRefusal {
	// A frame the balloon does not hold, or one named twice.
	NotHeld,
}

impl Balloon {
	// FALLIBLY: `SYS_BALLOON_CREATE` reaches this.
	pub fn new() -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), frames: SpinLock::new(Vec::new()) })
	}

	// UP TO `out.len()` FREE FRAMES TAKEN, their addresses written to `out`: the count, which stops at the floor, at a
	// heap that cannot record one more, or at `out`'s length.
	pub fn take(&self, out: &mut [u64]) -> usize {
		let mut frames = self.frames.lock();
		let mut taken = 0;
		while taken < out.len() {
			if frames.try_reserve(1).is_err() {
				break;
			}
			let Some(frame) = crate::mem::frame::balloon_take() else { break };
			let at = frames.partition_point(|held| *held < frame);
			frames.insert(at, frame);
			out[taken] = frame;
			taken += 1;
		}
		taken
	}

	// THE FRAMES NAMED given back, all of them or none: a frame this balloon does not hold, or one named twice, refuses
	// the whole request, so a driver's mistake never returns a frame twice or one that is somebody else's.
	pub fn give(&self, named: &[u64]) -> Result<usize, GiveRefusal> {
		let mut frames = self.frames.lock();
		let mut positions: Vec<usize> = Vec::new();
		if positions.try_reserve_exact(named.len()).is_err() {
			return Err(GiveRefusal::NotHeld);
		}
		for frame in named {
			let Ok(at) = frames.binary_search(frame) else { return Err(GiveRefusal::NotHeld) };
			if positions.contains(&at) {
				return Err(GiveRefusal::NotHeld);
			}
			positions.push(at);
		}
		positions.sort_unstable();
		for &at in positions.iter().rev() {
			let frame = frames.remove(at);
			// SAFETY: a frame `balloon_take` handed this balloon, removed from it just now, so returned exactly once.
			unsafe { crate::mem::frame::balloon_return(frame) };
		}
		Ok(named.len())
	}

	// How many frames it holds.
	#[cfg(test)]
	pub fn held(&self) -> usize {
		self.frames.lock().len()
	}
}

impl Drop for Balloon {
	fn drop(&mut self) {
		for frame in self.frames.lock().drain(..) {
			// SAFETY: as in `give` - the balloon's own frames, each returned once, as it ends.
			unsafe { crate::mem::frame::balloon_return(frame) };
		}
	}
}

impl_kernel_object!(Balloon, Balloon);

#[cfg(test)]
mod tests;
