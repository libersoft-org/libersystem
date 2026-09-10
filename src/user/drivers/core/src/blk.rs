// The block request contract, as pure decisions: what a block driver refuses before it touches
// its device, tested on the host through the crate's seam.
//
// TWO DEBTS LIVE HERE, AND EACH IS A FUNCTION SO A TEST CAN HOLD IT.
//
// The count and the range (DRV-002). The block path used to CLAMP a request's sector count into
// the device's bounds and never looked at the LBA at all, so a request past the end of the disk
// went to the device as written and a request too large became a smaller one nobody asked for. A
// clamp turns a wrong request into a wrong WRITE, which is worse than a refusal: the caller believes
// its bytes landed where it said. Both paths refuse instead, with a typed status.
//
// The transferred object (DRV-003 / WIRE-002). A write carries a memory object the client filled,
// and the driver computed `count * 512`, checked that its DMA span could hold that many bytes, and
// copied that many bytes OUT of the mapped object without ever asking how big the object was, what
// it was, or whether the handle could be read. `count = 1000` against a 512-byte object drove a
// 512 kB read out of a 512-byte mapping. The three questions are one `object_info` call, answered
// here from the answer.
use rt::{OBJECT_TYPE_MEMORY_OBJECT, ObjectInfo, RIGHT_READ};

// One sector, the unit every field of the wire is counted in.
pub const SECTOR: u64 = 512;

// Why a request was refused. Every arm is a decision the caller made wrong, never a device fault:
// the driver replies with the typed status and the device is never asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	// A zero count, or one above what a single request to this device may carry.
	Count,
	// The request runs past the last sector the device has (or overflows the address).
	Range,
	// The transferred handle is not a memory object.
	ObjectType,
	// The transferred handle cannot be read.
	Rights,
	// The object is smaller than the bytes the request says it holds.
	Size,
}

// The sector range a request names, admitted or refused. `count` is the wire value as sent - no
// clamp happens before this; `max_sectors` is what one request to this device may carry and
// `capacity_sectors` is the device's size. Returns the admitted count.
pub fn request_range(lba: u64, count: u32, capacity_sectors: u64, max_sectors: u64) -> Result<u32, Refusal> {
	if count == 0 || count as u64 > max_sectors {
		return Err(Refusal::Count);
	}
	let end = lba.checked_add(count as u64).ok_or(Refusal::Range)?;
	if end > capacity_sectors {
		return Err(Refusal::Range);
	}
	Ok(count)
}

// The three-part contract on a write's source object: a memory object, readable through this
// handle, and at least `bytes` long. `info` is what `object_info` answered for the transferred
// handle; the caller has not mapped anything yet.
pub fn write_source(info: &ObjectInfo, bytes: u64) -> Result<(), Refusal> {
	if info.object_type != OBJECT_TYPE_MEMORY_OBJECT {
		return Err(Refusal::ObjectType);
	}
	if info.rights & RIGHT_READ == 0 {
		return Err(Refusal::Rights);
	}
	if info.size < bytes {
		return Err(Refusal::Size);
	}
	Ok(())
}

#[cfg(test)]
mod tests;
