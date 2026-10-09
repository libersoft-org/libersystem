// Goldfish RTC: nanoseconds since the Unix epoch and a retained alarm where the device tree explicitly
// declares `wakeup-source`. Register semantics follow QEMU hw/rtc/goldfish_rtc.c: TIME_LOW latches HIGH,
// ALARM_HIGH precedes LOW (which arms), ALARM_STATUS means running, and cancelling does not clear IRQ.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::object::interrupt::Interrupt;
use crate::sync::SpinLock;

const TIME_LOW: u64 = 0x00;
const TIME_HIGH: u64 = 0x04;
const ALARM_LOW: u64 = 0x08;
const ALARM_HIGH: u64 = 0x0c;
const IRQ_ENABLED: u64 = 0x10;
const CLEAR_ALARM: u64 = 0x14;
const ALARM_STATUS: u64 = 0x18;
const CLEAR_INTERRUPT: u64 = 0x1c;

// Preserve the existing QEMU no-DT clock reader. A system wake never uses this fallback.
const DEFAULT_BASE: u64 = 0x0010_1000;
static CLOCK_BASE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct Description {
	base: u64,
	domain: u64,
	source: u32,
}

struct Alarm {
	description: Description,
	interrupt: Arc<Interrupt>,
	previous_enables: u64,
	previous_target: u32,
	previous_alarm: u64,
	deadline: u64,
	hart: u64,
}

static ALARM: SpinLock<Option<Alarm>> = SpinLock::new(None);

fn read(base: u64, offset: u64) -> u32 {
	// SAFETY: a validated DT range, or the established no-DT QEMU clock descriptor.
	unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(base + offset) as *const u32) }
}

fn write(base: u64, offset: u64, value: u32) {
	// SAFETY: only the alarm path writes, after validating the DT range and reserving its source.
	unsafe { core::ptr::write_volatile(super::paging::phys_to_virt(base + offset) as *mut u32, value) };
}

fn ordered() {
	// Order controller and RTC I/O before checking pending state or releasing the interrupt identity.
	unsafe { core::arch::asm!("fence iorw, iorw", options(nostack, preserves_flags)) };
}

fn now(base: u64) -> u64 {
	let low = u64::from(read(base, TIME_LOW));
	(u64::from(read(base, TIME_HIGH)) << 32) | low
}

fn clock_base() -> u64 {
	let cached = CLOCK_BASE.load(Ordering::Acquire);
	if cached != 0 {
		return cached;
	}
	let mut base = DEFAULT_BASE;
	if let Some(tree) = super::device_tree() {
		tree.devices(|node| {
			if node.is_compatible(b"google,goldfish-rtc")
				&& !node.reg_refused
				&& node.bus == fdt::NodeBus::Memory
				&& let [(at, size)] = node.regs()
				&& *at != 0 && *at & 3 == 0
				&& *size >= 0x20
				&& crate::mem::within_direct_map(*at, *size)
			{
				base = *at;
			}
		});
	}
	CLOCK_BASE.store(base, Ordering::Release);
	base
}

pub fn read_unix_ns() -> u64 {
	// TIME_LOW/HIGH are a shared latch, including between the clock reader and alarm preparation.
	let _alarm = ALARM.lock();
	now(clock_base())
}

pub fn read_unix() -> u64 {
	read_unix_ns() / 1_000_000_000
}

fn description() -> Option<Description> {
	if !super::rtc_present() || !super::imsic::usable() {
		return None;
	}
	let tree = super::device_tree()?;
	let mut rtc = None;
	let mut count = 0;
	if !tree.devices(|node| {
		if node.is_compatible(b"google,goldfish-rtc") {
			count += 1;
			rtc = Some(*node);
		}
	}) || count != 1
	{
		return None;
	}
	let rtc = rtc?;
	if !rtc.wakeup_source || rtc.reg_refused || rtc.bus != fdt::NodeBus::Memory {
		return None;
	}
	let [(base, size)] = rtc.regs() else { return None };
	if *base == 0 || *base & 3 != 0 || *size < 0x20 || !crate::mem::within_direct_map(*base, *size) {
		return None;
	}
	let [fdt::NodeInterrupt::Controller(route)] = rtc.interrupts() else { return None };
	// Goldfish drives an active-high level line. A different binding is not guessed into this one.
	if route.cells != 2 || route.spec[0] == 0 || route.spec[0] > 1023 || route.spec[1] != 4 {
		return None;
	}
	let mut domain = None;
	if !tree.devices(|node| {
		if node.phandle == route.controller
			&& node.interrupt_controller
			&& node.is_compatible(b"riscv,aplic")
			&& !node.reg_refused
			&& let [(at, size)] = node.regs()
			&& *at != 0
			&& *at & 0xfff == 0
			&& *size >= 0x4000
			&& crate::mem::within_direct_map(*at, *size)
		{
			domain = Some(*at);
		}
	}) {
		return None;
	}
	let domain = domain?;
	if super::aplic::domain().is_some_and(|adopted| adopted != domain) {
		return None;
	}
	Some(Description { base: *base, domain, source: route.spec[0] })
}

pub fn wake_supported() -> bool {
	description().is_some()
}

// Called by the returning boot-core coordinator before it saves its context. Other device identities,
// including the kernel UART/hotplug identity, remain pending but cannot cause its firmware WFI to return.
pub fn arm_alarm(after_ns: u64) -> Result<(), i64> {
	let description = description().ok_or(abi::ERR_UNSUPPORTED)?;
	let mut slot = ALARM.lock();
	if slot.is_some() || read(description.base, IRQ_ENABLED) != 0 || read(description.base, ALARM_STATUS) != 0 {
		return Err(abi::ERR_ALREADY_CLAIMED);
	}
	let hart = super::percpu::this_cpu().lapic_id();
	if !super::imsic::has_file(hart) || !super::aplic::adopt_domain(description.domain) {
		return Err(abi::ERR_UNSUPPORTED);
	}
	let deadline = now(description.base).checked_add(after_ns).filter(|at| *at <= i64::MAX as u64).ok_or(abi::ERR_INVALID)?;
	let previous_enables = super::imsic::save_hart().2;
	let previous_alarm = (u64::from(read(description.base, ALARM_HIGH)) << 32) | u64::from(read(description.base, ALARM_LOW));
	let line = abi::WiredLine { number: description.source, trigger: abi::LINE_TRIGGER_LEVEL, polarity: abi::LINE_POLARITY_HIGH, controller: abi::LINE_CONTROLLER_APLIC, _pad: 0 };
	let (interrupt, previous_target) = super::interrupts::bind_wired_inactive(&line).map_err(|_| abi::ERR_ALREADY_CLAIMED)?;
	let eid = interrupt.vector();
	if !crate::sleep::mark_wake(eid, true) {
		super::interrupts::release_wired_inactive(interrupt, previous_target);
		return Err(abi::ERR_ALREADY_CLAIMED);
	}
	write(description.base, CLEAR_INTERRUPT, 1);
	super::imsic::clear_pending(eid);
	super::imsic::replace_enables(1u64 << eid);
	write(description.base, IRQ_ENABLED, 1);
	write(description.base, ALARM_HIGH, (deadline >> 32) as u32);
	write(description.base, ALARM_LOW, deadline as u32);
	ordered();
	*slot = Some(Alarm { description, interrupt, previous_enables, previous_target, previous_alarm, deadline, hart });
	Ok(())
}

fn pending(alarm: &Alarm) -> bool {
	alarm.interrupt.is_pending()
		|| (alarm.hart == super::percpu::this_cpu().lapic_id() && super::imsic::is_pending(alarm.interrupt.vector()))
		// ALARM_STATUS is running, not pending. Only our armed alarm, whose programmed deadline has
		// passed and which nobody has cancelled, makes its transition to zero evidence of firing.
		|| (read(alarm.description.base, ALARM_STATUS) == 0 && now(alarm.description.base) >= alarm.deadline)
}

pub fn alarm_pending() -> bool {
	ALARM.lock().as_ref().is_some_and(pending)
}

// Same coordinator hart, after the saved controller/core settings have been restored. Also unwinds a
// refused entry. The prior alarm was inactive; restore its compare value without leaving it armed.
pub fn disarm_alarm() -> bool {
	let mut slot = ALARM.lock();
	let Some(alarm) = slot.take() else { return false };
	assert_eq!(alarm.hart, super::percpu::this_cpu().lapic_id());
	let fired = pending(&alarm);
	let description = alarm.description;
	write(description.base, IRQ_ENABLED, 0);
	write(description.base, CLEAR_ALARM, 1);
	write(description.base, CLEAR_INTERRUPT, 1);
	unsafe { super::aplic::mask_source(description.domain, description.source) };
	ordered();
	super::imsic::clear_pending(alarm.interrupt.vector());
	super::interrupts::release_wired_inactive(alarm.interrupt, alarm.previous_target);
	write(description.base, ALARM_HIGH, (alarm.previous_alarm >> 32) as u32);
	write(description.base, ALARM_LOW, alarm.previous_alarm as u32);
	write(description.base, CLEAR_ALARM, 1);
	write(description.base, CLEAR_INTERRUPT, 1);
	ordered();
	super::imsic::replace_enables(alarm.previous_enables);
	fired
}
