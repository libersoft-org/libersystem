// Portable timekeeping policy + arithmetic shared by every arch backend.
//
// The periodic scheduler-tick RATE is a policy the whole kernel shares; only how each
// backend programs its timer to fire at that rate is arch-specific (the LAPIC on x86,
// CNTP on aarch64, the CLINT on riscv64). Likewise the cycles->ns conversion each
// backend's fine cycle clock reports latency through is pure arithmetic - the only
// arch-specific part is where the frequency comes from (a calibrated TSC, CNTFRQ_EL0,
// the device tree's timebase-frequency).

// The periodic scheduler-tick rate, in Hz. Each backend programs its timer to fire at
// this rate; the portable scheduler counts these ticks as its monotonic coarse clock.
pub const TICK_HZ: u32 = 100;

// Convert a raw cycle count to nanoseconds at frequency `hz` (cycles per second). The
// u128 intermediate keeps `cycles * 1e9` from overflowing; an uncalibrated clock
// (hz == 0) reports 0 rather than dividing by zero.
pub fn cycles_to_ns(cycles: u64, hz: u64) -> u64 {
	if hz == 0 {
		return 0;
	}
	(cycles as u128 * 1_000_000_000 / hz as u128) as u64
}
