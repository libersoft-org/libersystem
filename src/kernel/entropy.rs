// THE MACHINE'S ENTROPY POOL, AND THE ONE CONCLUSION IT WILL NOT DRAW.
//
// `SYS_RANDOM_GET` used to have exactly one answer on a machine with no hardware random instruction:
// `ERR_UNSUPPORTED`. That is honest and it is also most of this system's machines - two of the three
// architectures have no such instruction at all - so anything wanting key material had nowhere to go,
// and the only thing that always answered was `SYS_RANDOM_INSECURE`, which says in its own name that
// it is not that.
//
// A paravirtual entropy device closes that gap and must not be allowed to close it too far. The host
// may be replaying a recording; the device may be backed by a file; a guest resumed from a snapshot
// is handed a device about to produce exactly what it produced before the snapshot. None of it is
// visible from inside the guest. So the bytes are a SEED - worth having, worth crediting below their
// length - and never a statement about this machine's cryptographic health. `entropy::Pool` holds
// that policy, in a crate outside the kernel where a host test can watch it refuse; this file is the
// machine's single instance of it and the authority rule for who may add to it.
//
// THE AUTHORITY IS THE DEVICE CAPABILITY, AND THE CURRENT CLAIM ON IT. A capability from a previous
// binding is held by somebody who is no longer driving that device, and the device type is checked
// as well: a claim on a NIC is not a licence to seed the machine's randomness.

use crate::device;
use crate::sync::SpinLock;
use entropy::{Pool, Source};

static POOL: SpinLock<Pool> = SpinLock::new(Pool::new());

// Take a submission from a driver that has proved its claim, and answer what it was credited.
pub fn absorb(bytes: &[u8], source: Source) -> u32 {
	POOL.lock().absorb(bytes, source)
}

// Fill `out` from the pool, or answer false because the pool has not been seeded.
//
// FALSE IS THE ANSWER, NOT WEAK BYTES: a caller that asked for key material and got a buffer it
// cannot distinguish from good key material has no way to act on the difference.
pub fn draw(out: &mut [u8]) -> bool {
	POOL.lock().draw(out)
}

pub fn seeded() -> bool {
	POOL.lock().seeded()
}

// What the pool holds, in the shape the syscall hands out. Counts and provenance; no verdict.
pub fn health(hardware_available: bool) -> abi::EntropyHealth {
	let held = POOL.lock().health();
	abi::EntropyHealth { credited_bits: held.credited_bits, submissions: held.submissions, paravirtual_submissions: held.paravirtual_submissions, hardware_submissions: held.hardware_submissions, draws: held.draws, seeded: u8::from(held.seeded), hardware_available: u8::from(hardware_available), _pad: [0u8; 6] }
}

// WHETHER THIS CAPABILITY MAY SEED THE MACHINE.
//
// Three questions, and all three have to be yes: the capability names a device-table entry, the claim
// it was minted under is the one that is CURRENT, and the device it names is an entropy device. The
// middle one is the same rule `SYS_DEVICE_QUIESCED` applies - a stale capability is a statement about
// a machine that has moved on - and the last one is what keeps a claim on any device at all from
// being a licence to decide what this machine's randomness is made of.
pub fn source_for(claim: Option<abi::ClaimKey>) -> Option<Source> {
	let key = claim?;
	if !device::claim_is_current(key) {
		return None;
	}
	match device::device_type_at(key.device_index as usize) {
		Some(device_type) if u32::from(device_type) == abi::VIRTIO_TYPE_RNG => Some(Source::Paravirtual),
		_ => None,
	}
}
