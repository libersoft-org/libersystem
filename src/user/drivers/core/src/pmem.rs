// PERSISTENT MEMORY SERVED AS BLOCKS: whether a namespace takes writes, how a write is made durable before it is
// answered, and where one request's bytes lie - the parts of `driver.nvdimm` a host can test.
//
// THE PERSISTENCE ORDERING. A write is answered only after its bytes are durable, and what makes them durable is the
// platform's word in the NFIT's capabilities: where the processor's caches are flushed on a power loss, a store is
// durable once it is globally visible, so the copy and one fence are the whole write; where only the memory controller
// is - or the table says nothing, which is what a platform older than the capabilities structure is - every cache line
// the write touched is written back and then the fence runs. A platform that names neither domain promises nothing a
// store could reach, and its namespace is served read-only rather than answered as written.
//
// THE MODULES' STATE. A module that is not armed will not save what is written to it, so its namespace is served
// read-only too. A failed save, restore or platform flush, or SMART events, are said in the driver's line and do not
// change what is served: the contents may be stale, and the reader is told, but refusing the writes would also refuse
// the repair.
//
// POISON. A line the media lost raises a machine check when it is read, and this kernel recovers none - so a namespace
// is never mapped into a client, where a read would stop the machine with no driver between, and the bad-block list an
// address-range scrub would give is not read: the root device's `_DSM` that runs one is a firmware interface no emulator
// here implements, so nothing could prove it.

use acpi::nfit::{self, Namespace};

// One sector, the block wire's unit; and the most one request may carry - 128 KiB, the bound the other storage drivers
// publish.
pub const SECTOR: u64 = 512;
pub const MAX_SECTORS: u64 = 256;

// The cache line, where the processor does not say: 64 bytes on every x86_64 processor this targets.
pub const LINE: usize = 64;

// How a write becomes durable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Durability {
	// The processor's caches are inside the persistence domain: the copy, then a fence.
	Visible,
	// Only the memory controller is: the copy, every line it touched written back, then a fence.
	Flushed,
}

// Why a namespace is served read-only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadOnly {
	// A module backing it is not armed.
	NotArmed,
	// The platform names no persistence domain a store reaches.
	NoDomain,
}

// What a namespace is served as: writable with a durability, or read-only for a reason.
pub fn plan(namespace: &Namespace) -> Result<Durability, ReadOnly> {
	if namespace.state & nfit::STATE_NOT_ARMED != 0 {
		return Err(ReadOnly::NotArmed);
	}
	match namespace.capabilities {
		Some(bits) if bits & nfit::CAPABILITY_CPU_CACHE_FLUSH != 0 => Ok(Durability::Visible),
		Some(bits) if bits & nfit::CAPABILITY_MEMORY_CONTROLLER_FLUSH != 0 => Ok(Durability::Flushed),
		Some(_) => Err(ReadOnly::NoDomain),
		None => Ok(Durability::Flushed),
	}
}

// The state flags the driver's line names, each with its words.
pub const STATE_WORDS: [(u16, &[u8]); 4] = [
	(nfit::STATE_SAVE_FAILED, b"the last save failed"),
	(nfit::STATE_RESTORE_FAILED, b"the last restore failed"),
	(nfit::STATE_FLUSH_FAILED, b"the platform's last flush failed"),
	(nfit::STATE_SMART_EVENTS, b"SMART health events observed"),
];

// What the processor does to make a span durable: write a line back, and order everything before against everything
// after. The driver's is the instructions; a host test's records them.
pub trait Persist {
	fn flush(&mut self, address: usize);
	fn fence(&mut self);
}

// Where a request's bytes lie in the namespace: the byte offset and length of `count` sectors from `lba`, or None when
// any part of it is past `capacity` bytes or does not fit an address.
pub fn span(lba: u64, count: u32, capacity: u64) -> Option<(usize, usize)> {
	let offset = lba.checked_mul(SECTOR)?;
	let len = u64::from(count).checked_mul(SECTOR)?;
	if offset.checked_add(len)? > capacity {
		return None;
	}
	Some((usize::try_from(offset).ok()?, usize::try_from(len).ok()?))
}

// COPY `src` INTO THE NAMESPACE AT `offset`, AND MAKE IT DURABLE, in the order that makes the answer true: the bytes,
// then - where only the memory controller is in the persistence domain - every line the span touches written back, then
// one fence. The caller answers after this returns, and not before. False, and nothing written, for a span past the
// namespace.
//
// # Safety
// `pmem` is the start of a writable mapping of `len` bytes that nothing else writes while this runs.
pub unsafe fn write_durably(pmem: *mut u8, len: usize, offset: usize, src: &[u8], durability: Durability, line: usize, persist: &mut impl Persist) -> bool {
	if offset.checked_add(src.len()).is_none_or(|end| end > len) || line == 0 || !line.is_power_of_two() {
		return false;
	}
	// SAFETY: inside the mapping, as checked; the source is a slice the caller holds and not this mapping.
	unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), pmem.add(offset), src.len()) };
	if durability == Durability::Flushed && !src.is_empty() {
		let start = (pmem as usize + offset) & !(line - 1);
		let end = pmem as usize + offset + src.len();
		let mut at = start;
		while at < end {
			persist.flush(at);
			at += line;
		}
	}
	persist.fence();
	true
}

// Copy the namespace's bytes at `offset` out into `dst`. False, and nothing read, for a span past the namespace.
//
// # Safety
// `pmem` is the start of a readable mapping of `len` bytes.
pub unsafe fn read(pmem: *const u8, len: usize, offset: usize, dst: &mut [u8]) -> bool {
	if offset.checked_add(dst.len()).is_none_or(|end| end > len) {
		return false;
	}
	// SAFETY: inside the mapping, as checked.
	unsafe { core::ptr::copy_nonoverlapping(pmem.add(offset), dst.as_mut_ptr(), dst.len()) };
	true
}

// The namespace record out of a row's property block: one VALUE record named `NFIT`.
pub fn record(block: &[u8]) -> Option<Namespace> {
	if block.len() < 12 || block[0] != rt::DEVICE_PROPERTY_VALUE {
		return None;
	}
	let name_len = u16::from_le_bytes([block[2], block[3]]) as usize;
	let value_len = u32::from_le_bytes([block[4], block[5], block[6], block[7]]) as usize;
	if name_len != 4 || &block[8..12] != b"NFIT" || value_len != nfit::RECORD_LEN || block.len() < 12 + value_len {
		return None;
	}
	Namespace::decode(&block[12..12 + value_len])
}

#[cfg(test)]
mod tests;
