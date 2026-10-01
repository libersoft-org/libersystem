// LatencyRequest: A BOUND ON EVERY CORE'S IDLE STATES, for as long as it lives - see `crate::processor`. Minted by
// `SYS_LATENCY_REQUEST` under the `IdleLatency` privilege; the idle governor enters no state whose exit latency exceeds
// the smallest live request's bound. It ends when its last handle closes, so a holder that dies releases it.

use alloc::sync::Arc;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};

// The bound itself is `processor`'s, counted under this object's koid: the object is what makes the request end.
pub struct LatencyRequest {
	header: ObjectHeader,
}

impl LatencyRequest {
	// FALLIBLY: `SYS_LATENCY_REQUEST` reaches this; it then counts the request under this object's koid.
	pub fn new() -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new() })
	}
}

impl Drop for LatencyRequest {
	fn drop(&mut self) {
		crate::processor::latency_ended(self.header.koid());
	}
}

impl_kernel_object!(LatencyRequest, LatencyRequest);
