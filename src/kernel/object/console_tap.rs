// ConsoleTap: the kernel console's output, for the driver that holds its UART.
//
// While the kernel drives the console UART it drains its own transmit ring - every kernel line and every
// `SYS_DEBUG_WRITE` caller's bytes - onto the wire. When a claim takes the UART, the ring stays the kernel's
// output queue at its bound and THE TAP IS HOW IT LEAVES: a derived object of that claim, which moves bytes
// out of the ring in order and says how many the bound dropped since the last read. The driver writes them to
// the wire, and a one-line marker for a non-zero count.
//
// IT IS SIGNALLED the way an `Interrupt` is - pending, and its waiters woken - when the ring goes from empty to
// holding bytes. The kernel line that does that may be printed under any lock, and waking a thread takes the
// scheduler's, so the signal is delivered from a context that holds none: the timer tick, the idle loop and
// the debug-write syscall (`arch::serial`'s `deliver_tap_signal`).
//
// DERIVED FROM THE CLAIM: the release revokes it, a revoked tap reads nothing and is never pending, and the
// UART is the kernel's again before the claim is free.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
#[cfg(target_arch = "x86_64")]
use crate::sched;

pub struct ConsoleTap {
	header: ObjectHeader,
	// The UART whose ring this drains, by its base.
	base: u64,
	// The claim generation that holds it.
	generation: u64,
	pending: AtomicBool,
	revoked: AtomicBool,
}

// THE DEVELOPMENT REQUEST THAT HOLDS EVERY TAP'S READS, so the handoff gate can fill the ring past its bound
// while the driver keeps serving - the case the terminal-path writer exists for. Compiled into the
// development build alone.
#[cfg(liber_development)]
static HELD: AtomicBool = AtomicBool::new(false);

#[cfg(liber_development)]
pub fn hold_reads(held: bool) {
	HELD.store(held, Ordering::SeqCst);
}

impl ConsoleTap {
	// FALLIBLY: `SYS_DEVICE_RESOURCE_ACQUIRE` reaches this.
	pub fn new(base: u64, generation: u64) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), base, generation, pending: AtomicBool::new(false), revoked: AtomicBool::new(false) })
	}

	// The ring holds bytes: pending, and every waiter woken. COM1's tap, x86_64's alone.
	#[cfg(target_arch = "x86_64")]
	pub fn signal(&self) {
		if self.revoked.load(Ordering::Acquire) {
			return;
		}
		self.pending.store(true, Ordering::Release);
		sched::wake_object(self.header.koid());
	}

	// The wait readiness. A revoked tap is never pending.
	pub fn is_pending(&self) -> bool {
		!self.revoked.load(Ordering::Acquire) && self.pending.load(Ordering::Acquire)
	}

	// MOVE BYTES OUT OF THE RING into `buf`: how many, and how many the bound dropped since the last read.
	// `None` once the tap is revoked or its claim no longer holds the UART. Pending again when the read left
	// bytes behind - the reader was given less room than the ring held.
	pub fn read(&self, buf: &mut [u8]) -> Option<(usize, u64)> {
		if self.revoked.load(Ordering::Acquire) {
			return None;
		}
		// Cleared BEFORE the ring is read, so a byte queued after the read signals again.
		self.pending.store(false, Ordering::Release);
		#[cfg(liber_development)]
		if HELD.load(Ordering::SeqCst) {
			return Some((0, 0));
		}
		let (n, dropped, left) = crate::arch::serial::console_tap_read(self.base, self.generation, buf)?;
		if left != 0 {
			self.pending.store(true, Ordering::Release);
		}
		Some((n, dropped))
	}

	// THE CLAIM WAS RELEASED: nothing more is read through this tap, and nothing wakes on it.
	pub fn revoke(&self) {
		self.revoked.store(true, Ordering::Release);
		self.pending.store(false, Ordering::Release);
	}
}

impl_kernel_object!(ConsoleTap, ConsoleTap);
