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
pub mod privilege;
pub mod process;
pub mod process_group;
pub mod rights;
pub mod thread;
pub mod timer;
pub mod wait_set;

#[cfg(test)]
pub(crate) mod tests;

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
	ProcessGroup,
	// A named authority with no object of its own - the display, the console's input
	// sink, the console's input source. See `privilege`.
	Privilege,
	// A set of objects registered once and waited on many times. See `wait_set`.
	WaitSet,
}

impl ObjectType {
	// A short, stable name for this type (used by introspection and the graph).
	pub fn name(self) -> &'static str {
		match self {
			ObjectType::WaitSet => "WaitSet",
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
			ObjectType::ProcessGroup => "ProcessGroup",
			ObjectType::Privilege => "Privilege",
		}
	}

	// A stable numeric code for this type, carried across the syscall boundary by
	// object_info_get (the wire-stable index, distinct from the in-memory enum).
	pub fn code(self) -> u64 {
		// The values are `abi`'s, not this file's: they are documented as stable ABI codes that
		// userspace reads out of `ObjectInfo`, so they belong where userspace can see them.
		match self {
			ObjectType::Domain => abi::OBJECT_TYPE_DOMAIN,
			ObjectType::Process => abi::OBJECT_TYPE_PROCESS,
			ObjectType::Thread => abi::OBJECT_TYPE_THREAD,
			ObjectType::AddressSpace => abi::OBJECT_TYPE_ADDRESS_SPACE,
			ObjectType::MemoryObject => abi::OBJECT_TYPE_MEMORY_OBJECT,
			ObjectType::Channel => abi::OBJECT_TYPE_CHANNEL,
			ObjectType::Event => abi::OBJECT_TYPE_EVENT,
			ObjectType::Timer => abi::OBJECT_TYPE_TIMER,
			ObjectType::Interrupt => abi::OBJECT_TYPE_INTERRUPT,
			ObjectType::DeviceMemory => abi::OBJECT_TYPE_DEVICE_MEMORY,
			ObjectType::DmaBuffer => abi::OBJECT_TYPE_DMA_BUFFER,
			ObjectType::ProcessGroup => abi::OBJECT_TYPE_PROCESS_GROUP,
			ObjectType::Privilege => abi::OBJECT_TYPE_PRIVILEGE,
			ObjectType::WaitSet => abi::OBJECT_TYPE_WAIT_SET,
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
	//
	// FALLIBLY, because the name comes from ring 3: `String::from` aborts the kernel on a short
	// heap, and a label is the least important thing in the system to halt for. A refused name
	// leaves the object unnamed, which is what it was a moment ago.
	pub fn set_name(&self, name: &str) {
		let Some(owned) = crate::mem::heap::try_string(name) else { return };
		*self.name.lock() = Some(owned);
	}

	// This object's label, if one was set.
	//
	// BORROWED, not cloned. `name()` returned `Option<String>` - a fresh heap allocation for every
	// reader - and the reader that matters is the ring-3 fault handler, which names the process it
	// is about to terminate. So a faulting process on a short heap took the kernel through an
	// allocation while it was handling the fault, which is the worst moment this kernel has to ask
	// for memory: the request that caused the pressure is the one being cleaned up.
	//
	// The lock is held across the closure, so the callee must not name another object - which no
	// caller does and none should: this answers one question.
	pub fn with_name<R>(&self, f: impl FnOnce(Option<&str>) -> R) -> R {
		let guard = self.name.lock();
		f(guard.as_deref())
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

// Implement the `KernelObject` boilerplate for a type with a `header: ObjectHeader`
// field. Every object's `header`/`as_any`/`into_any_arc` bodies are byte-identical;
// only `object_type` varies. Use as `impl_kernel_object!(Channel, Channel);` (the
// type, then its `ObjectType` variant). The bodies use the names each object module
// already imports (KernelObject, ObjectHeader, ObjectType, Any, Arc).
macro_rules! impl_kernel_object {
	($ty:ty, $variant:ident) => {
		impl KernelObject for $ty {
			fn header(&self) -> &ObjectHeader {
				&self.header
			}
			fn object_type(&self) -> ObjectType {
				ObjectType::$variant
			}
			fn as_any(&self) -> &dyn Any {
				self
			}
			fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
				self
			}
		}
	};
}
pub(crate) use impl_kernel_object;
