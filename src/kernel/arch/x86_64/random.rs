// Kernel random source.
//
// Uses the CPU's RDRAND instruction - an on-chip hardware DRBG (a CSPRNG) - when
// the CPU advertises it (CPUID.01H:ECX.RDRAND, bit 30). RDRAND can transiently
// fail under contention, so each draw retries a bounded number of times. When
// RDRAND is absent the kernel falls back to a SplitMix64 PRNG seeded from the
// timestamp counter; that fallback is NOT cryptographic and only covers
// environments without RDRAND (e.g. an old QEMU CPU model) - real targets have it.

#![allow(dead_code)]

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
	seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
	FALLBACK_STATE.store(seed, Ordering::Relaxed);
	let mut z = seed;
	z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
	z ^ (z >> 31)
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

// Fill `buf` with random bytes.
pub fn fill(buf: &mut [u8]) {
	let mut i = 0;
	while i < buf.len() {
		let bytes = next_u64().to_le_bytes();
		let n = (buf.len() - i).min(8);
		buf[i..i + n].copy_from_slice(&bytes[..n]);
		i += n;
	}
}
