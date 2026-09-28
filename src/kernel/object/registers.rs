// Registers: a claim's DECLARED REGISTERS, reached one at a time at exactly their width - see `crate::declared`.
//
// The object names the claim it was minted from and nothing else: which registers it reaches is the claimed row's
// declaration, and every access is refused once that claim is not the device's current binding. It is derived from
// the claim, so the release revokes it - and the release writes nothing to any register.

use alloc::sync::Arc;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};

pub struct Registers {
	header: ObjectHeader,
	key: abi::ClaimKey,
}

impl Registers {
	// FALLIBLY: `SYS_DEVICE_RESOURCE_ACQUIRE` reaches this.
	pub fn new(key: abi::ClaimKey) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), key })
	}

	pub fn key(&self) -> abi::ClaimKey {
		self.key
	}
}

impl_kernel_object!(Registers, Registers);
