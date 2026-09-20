// Fixed-hardware platform events, and the one channel they are delivered on.
//
// WHY THIS IS NOT AN INTERRUPT HANDED TO A DRIVER. The events here arrive on a SHARED,
// LEVEL-TRIGGERED line whose source register must be acknowledged before the interrupt is ended -
// an unacknowledged level source re-asserts immediately and the machine makes no further progress.
// That acknowledgement is a read and a write of a register the firmware describes in a table, in an
// address space the public driver resource vocabulary has no object for, and it has to happen inside
// the handler. So the kernel is already holding the register when the event is decoded, and what is
// left to hand on is the EVENT rather than the line. See `arch::sci` for the decision and its
// reasoning.
//
// ONE CHANNEL, ATTACHED BY A PRIVILEGED PROCESS, which is the shape `device::attach_events` and
// `console_input::attach` already have: the holder hands the kernel a Channel it may send on, and
// the kernel pushes typed messages into it. A second attach replaces the first, so a restarted
// holder takes over rather than finding the slot occupied by a channel nobody reads.
//
// AND THE SEND HAPPENS ON THE IDLE PASS, NOT IN THE HANDLER. This is the one structural rule in
// this file: `report` is called from an interrupt and does nothing but set a bit, and `deliver` is
// called from the idle pass and does everything else. `report` used to send, and a machine with a
// listener attached WEDGED on the first press - see `report`.
//
// AND A PRESS BEFORE ANYBODY LISTENS IS NOT LOST. The button can be pressed while userspace is
// still coming up, and a machine that ignored it would be a machine whose power button does nothing
// for the first few seconds with nothing saying why. The kinds seen before an attach are latched,
// and delivered once when one arrives.

use alloc::vec::Vec;

use crate::sync::SpinLock;

/// What a platform event says. One byte, because that is all any of these carries: the event IS the
/// message, and a power button has no payload.
///
/// THE POWER BUTTON AND THE SLEEP BUTTON ARE DIFFERENT KINDS AND NOT ONE WITH A FLAG. They are
/// separate bits in the same status register, they mean different things, and this system can act on
/// exactly one of them - a holder that received "a button" would have to ask which, and the answer
/// would have been thrown away here.
pub const POWER_BUTTON: u8 = 1;
pub const SLEEP_BUTTON: u8 = 2;

static LISTENER: SpinLock<Option<alloc::sync::Arc<crate::object::channel::Channel>>> = SpinLock::new(None);

/// Kinds that happened before anybody attached, as a bitmap over the kinds above.
///
/// A BITMAP AND NOT A COUNT. Two presses of the power button before userspace is up are one
/// instruction to power off, not two; what must not be lost is that it was pressed at all.
static PENDING: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn bit(kind: u8) -> u32 {
	1u32 << (kind as u32 & 31)
}

/// Take this channel as the one platform events are delivered on.
///
/// NOTHING IS DELIVERED HERE. The latch is emptied by `deliver` on the idle pass, which is the one
/// context in this module allowed to touch a channel - see `report`.
pub fn attach(channel: alloc::sync::Arc<crate::object::channel::Channel>) {
	*LISTENER.lock() = Some(channel);
}

/// Record that `kind` happened. **CALLED FROM AN INTERRUPT HANDLER, AND THAT IS THE WHOLE OF WHY
/// THIS FUNCTION DOES SO LITTLE.**
///
/// It used to do the delivery itself: take the listener's lock, allocate a message and call
/// `Channel::send`, which takes the peer's inbox lock and then `sched::wake_object`. Every one of
/// those is a lock an ordinary thread can be holding at the moment a hardware interrupt arrives, and
/// this one arrives on a SHARED line at an arbitrary instruction boundary. Measured, not reasoned
/// about: with a listener attached the guest printed `acpi: the power button was pressed` and then
/// made no further progress - the press never came out the other end, the shell never answered, and
/// the harness's teardown did not complete. With NO listener attached the same handler returned
/// cleanly, because the `None` arm never reached the send. That is the difference between the two
/// halves of this file, and it is why the delivery moved to the idle pass.
///
/// A BITMAP AND NOT A COUNT. Two presses of the power button before the delivery pass are one
/// instruction to power off, not two; what must not be lost is that it was pressed at all.
///
/// **WHERE IT IS REACHED FROM, SAID IN A CFG RATHER THAN SUPPRESSED.** One caller is `arch::sci`,
/// which is x86_64's ACPI SCI handler - the only source of fixed-hardware events any port has today
/// - and the other is the hardware suite, which raises one directly to prove the latch. On the two
/// device-tree ports a shipping kernel therefore reaches this from nowhere, and a `dead_code`
/// warning there is CORRECT: it says this system has no fixed-hardware event source on them yet.
/// The day one arrives - a PSCI or SBI event, a GPIO button - its port adds itself here.
#[cfg(any(target_arch = "x86_64", test))]
pub fn report(kind: u8) {
	PENDING.fetch_or(bit(kind), core::sync::atomic::Ordering::AcqRel);
}

/// Hand whatever is latched to the listener. Called from the idle pass, where allocating and
/// sending on a channel is ordinary work.
///
/// A KIND IS TAKEN BEFORE IT IS SENT AND PUT BACK IF IT CANNOT BE. Nothing may be listening yet -
/// the button can be pressed while userspace is still coming up - and a machine that dropped that
/// press would be one whose power button does nothing for the first seconds of every boot with
/// nothing saying why.
///
/// A FAILED SEND IS NOT PUT BACK. The listener exists and its queue is full or its end has closed,
/// which is a listener that is not reading rather than one that is not there; latching would deliver
/// the press to whoever attached next, minutes later, as if the button had just been pressed.
/// A delivered event nothing has taken yet: which kind, and the tick its consumer stops getting the
/// benefit of the doubt.
static UNREAD: SpinLock<Option<(u8, u64)>> = SpinLock::new(None);
static UNREAD_SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// How long a listener has to take an event before this says that it did not.
///
/// A PERSON'S PATIENCE AND NOT A PROTOCOL DEADLINE. Nothing is retried when it passes and nothing is
/// cancelled: an event still queued three seconds after it was handed on is an event whose consumer
/// is not reading it, and the one useful thing to do about that is SAY SO - once, with what every
/// blocked thread in the machine was waiting on at the time.
const UNREAD_TICKS: u64 = 300;

/// "It was delivered" and "it was acted on" are two answers, and this is what stops them looking
/// like one in the log.
///
/// DRIVEN BY THE IDLE PASS, like the delivery itself: `deliver` calls it before it looks at its own
/// latch, so a pass with nothing to deliver still checks what the last delivery did.
fn check_unread() {
	let due: Option<u8> = {
		let mut slot = UNREAD.lock();
		match *slot {
			Some((kind, at)) if crate::arch::apic::ticks() >= at => {
				*slot = None;
				Some(kind)
			}
			_ => None,
		}
	};
	let Some(kind) = due else { return };
	// ALLOC-OK: a refcount bump out of the guard, so nothing below runs under the lock.
	let Some(channel) = LISTENER.lock().clone() else { return };
	let Some(queued) = channel.peer_unread() else { return };
	if queued == 0 {
		return;
	}
	// SAID ONCE. A consumer that is not reading is not reading, and a line per idle pass would bury
	// the boot log it is meant to be read in.
	if !UNREAD_SAID.swap(true, core::sync::atomic::Ordering::AcqRel) {
		crate::serial_println!("platform: event {kind} reached its listener and NOBODY READ IT - {queued} message(s) still queued {UNREAD_TICKS} tick(s) later, so the button works and its consumer does not");
		// NOT IN THE TEST KERNEL, WHICH DOES NOT CARRY IT. `dump_blocked` is `cfg(not(test))` because
		// its one other caller is the panic handler the test build replaces - and a symbol a second
		// compilation does not have turns this line into a build failure of the whole test kernel.
		#[cfg(not(test))]
		crate::sched::dump_blocked("a platform event nobody read");
	}
}

pub fn deliver() {
	// WHAT THE LAST DELIVERY CAME TO, BEFORE THIS ONE'S LATCH IS EVEN READ. See `check_unread`.
	check_unread();
	let latched = PENDING.load(core::sync::atomic::Ordering::Acquire);
	if latched == 0 {
		return;
	}
	// ALLOC-OK: an `Option<Arc<Channel>>` out of the guard - a refcount bump, not a copy. Cloned out
	// so the send below does not run under the lock.
	let Some(channel) = LISTENER.lock().clone() else {
		// SAID ONCE AND NOT ON EVERY IDLE PASS. A button pressed before userspace is up does nothing
		// visible for as long as the boot takes, and a log with nothing in it about the press is how
		// that becomes "the power button does not work".
		if !HELD_SAID.swap(true, core::sync::atomic::Ordering::AcqRel) {
			crate::serial_println!("platform: a fixed-hardware event arrived before anything was listening - it is held until something does");
		}
		return;
	};
	for kind in [POWER_BUTTON, SLEEP_BUTTON] {
		if PENDING.fetch_and(!bit(kind), core::sync::atomic::Ordering::AcqRel) & bit(kind) == 0 {
			continue;
		}
		let mut bytes: Vec<u8> = Vec::new();
		if bytes.try_reserve_exact(1).is_err() {
			crate::serial_println!("platform: event {kind} could not be delivered - no memory for a one-byte message");
			continue;
		}
		bytes.push(kind);
		// A REFUSED SEND IS SAID AND NOT SWALLOWED, so "the event never arrived", "it arrived and
		// could not be delivered" and "it was delivered and the consumer did nothing" stay three
		// distinguishable answers.
		match channel.send(crate::object::channel::Message::new(bytes, Vec::new())) {
			// NAMED AT BOTH ENDS, BECAUSE "IT WAS SENT" IS NOT AN ANSWER WHEN IT DOES NOT ARRIVE. This
			// line said only that the send succeeded, and a day went into establishing what it now
			// states in eight characters: which object the message left and which object it reached.
			Ok(()) => {
				crate::serial_println!("platform: event {kind} handed on, from object {} to {:?}", crate::object::KernelObject::header(&*channel).koid(), channel.peer_koid());
				// AND WHEN TO COME BACK AND SEE WHETHER IT WAS ANY USE. See `check_unread`.
				*UNREAD.lock() = Some((kind, crate::arch::apic::ticks().saturating_add(UNREAD_TICKS)));
			}
			Err(error) => crate::serial_println!("platform: event {kind} could not be delivered to its listener ({error:?})"),
		}
	}
}

/// Whether the "held" note has been said, so it is said once rather than on every idle pass.
static HELD_SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn drain_for_test() -> u32 {
	*LISTENER.lock() = None;
	HELD_SAID.store(false, core::sync::atomic::Ordering::Relaxed);
	UNREAD_SAID.store(false, core::sync::atomic::Ordering::Relaxed);
	*UNREAD.lock() = None;
	PENDING.swap(0, core::sync::atomic::Ordering::AcqRel)
}
