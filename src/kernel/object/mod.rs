// Generic kernel-object base: identity, type, lifetime, and revocation.
//
// Every kernel object embeds an ObjectHeader and implements KernelObject. Objects
// are reference-counted through Arc: an object lives as long as a capability (in
// some handle table) or a message in transit holds a reference. Revocation is
// O(1) via a generation counter in the header, compared at capability lookup.

#![allow(dead_code)]

pub mod address_space;
pub mod channel;
pub mod device_memory;
pub mod dma_buffer;
pub mod domain;
pub mod event;
pub mod handle;
pub mod interrupt;
pub mod memory_object;
pub mod process;
pub mod rights;
pub mod thread;
pub mod timer;

use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::sync::SpinLock;

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

impl ObjectType {
	// A short, stable name for this type (used by introspection and the graph).
	pub fn name(self) -> &'static str {
		match self {
			ObjectType::Domain => "Domain",
			ObjectType::Process => "Process",
			ObjectType::Thread => "Thread",
			ObjectType::AddressSpace => "AddressSpace",
			ObjectType::MemoryObject => "MemoryObject",
			ObjectType::Channel => "Channel",
			ObjectType::Event => "Event",
			ObjectType::Timer => "Timer",
			ObjectType::Interrupt => "Interrupt",
			ObjectType::DeviceMemory => "DeviceMemory",
			ObjectType::DmaBuffer => "DmaBuffer",
		}
	}

	// A stable numeric code for this type, carried across the syscall boundary by
	// object_info_get (the wire-stable index, distinct from the in-memory enum).
	pub fn code(self) -> u64 {
		match self {
			ObjectType::Domain => 0,
			ObjectType::Process => 1,
			ObjectType::Thread => 2,
			ObjectType::AddressSpace => 3,
			ObjectType::MemoryObject => 4,
			ObjectType::Channel => 5,
			ObjectType::Event => 6,
			ObjectType::Timer => 7,
			ObjectType::Interrupt => 8,
			ObjectType::DeviceMemory => 9,
			ObjectType::DmaBuffer => 10,
		}
	}
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
	// Optional human-readable label set via object_property_set, for the System
	// Graph and debugging. None until named.
	name: SpinLock<Option<String>>,
}

impl ObjectHeader {
	pub fn new() -> Self {
		Self { koid: next_koid(), generation: AtomicU32::new(1), name: SpinLock::new(None) }
	}

	// Stable, unique identity for this object (useful for debugging and info).
	pub fn koid(&self) -> u64 {
		self.koid
	}

	// Set this object's human-readable label.
	pub fn set_name(&self, name: &str) {
		*self.name.lock() = Some(String::from(name));
	}

	// This object's label, if one was set.
	pub fn name(&self) -> Option<String> {
		self.name.lock().clone()
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
	// Recover the concrete type from an owning reference: after a typed handle
	// lookup, `obj.into_any_arc().downcast::<T>()` yields an `Arc<T>`. Needed by the
	// handlers that must own a typed Arc (e.g. spawning a thread into a looked-up
	// Process), which `as_any` (a borrow) cannot provide.
	fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}
