// SplitMix64 - the tiny non-cryptographic PRNG mixer the arch random fallbacks share.
//
// No architecture guarantees a hardware RNG on the bring-up core (x86 RDRAND is
// CPUID-gated, aarch64 FEAT_RNG / RNDR is optional, riscv64 likewise), so each backend
// falls back to a SplitMix64 stream seeded from its cycle counter. It is adequate for
// the kernel's non-cryptographic bring-up needs (a real entropy source replaces it
// later); centralizing it keeps the magic constants - easy to mistype and silently
// weaken - in exactly one place.

// Advance a SplitMix64 `state` by one step and return the mixed output: add the
// golden-ratio increment to the running seed, then avalanche-mix it. The caller owns
// the seeding (each backend derives the initial `state` from its cycle counter).
pub fn splitmix64(state: &mut u64) -> u64 {
	*state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
	let mut z = *state;
	z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
	z ^ (z >> 31)
}
