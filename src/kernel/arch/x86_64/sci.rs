// ACPI fixed-hardware event delivery: the System Control Interrupt, and what arrives on it.
//
// **THE DECISION THIS MODULE IS.** A PM1a status/enable pair reached through a Generic Address
// Structure is either an interrupt the KERNEL routes and hands on, or a claimable firmware-described
// node a userspace driver takes. It is the first, and the reasons are not preferences:
//
//   1. PM1a IS FIXED HARDWARE AND NOT A DEVICE. It has no `_HID`, no address on any bus, and no
//      entry in any namespace - it is a register block whose address is a field of the FADT. There
//      is nothing to describe as a node, so "a claimable firmware-described node" would mean
//      inventing an identity for a thing the firmware does not give one to.
//   2. THE PUBLIC DRIVER RESOURCE VOCABULARY HAS NO PORT-I/O OBJECT. It is device MMIO, one IRQ,
//      keys, system power and console. Handing this block to a driver means a new public resource
//      kind - range-scoped and revocable - which is a widening of what a driver may hold, made to
//      unblock the item that wanted it.
//   3. AND THE LINE FORCES IT ANYWAY. The SCI is shared and level-triggered. A level source that is
//      not acknowledged before the interrupt is ended re-asserts at once and the machine makes no
//      further progress, so SOMETHING has to read and write the status register inside the handler.
//      The only candidate that can is the kernel. Once the kernel holds the register, handing the
//      LINE on as well would be handing on a line whose cause has already been consumed.
//
// So: the kernel routes the SCI, decodes the status register, acknowledges it, and reports a typed
// EVENT on `platform_event`'s channel. What acts on the event is somebody else's, and holds no
// authority over this register.
//
// **GENERAL-PURPOSE EVENTS ARE SPLIT WITH THE ACPI SERVICE.** A GPE's meaning is a control method in a
// namespace this kernel does not have, so the kernel keeps the registers - the FADT's GPE0 and GPE1 blocks - and
// the service keeps the meaning: at init every enable bit is cleared and every status bit acknowledged before the
// SCI is routed; in the handler each asserted ENABLED event is masked and its number latched for the service, whose
// event channel `firmware::deliver` feeds from the idle pass; an event is enabled again only when the service has
// run its `_Lxx` or `_Exx` and asks. Storms are per event (`acpi::gpe`). PM1's `GBL_STS` - the firmware released the
// global lock the service waits on - is decoded and delivered the same way.
//
// **WHAT IT DOES NOT DO.** It enters no sleep state and touches neither the PM timer nor the reset register - those
// are their own items.

use acpi::{Fadt, Madt, Polarity, Trigger};

use super::port;
use crate::sync::SpinLock;

/// What the FADT said, resolved once at boot.
#[derive(Clone, Copy)]
struct Blocks {
	/// PM1a's status register, and the enable register that follows it.
	a_status: u16,
	a_enable: u16,
	/// PM1b's, on the machines that have a second block. Most do not.
	b_status: Option<u16>,
	b_enable: Option<u16>,
	/// The GSI the SCI arrives at, which a storm masks.
	gsi: u32,
}

static BLOCKS: SpinLock<Option<Blocks>> = SpinLock::new(None);

/// The PM1 enable bits `init` armed, for the re-arm after an S3.
static ARMED: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);

/// PM1 status and enable bits. The two registers share a layout, which is what makes
/// "acknowledge what was enabled" expressible at all.
const PWRBTN: u16 = 1 << 8;
const SLPBTN: u16 = 1 << 9;
/// The firmware released the global lock while this system's pending bit was set: the waiter tries again.
const GBL: u16 = 1 << 5;

/// THE GENERAL-PURPOSE EVENT BLOCKS, as the port of each block's status half: its enable half follows it.
struct GpeIo {
	status: [u16; 2],
}

impl acpi::gpe::Registers for GpeIo {
	fn read(&mut self, block: usize, offset: u16) -> u8 {
		// SAFETY: a byte of the FADT's GPE block, which the kernel alone drives.
		unsafe { port::inb(self.status[block] + offset) }
	}

	fn write(&mut self, block: usize, offset: u16, value: u8) {
		// SAFETY: as above.
		unsafe { port::outb(self.status[block] + offset, value) }
	}
}

static GPES: SpinLock<Option<(acpi::gpe::Gpes, GpeIo)>> = SpinLock::new(None);

/// THE EVENTS THE HANDLER LATCHED FOR THE SERVICE, as bitmaps over the event number: delivered on the idle pass,
/// never from the handler (see `platform_event::report`). A latched event is masked until the service asks, so
/// each number is pending at most once.
const MAX_GPES: usize = 1024;
static PENDING: SpinLock<([u64; MAX_GPES / 64], [u64; MAX_GPES / 64])> = SpinLock::new(([0; MAX_GPES / 64], [0; MAX_GPES / 64]));
static GLOBAL_LOCK_RELEASED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// PM1 control bit 0: the machine is in ACPI mode and events arrive as an SCI rather than an SMI.
const SCI_EN: u16 = 1;

/// THE STORM BOUND, AND WHY IT IS A COUNT PER WINDOW RATHER THAN A RATE.
///
/// A status bit this kernel fails to clear - a firmware whose register does not answer a write-one,
/// a bit this decoder does not know about that is enabled anyway - is a line that re-asserts the
/// instant the interrupt ends, forever, at interrupt rate. That is not a slow machine: it is a
/// machine that executes nothing else ever again, and the only way out is from inside the handler.
/// So the handler counts what it could not silence and, past this many in one window, DISARMS the
/// enable register and says so. A power button that stops working is a defect; a machine that
/// stops is not recoverable.
const STORM_LIMIT: u32 = 64;

/// The window, in the APIC tick the scheduler already counts. A hundred ticks is a second on this
/// timer, and a human pressing a button cannot make sixty-four events in one.
const STORM_WINDOW_TICKS: u64 = 100;

static STORM_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static STORM_SINCE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Read the FADT, enter ACPI mode if the firmware has not, arm the buttons and route the SCI.
///
/// EVERY STEP CAN SAY NO, AND SAYS WHICH. A machine with no RSDP, no FADT, a hardware-reduced FADT,
/// a PM1 block in a space this does not reach, or an SCI the redirection entry cannot address, ends
/// with no interrupt armed and a line saying why - which is a machine whose power button does
/// nothing, and is a great deal better than one that routed a line it cannot acknowledge.
pub fn init(rsdp_phys: u64) {
	let Some(bytes) = crate::smp::acpi_table(rsdp_phys, b"FACP") else {
		crate::serial_println!("acpi: no FADT - fixed-hardware events are unavailable on this machine");
		return;
	};
	let fadt = match Fadt::new(bytes) {
		Ok(fadt) => fadt,
		Err(error) => {
			crate::serial_println!("acpi: the FADT is not readable ({error:?}) - fixed-hardware events are unavailable");
			return;
		}
	};
	// HARDWARE-REDUCED IS NOT A FAILURE. Such a machine has no PM1 blocks at all by its own
	// declaration, and its events arrive through structures described elsewhere. Saying so is the
	// whole of what this path can do about it.
	if fadt.hardware_reduced() {
		crate::serial_println!("acpi: this FADT is hardware-reduced - there is no PM1 event block and no SCI");
		return;
	}
	let Some(event) = fadt.pm1a_event() else {
		crate::serial_println!("acpi: the FADT describes no PM1a event block - fixed-hardware events are unavailable");
		return;
	};
	// A REGISTER THIS KERNEL CANNOT REACH IS REFUSED RATHER THAN GUESSED AT. The standard allows
	// the PM1 blocks in system memory, and every machine this runs on puts them in port I/O; a
	// reader that treated an address as a port because it expected one would be writing to a port
	// numbered by the low sixteen bits of a physical address.
	let Some(a) = event.io_port() else {
		crate::serial_println!("acpi: the PM1a event block is not a system-I/O register - this kernel reaches no other space for it");
		return;
	};
	// THE BLOCK IS TWO REGISTERS AND THE FADT DECLARES THEIR TOTAL. Status is the lower half and
	// enable the upper half, so the enable register is half a block past the base - which is the
	// one piece of arithmetic here that a reader gets wrong by writing the enable bits into the
	// status register, where writing a one ACKNOWLEDGES an event instead of arming it.
	let Some(a_enable) = event.enable_port() else {
		crate::serial_println!("acpi: the PM1a event block declares {} byte(s), which is not two registers", event.declared_bytes);
		return;
	};
	// PM1b IS BOTH REGISTERS OR NEITHER. A machine with a second block that this reader can only
	// half describe is one where arming what it can reach leaves a status bit nothing acknowledges,
	// which is the storm the bound below exists for.
	let (b_status, b_enable) = match fadt.pm1b_event() {
		Some(block) => match (block.io_port(), block.enable_port()) {
			(Some(status), Some(enable)) => (Some(status), Some(enable)),
			_ => (None, None),
		},
		None => (None, None),
	};
	// THE SCI'S GSI, resolved before the blocks are stored so a storm can mask it.
	let gsi = sci_gsi(&fadt, rsdp_phys).map_or(u32::MAX, |(gsi, _)| gsi);
	let blocks = Blocks { a_status: a, a_enable, b_status, b_enable, gsi };
	// THE GLOBAL LOCK'S EVENT ARMED WITH THE BUTTONS where the firmware has a FACS to hold the lock in.
	let armed = if fadt.facs().is_some() { PWRBTN | SLPBTN | GBL } else { PWRBTN | SLPBTN };

	// **THE ORDER OF THE FOUR STEPS BELOW IS THE WHOLE OF WHAT CAN GO WRONG HERE**, and each pair of
	// them is wrong in a different way round:
	//
	//   ACPI MODE BEFORE ANYTHING, because arming an event on a machine still in legacy mode arms
	//   nothing - the firmware answers the button with an SMI and the SCI is never raised.
	//
	//   THE HANDLER BEFORE THE ROUTING, because routing UNMASKS the redirection entry. The SCI is a
	//   SHARED line: something else on the same pin may be asserted already, and an unmasked level
	//   entry with no handler behind it is dispatched, end-of-interrupted and re-asserted at once,
	//   forever, with nothing in place that could disarm it. Registering first costs nothing - the
	//   handler reads a register whose enable bits are still zero - and is the difference between a
	//   storm the storm bound catches and a machine that never reaches the next line.
	//
	//   THE ACKNOWLEDGEMENT BEFORE THE ARMING, because firmware leaves status bits set across the
	//   handoff, and an enable written over a set status bit raises the interrupt immediately for an
	//   event that happened before this kernel existed.
	//
	//   AND THE ARMING LAST, because a source enabled while its line goes to a MASKED entry is still
	//   asserted when the entry is unmasked - which delivers a press nobody made.
	enter_acpi_mode(&fadt);
	*BLOCKS.lock() = Some(blocks);
	ARMED.store(armed, core::sync::atomic::Ordering::Relaxed);
	unsafe {
		port::outw(blocks.a_status, armed);
		if let Some(b) = blocks.b_status {
			port::outw(b, armed);
		}
	}
	// AND THE GENERAL-PURPOSE EVENT BLOCKS, BEFORE THE SCI IS ROUTED: every enable bit cleared and every status bit
	// acknowledged, so an event the firmware left enabled cannot raise the line before its owner exists.
	init_gpes(&fadt);
	let Some(vector) = route_sci_with_handler(&fadt, rsdp_phys) else {
		// NOTHING IS ARMED AND NOTHING IS LISTENING. The blocks are dropped so the handler - which
		// is registered on no vector here - could not act on a machine whose SCI this kernel
		// declined to route.
		*BLOCKS.lock() = None;
		*GPES.lock() = None;
		return;
	};
	unsafe {
		port::outw(blocks.a_enable, armed);
		if let Some(b) = blocks.b_enable {
			port::outw(b, armed);
		}
	}
	crate::serial_println!("acpi: power and sleep buttons armed on PM1 status {a:#06x}, enable {a_enable:#06x}, vector {vector}");
}

/// Enter ACPI mode if the firmware has not already.
///
/// MOST FIRMWARE HAS. QEMU hands the kernel a machine whose `SCI_EN` is already set and whose FADT
/// names an SMI command port anyway; a kernel that wrote the enable value unconditionally would be
/// asking a machine that is already in ACPI mode to enter it again, which is a legal request and a
/// pointless one. The read decides.
fn enter_acpi_mode(fadt: &Fadt<'_>) {
	let Some(control) = fadt.pm1a_control().and_then(|block| block.io_port()) else {
		return;
	};
	if unsafe { port::inw(control) } & SCI_EN != 0 {
		return;
	}
	let (Some(command), Some((enable, _disable))) = (fadt.smi_command(), fadt.acpi_mode_values()) else {
		crate::serial_println!("acpi: this machine is not in ACPI mode and its FADT names no SMI command port - fixed-hardware events will not arrive");
		return;
	};
	let Ok(command) = u16::try_from(command) else {
		crate::serial_println!("acpi: the SMI command port {command:#x} is not an I/O port address");
		return;
	};
	unsafe { port::outb(command, enable) };
	// THE HANDOFF IS NOT INSTANT AND THE STANDARD SAYS SO: the firmware takes the SMI, does its
	// work and sets the bit. A bounded wait is the difference between a machine that came up in
	// ACPI mode and one this kernel merely asked to.
	for _ in 0..100_000 {
		if unsafe { port::inw(control) } & SCI_EN != 0 {
			crate::serial_println!("acpi: entered ACPI mode through SMI command port {command:#06x}");
			return;
		}
		core::hint::spin_loop();
	}
	crate::serial_println!("acpi: the machine did not enter ACPI mode after the SMI command - fixed-hardware events may not arrive");
}

/// THE FADT'S GPE BLOCKS, taken over: each a status half and an enable half of the declared length, in system I/O -
/// a block in any other space, or of an odd length, is refused and said.
fn init_gpes(fadt: &Fadt<'_>) {
	let mut layouts = [acpi::gpe::Layout { half_bytes: 0, base: 0 }; 2];
	let mut io = GpeIo { status: [0; 2] };
	let mut count = 0usize;
	for (name, block) in [("GPE0", fadt.gpe0()), ("GPE1", fadt.gpe1())] {
		let Some(block) = block else { continue };
		let Some(status) = block.block.io_port() else {
			crate::serial_println!("acpi: the {name} block is not a system-I/O register block - its events are not handled");
			continue;
		};
		let bytes = block.block.declared_bytes as u16;
		if bytes < 2 || bytes % 2 != 0 {
			crate::serial_println!("acpi: the {name} block declares {bytes} byte(s), which is not two halves - its events are not handled");
			continue;
		}
		if block.base as usize + 4 * bytes as usize > MAX_GPES {
			crate::serial_println!("acpi: the {name} block numbers events past {MAX_GPES} - its events are not handled");
			continue;
		}
		layouts[count] = acpi::gpe::Layout { half_bytes: bytes / 2, base: block.base };
		io.status[count] = status;
		count += 1;
	}
	if count == 0 {
		return;
	}
	let mut gpes = acpi::gpe::Gpes::new(&layouts[..count]);
	gpes.initialize(&mut io);
	crate::serial_println!("acpi: {} general-purpose event(s) in {count} block(s), every one disabled until the ACPI service enables it", gpes.len());
	*GPES.lock() = Some((gpes, io));
}

/// The GSI the SCI arrives at, and the trigger and polarity it has there - see `route_sci_with_handler`.
fn sci_gsi(fadt: &Fadt<'_>, rsdp_phys: u64) -> Option<(u32, super::ioapic::Kind)> {
	let source = u8::try_from(fadt.sci_interrupt()?).ok()?;
	let mut gsi = source as u32;
	let mut kind = super::ioapic::Kind::LevelLow;
	if let Some(bytes) = crate::smp::acpi_table(rsdp_phys, b"APIC")
		&& let Ok(madt) = Madt::new(bytes)
		&& let Some(entry) = madt.isa_override(source)
	{
		gsi = entry.gsi;
		kind = match (entry.polarity, entry.trigger) {
			(Polarity::ActiveHigh, Trigger::Edge) => super::ioapic::Kind::IsaEdge,
			_ => super::ioapic::Kind::LevelLow,
		};
	}
	Some((gsi, kind))
}

/// The SCI's line number, for the `_CRS` check that no namespace device takes a line the kernel uses.
pub fn sci_line() -> Option<u32> {
	BLOCKS.lock().map(|blocks| blocks.gsi).filter(|gsi| *gsi != u32::MAX)
}

/// Route the SCI and return the vector it lands on.
///
/// THE TRIGGER AND THE POLARITY COME FROM THE FIRMWARE AND NOT FROM THIS FILE. The SCI's number in
/// the FADT is an ISA source, and the MADT's Interrupt Source Override is where a machine says which
/// Global System Interrupt it actually arrives at and how. q35 leaves the number alone and overrides
/// only the polarity and the trigger, which is exactly the half a reader forgets.
fn route_sci_with_handler(fadt: &Fadt<'_>, rsdp_phys: u64) -> Option<u8> {
	let Some(source) = fadt.sci_interrupt() else {
		crate::serial_println!("acpi: the FADT names no SCI - fixed-hardware events have no delivery path");
		return None;
	};
	let Ok(source) = u8::try_from(source) else {
		// A SOURCE ABOVE 255 IS A GSI AND NOT AN ISA LINE, which is what the field means on a
		// machine with no 8259 - and a machine with no 8259 is one this routing path does not
		// describe. Refusing is the honest answer.
		crate::serial_println!("acpi: the SCI is source {source}, which is not an ISA line this kernel routes");
		return None;
	};
	let mut gsi = source as u32;
	// THE DEFAULT IS THE STANDARD'S AND NOT THE BUS'S. An ISA line conforms to edge-triggered
	// active-high, and the SCI is the one ISA source the standard says is neither: it is level and
	// it is shareable. A machine whose MADT states nothing about it gets what the standard states.
	let mut kind = super::ioapic::Kind::LevelLow;
	if let Some(bytes) = crate::smp::acpi_table(rsdp_phys, b"APIC")
		&& let Ok(madt) = Madt::new(bytes)
		&& let Some(entry) = madt.isa_override(source)
	{
		gsi = entry.gsi;
		kind = match (entry.polarity, entry.trigger) {
			// "CONFORMS TO THE BUS" ON THE SCI IS THE SCI'S OWN RULE. The override exists, and it
			// declines to state one of the two halves; the standard's answer for this line is the
			// one above.
			(Polarity::ActiveHigh, Trigger::Edge) => super::ioapic::Kind::IsaEdge,
			(Polarity::Reserved, _) | (_, Trigger::Reserved) => {
				crate::serial_println!("acpi: the MADT describes SCI source {source} with a reserved polarity or trigger - taking the standard's level, active-low");
				super::ioapic::Kind::LevelLow
			}
			_ => super::ioapic::Kind::LevelLow,
		};
	}
	// THE VECTOR WINDOW IS THE LEGACY SIXTEEN, which is what an I/O APIC redirection entry on this
	// machine delivers into. A GSI outside it has no vector here, and routing it would overwrite
	// the handler of whichever line the arithmetic landed on.
	if gsi >= super::interrupts::IRQ_COUNT as u32 {
		crate::serial_println!("acpi: the SCI arrives at GSI {gsi}, which is outside the {} vectors this kernel routes", super::interrupts::IRQ_COUNT);
		return None;
	}
	let vector = super::interrupts::IRQ_BASE + gsi as u8;
	// THE HANDLER IS INSTALLED BEFORE THE ENTRY IS UNMASKED, which is why this function does both -
	// see the ordering note in `init`. A caller that could route without registering would be a
	// caller that could get this wrong.
	super::interrupts::register(vector as u32, handle);
	super::ioapic::route(gsi, vector, crate::smp::lapic_id(0), kind);
	let shape = match kind {
		super::ioapic::Kind::LevelLow => "level, active-low",
		super::ioapic::Kind::IsaEdge => "edge, active-high",
	};
	crate::serial_println!("acpi: SCI is GSI {gsi} ({shape}) on vector {vector}");
	Some(vector)
}

/// WHETHER A FIXED BUTTON'S STATUS IS ALREADY SET: a wake the sleep entry would miss, which refuses it.
pub fn button_pending() -> bool {
	let Some(blocks) = *BLOCKS.lock() else { return false };
	[Some(blocks.a_status), blocks.b_status].into_iter().flatten().any(|status| unsafe { port::inw(status) } & (PWRBTN | SLPBTN) != 0)
}

/// THE GENERAL-PURPOSE EVENTS FOR A SLEEP: the wake mask alone enabled at the entry, the runtime mask back after.
pub fn gpes_enter_sleep() {
	if let Some((gpes, io)) = GPES.lock().as_mut() {
		gpes.enter_sleep(io);
	}
}

pub fn gpes_leave_sleep() {
	if let Some((gpes, io)) = GPES.lock().as_mut() {
		gpes.leave_sleep(io);
	}
}

/// PM1's enables and the SCI's routing again, after an S3 reset the chipset: the same arming `init` made.
pub fn rearm_after_reset() {
	let Some(blocks) = *BLOCKS.lock() else { return };
	let armed = ARMED.load(core::sync::atomic::Ordering::Relaxed);
	for (status, enable) in [(Some(blocks.a_status), Some(blocks.a_enable)), (blocks.b_status, blocks.b_enable)] {
		if let (Some(status), Some(enable)) = (status, enable) {
			unsafe {
				port::outw(status, armed);
				port::outw(enable, armed);
			}
		}
	}
}

/// The SCI handler: decode what is set, acknowledge it, and report it.
fn handle(_vector: u32) {
	let Some(blocks) = *BLOCKS.lock() else { return };
	let mut seen: u16 = 0;
	for status in [Some(blocks.a_status), blocks.b_status].into_iter().flatten() {
		// A STATUS REGISTER IS ACKNOWLEDGED BY WRITING BACK THE BITS THAT WERE SET, and only those.
		// Writing ones everywhere would acknowledge events this kernel did not decode - a wake
		// status, a timer rollover - and their owners would never see them.
		let raised = unsafe { port::inw(status) } & (PWRBTN | SLPBTN | GBL);
		if raised != 0 {
			unsafe { port::outw(status, raised) };
			seen |= raised;
		}
	}
	// SAID BY THE KERNEL, EVERY TIME, WHETHER OR NOT ANYBODY IS LISTENING.
	//
	// It was said only when NOBODY was listening, which is exactly backwards: with a listener
	// attached the whole path went silent, so a boot log could not distinguish "the interrupt never
	// arrived" from "it arrived and the consumer did nothing with it" - and those are two different
	// defects with two different owners. The press is a fact about the MACHINE and belongs in the
	// machine's own log.
	// WHILE THE MACHINE SLEEPS A BUTTON IS WHAT WOKE IT, reported as the wake and NOT delivered as a press: QEMU's
	// `system_wakeup` itself sets the power button's status, and a delivered press would power the machine off.
	let sleeping = crate::sleep::sleeping();
	if seen & PWRBTN != 0 {
		if sleeping {
			crate::serial_println!("acpi: the power button woke the machine");
			crate::sleep::woke_by(abi::WAKE_POWER_BUTTON, 0);
		} else {
			crate::serial_println!("acpi: the power button was pressed");
			crate::platform_event::report(crate::platform_event::POWER_BUTTON);
		}
	}
	if seen & SLPBTN != 0 {
		if sleeping {
			crate::serial_println!("acpi: the sleep button woke the machine");
			crate::sleep::woke_by(abi::WAKE_SLEEP_BUTTON, 0);
		} else {
			crate::serial_println!("acpi: the sleep button was pressed");
			crate::platform_event::report(crate::platform_event::SLEEP_BUTTON);
		}
	}
	if seen & GBL != 0 {
		GLOBAL_LOCK_RELEASED.store(true, core::sync::atomic::Ordering::Release);
	}
	// AND THE GENERAL-PURPOSE EVENTS: each asserted enabled one masked and latched for the service.
	if let Some((gpes, io)) = GPES.lock().as_mut() {
		let mut pending = PENDING.lock();
		gpes.handle_into(io, super::apic::ticks(), &mut |gpe, storm| {
			// In a sleep only the wake mask is enabled: an event asserted is a wake, and latched for the service too.
			if sleeping {
				crate::sleep::woke_by(abi::WAKE_DEVICE, u32::from(gpe));
			}
			let (word, bit) = (gpe as usize / 64, gpe as usize % 64);
			if storm {
				pending.1[word] |= 1 << bit;
			} else {
				pending.0[word] |= 1 << bit;
			}
			seen |= 1 << 15;
		});
	}
	// NOTHING THIS HANDLER KNOWS ABOUT WAS SET, WHICH IS THE DANGEROUS CASE. The line is shared, so
	// an interrupt that decodes to nothing is ordinary once - another source on the same pin - and
	// is a storm when it repeats, because the source is asserted and nothing here can clear it.
	if seen == 0 {
		storm(blocks);
	} else {
		STORM_COUNT.store(0, core::sync::atomic::Ordering::Relaxed);
	}
}

/// Count an interrupt this handler could not silence, and disarm past the bound.
fn storm(blocks: Blocks) {
	let now = super::apic::ticks();
	let since = STORM_SINCE.load(core::sync::atomic::Ordering::Relaxed);
	if now.saturating_sub(since) > STORM_WINDOW_TICKS {
		STORM_SINCE.store(now, core::sync::atomic::Ordering::Relaxed);
		STORM_COUNT.store(1, core::sync::atomic::Ordering::Relaxed);
		return;
	}
	if STORM_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1 < STORM_LIMIT {
		return;
	}
	// DISARM AND MASK. The enable register stops this kernel's own sources; masking the redirection
	// entry stops whatever else is on the pin, which is what an interrupt decoding to nothing means
	// it is. Both, because either alone leaves the other's source asserting.
	unsafe {
		port::outw(blocks.a_enable, 0);
		if let Some(b) = blocks.b_enable {
			port::outw(b, 0);
		}
	}
	// AND THE REDIRECTION ENTRY MASKED, as the comment above always said: a source still asserted on the pin - one no
	// enable register here controls - would otherwise keep raising the line with nothing left to decode it.
	if blocks.gsi != u32::MAX {
		super::ioapic::mask(blocks.gsi);
	}
	*BLOCKS.lock() = None;
	crate::serial_println!("acpi: the SCI raised {STORM_LIMIT} interrupts this kernel could not account for in one window - it is disarmed and its line masked, and fixed-hardware and general-purpose events stop here");
}

/// THE SERVICE'S REQUESTS on one event - see `abi::SYS_FIRMWARE_GPE`.
pub fn gpe_request(operation: u64, gpe: u64) -> i64 {
	let mut held = GPES.lock();
	let Some((gpes, io)) = held.as_mut() else { return abi::ERR_UNSUPPORTED };
	if operation == abi::GPE_COUNT {
		return gpes.len() as i64;
	}
	let Ok(gpe) = u16::try_from(gpe) else { return abi::ERR_INVALID };
	let result = match operation {
		abi::GPE_ENABLE => gpes.set_runtime(io, gpe, true),
		abi::GPE_DISABLE => gpes.set_runtime(io, gpe, false),
		abi::GPE_WAKE_SET => gpes.set_wake(gpe, true),
		abi::GPE_WAKE_CLEAR => gpes.set_wake(gpe, false),
		abi::GPE_ACKNOWLEDGE => gpes.acknowledge(io, gpe),
		abi::GPE_REARM => gpes.rearm(io, gpe),
		_ => return abi::ERR_INVALID,
	};
	match result {
		Ok(()) => 0,
		Err(acpi::gpe::Refusal::NoSuchEvent) => abi::ERR_INVALID,
		Err(acpi::gpe::Refusal::Stormed) | Err(acpi::gpe::Refusal::NotRuntime) => abi::ERR_ACCESS_DENIED,
	}
}

/// THE INSTANCE THAT ENABLED THEM ENDED: every runtime event disabled and every latch dropped, and what it had not
/// yet been told dropped with it - the next instance enables what its own namespace handles.
pub fn gpe_instance_ended() {
	if let Some((gpes, io)) = GPES.lock().as_mut() {
		gpes.reset_runtime(io);
	}
	let mut pending = PENDING.lock();
	pending.0 = [0; MAX_GPES / 64];
	GLOBAL_LOCK_RELEASED.store(false, core::sync::atomic::Ordering::Release);
}

/// Hand every latched event to `out` - (`abi::FIRMWARE_EVENT_*`, the event's number) - and forget it. Called from
/// the idle pass; a storm is said on the console as it is handed on.
pub fn take_events(out: &mut dyn FnMut(u8, u16)) {
	let (delivered, stormed) = {
		let mut pending = PENDING.lock();
		let taken = *pending;
		*pending = ([0; MAX_GPES / 64], [0; MAX_GPES / 64]);
		taken
	};
	for (words, kind) in [(stormed, abi::FIRMWARE_EVENT_STORM), (delivered, abi::FIRMWARE_EVENT_GPE)] {
		for (word, bits) in words.iter().enumerate() {
			for bit in 0..64 {
				if bits & 1 << bit != 0 {
					let gpe = (word * 64 + bit) as u16;
					if kind == abi::FIRMWARE_EVENT_STORM {
						crate::serial_println!("acpi: general-purpose event {gpe:#04x} asserted past its bound within one window - left disabled for the rest of the boot, every other event unaffected");
					}
					out(kind, gpe);
				}
			}
		}
	}
	if GLOBAL_LOCK_RELEASED.swap(false, core::sync::atomic::Ordering::AcqRel) {
		out(abi::FIRMWARE_EVENT_GLOBAL_LOCK, 0);
	}
}
