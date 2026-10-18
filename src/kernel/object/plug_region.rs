// PlugRegion: THE AUTHORITY OVER ONE REGION OF PLUGGED MEMORY - see `mem::hotplug`. Minted by `SYS_MEMORY_PLUG_REGION` for
// the binding of a virtio-mem device, and derived from it: its driver brings the blocks its host plugged online and
// takes them offline when the host asks, and nobody else has a reason to.
//
// IT OWNS NO MEMORY. The region is the kernel's, and what is online stays online when the last handle closes: the
// frames may be anybody's by then, and a driver's ending is no reason to take a program's memory. A restarted driver
// asks for the same range and is handed the same region, with what is online still online.

use alloc::sync::Arc;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};

pub struct PlugRegion {
	header: ObjectHeader,
	index: usize,
}

impl PlugRegion {
	// FALLIBLY: `SYS_MEMORY_PLUG_REGION` reaches this.
	pub fn new(index: usize) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), index })
	}

	// The region in `mem::hotplug` this authority is over.
	pub fn index(&self) -> usize {
		self.index
	}
}

impl_kernel_object!(PlugRegion, PlugRegion);
