// Kernel random source.
//
// Uses the CPU's RDRAND instruction - an on-chip hardware DRBG (a CSPRNG) - when
// the CPU advertises it (CPUID.01H:ECX.RDRAND, bit 30). RDRAND can transiently
// fail under contention, so each draw retries a bounded number of times. When
// RDRAND is absent the kernel falls back to a SplitMix64 PRNG seeded from the
// timestamp counter; that fallback is NOT cryptographic and only covers
// environments without RDRAND (e.g. an old QEMU CPU model) - real targets have it.

use core::arch::x86_64::{__cpuid, _rdrand64_step};
use core::sync::atomic::{AtomicU64, Ordering};

// Whether the running CPU advertises RDRAND (CPUID leaf 1, ECX bit 30).
fn has_rdrand() -> bool {
	let info = __cpuid(1);
	info.ecx & (1 << 30) != 0
}

// Draw a 64-bit value from RDRAND, retrying a bounded number of times (RDRAND
// signals a transient failure by returning 0 in the carry flag). Returns None if
// every retry failed. The caller must have confirmed RDRAND is available.
#[target_feature(enable = "rdrand")]
unsafe fn rdrand64() -> Option<u64> {
	let mut val: u64 = 0;
	for _ in 0..16 {
		if _rdrand64_step(&mut val) == 1 {
			return Some(val);
		}
	}
	None
}

// Non-cryptographic fallback: a SplitMix64 generator whose state is advanced on
// each draw and lazily seeded from the timestamp counter. Used only when RDRAND
// is unavailable.
static FALLBACK_STATE: AtomicU64 = AtomicU64::new(0);

fn fallback_u64() -> u64 {
	let mut seed = FALLBACK_STATE.load(Ordering::Relaxed);
	if seed == 0 {
		seed = super::tsc::now() | 1;
	}
	let out = crate::arch::common::rng::splitmix64(&mut seed);
	FALLBACK_STATE.store(seed, Ordering::Relaxed);
	out
}

// Draw the next 64-bit random value (RDRAND if available, else the fallback).
fn next_u64() -> u64 {
	if has_rdrand() {
		// SAFETY: has_rdrand() confirmed the CPU supports RDRAND.
		unsafe { rdrand64() }.unwrap_or_else(fallback_u64)
	} else {
		fallback_u64()
	}
}

// Whether this machine has a source fit for a key.
pub fn secure_available() -> bool {
	has_rdrand()
}

// Fill `buf` from the hardware source, or answer false if there is none.
//
// The two sources are now two FUNCTIONS, because they were one and userspace could not tell which
// it had been given. A caller that needs a key gets hardware or an error; a caller that needs a
// distinguishable number asks for `insecure` by that name.
pub fn secure(buf: &mut [u8]) -> bool {
	if !has_rdrand() {
		return false;
	}
	let mut i = 0;
	while i < buf.len() {
		// SAFETY: `has_rdrand` said the instruction is there.
		let Some(value) = (unsafe { rdrand64() }) else {
			// RDRAND signalling failure through every retry is a source that is not answering, and
			// quietly finishing the buffer from the formula is exactly the substitution this split
			// exists to prevent.
			return false;
		};
		let bytes = value.to_le_bytes();
		let n = (buf.len() - i).min(8);
		buf[i..i + n].copy_from_slice(&bytes[..n]);
		i += n;
	}
	true
}

// Fill `buf` from the deterministic generator. Always succeeds, never suitable for a secret.
pub fn insecure(buf: &mut [u8]) {
	let mut i = 0;
	while i < buf.len() {
		let bytes = fallback_u64().to_le_bytes();
		let n = (buf.len() - i).min(8);
		buf[i..i + n].copy_from_slice(&bytes[..n]);
		i += n;
	}
}
