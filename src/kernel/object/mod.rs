// Generic kernel-object base: identity, type, lifetime, and revocation.
//
// Every kernel object embeds an ObjectHeader and implements KernelObject. Objects
// are reference-counted through Arc: an object lives as long as a capability (in
// some handle table) or a message in transit holds a reference. Revocation is
// O(1) via a generation counter in the header, compared at capability lookup.

#![allow(dead_code)]

pub mod address_space;
pub mod channel;
pub mod domain;
pub mod event;
pub mod handle;
pub mod memory_object;
pub mod process;
pub mod rights;
pub mod thread;
pub mod timer;

use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// The set of object types the kernel knows. The type is bound into every
// capability so the kernel can reject a wrongly-typed handle ("sealing").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectType {
	Domain,
	Process,
	Thread,
	AddressSpace,
	MemoryObject,
	Channel,
	Event,
	Timer,
	Interrupt,
	DeviceMemory,
	DmaBuffer,
}

// Unique kernel object id allocator (0 is reserved as "invalid").
static NEXT_KOID: AtomicU64 = AtomicU64::new(1);

fn next_koid() -> u64 {
	NEXT_KOID.fetch_add(1, Ordering::Relaxed)
}

// Common header embedded in every kernel object.
pub struct ObjectHeader {
	koid: u64,
	generation: AtomicU32,
}

impl ObjectHeader {
	pub fn new() -> Self {
		Self { koid: next_koid(), generation: AtomicU32::new(1) }
	}

	// Stable, unique identity for this object (useful for debugging and info).
	pub fn koid(&self) -> u64 {
		self.koid
	}

	// Current revocation generation. Capabilities snapshot this at mint time and
	// lookup compares, so a single bump invalidates every existing capability.
	pub fn generation(&self) -> u32 {
		self.generation.load(Ordering::Acquire)
	}

	// Invalidate all existing capabilities to this object (O(1) revocation).
	pub fn revoke(&self) {
		self.generation.fetch_add(1, Ordering::AcqRel);
	}
}

impl Default for ObjectHeader {
	fn default() -> Self {
		Self::new()
	}
}

// Implemented by every kernel object. Send + Sync because objects are shared
// across cores via Arc; Any allows recovering the concrete type after lookup.
pub trait KernelObject: Send + Sync + Any {
	fn header(&self) -> &ObjectHeader;
	fn object_type(&self) -> ObjectType;
	fn as_any(&self) -> &dyn Any;
}
