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

// THE MACHINE'S ONE CLOCK, COMPUTED FROM THE FREE-RUNNING COUNTER WHEN IT IS READ.
//
// It used to be ADVANCED: whichever core's periodic timer interrupt first saw a whole period of the
// counter elapsed moved it on (KERN-ARCH-007's counter gate, which fixed three backends that had
// counted one tick per core per period). That tied time to an interrupt every core takes a hundred
// times a second, and a core that stops taking it when it has nothing to do would stop the clock. So the
// tick is the counter - the TSC, CNTVCT, the `time` CSR - less the sleep offset, less the anchor taken
// once the counter's frequency is known, divided by the cycles in one tick: nothing has to happen for
// time to pass, and an idle core's timer can be a one-shot. The arithmetic, the sleep offset and the
// suspended state are `tickclock`'s, where a host test drives them.
//
// ONE CLOCK FOR EVERY BACKEND, anchored by each backend where its counter's frequency becomes known.
pub static CLOCK: tickclock::Clock = tickclock::Clock::new();

// The ABI's tick rate is this kernel's and the clock's alike.
const _: () = assert!(TICK_HZ as u64 == tickclock::TICK_HZ);
