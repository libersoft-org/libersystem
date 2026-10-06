// THE PLATFORM-ROW SUITE: a device the firmware describes - published through the checks and the placement a
// boot's description goes through, claimed the way DeviceManager claims one, its resources minted from the
// claim by kind and index - and every resource taken back by a release and by a kill.
//
// THE LINES ARE RAISED ON DEMAND, by real devices: an EDGE from the test machine's second `isa-serial` on
// x86_64, and a LEVEL from the `edu` function's INTx on all three ports - which stays asserted until its
// acknowledge register is written, so a line that is not masked while its driver runs is a storm the count of
// deliveries below sees, and a line a release did not silence is a delivery it sees.

use super::*;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::object::ObjectType;
use crate::object::handle::Handle;
use crate::object::interrupt::Interrupt;
use crate::object::rights::Rights;
use crate::sync::SpinLock;
use crate::tests::{publish_synthetic_platform, synthetic_platform_description};

fn acquire(claim: u64, kind: u64, which: u64) -> i64 {
	unsafe { arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, claim, kind, which, 0) as i64 }
}

fn acknowledge(handle: i64) -> i64 {
	unsafe { arch::syscall::invoke(abi::SYS_INTERRUPT_ACK, handle as u64, 0, 0, 0) as i64 }
}

fn map(handle: u64) -> u64 {
	unsafe { arch::syscall::invoke(abi::SYS_DEVICE_MEMORY_MAP, handle, 0, 0, 0) }
}

// The Interrupt a line handle in the calling thread's table names.
fn interrupt_of(handle: i64) -> Arc<Interrupt> {
	let thread = sched::current_thread().expect("a current thread");
	let object = thread.handles().lock().lookup_typed(Handle::from_raw(handle as u64), ObjectType::Interrupt, Rights::WRITE).expect("an interrupt handle");
	object.into_any_arc().downcast::<Interrupt>().ok().expect("an Interrupt")
}

// Whether a handle of the calling thread no longer reaches its object - what a revocation does to every handle.
fn revoked(handle: u64, kind: ObjectType) -> bool {
	let thread = sched::current_thread().expect("a current thread");
	thread.handles().lock().lookup_typed(Handle::from_raw(handle), kind, Rights::NONE).is_err()
}

// Spin with interrupts open until `done`, or until `ticks` scheduler ticks have passed; whether `done` came true.
fn within(ticks: u64, done: impl Fn() -> bool) -> bool {
	let until = arch::apic::ticks() + ticks;
	while !done() {
		if arch::apic::ticks() >= until {
			return done();
		}
		core::hint::spin_loop();
	}
	true
}

// Let `ticks` pass: the window a storm, or a delivery that should not happen, would fill.
fn settle(ticks: u64) {
	let _ = within(ticks, || false);
}

// RUN `body` ON A THREAD OF ITS OWN, to its end: the syscall paths need a caller with a handle table.
fn in_thread(body: extern "C" fn(u64), arg: u64, done: &'static AtomicBool) {
	done.store(false, Ordering::SeqCst);
	sched::spawn(body, arg);
	sched::run_until_idle();
	assert!(done.load(Ordering::SeqCst), "the test's thread ran to its end");
}

// MMIO THAT NOTHING ON THE TEST MACHINE DECODES AND NO MEMORY MAP CALLS MEMORY: a page run in the chipset's
// window above the I/O APIC on x86_64, and in the empty platform-bus window of QEMU's `virt` on the other two.
// Mapped, never read - an access nothing decodes is a bus error on the two emulated ports.
fn free_window(offset: u64) -> u64 {
	#[cfg(target_arch = "x86_64")]
	let base = 0xfed6_0000;
	#[cfg(target_arch = "aarch64")]
	let base = 0x0d00_0000;
	#[cfg(target_arch = "riscv64")]
	let base = 0x0500_0000;
	base + offset
}

// THE LINE THE CLAIM TEST ACQUIRES: COM2's on x86_64, where it is raised; on the ports one the test machine
// leaves unconnected - SPI 12, and APLIC source 20 in the domain the console's own line names - since that test
// is about the claim and the release, and the level test below raises a real one.
fn claim_test_line() -> abi::WiredLine {
	#[cfg(target_arch = "x86_64")]
	{
		arch::platform::isa_irq_line(3)
	}
	#[cfg(target_arch = "aarch64")]
	{
		abi::WiredLine { number: 32 + 12, trigger: abi::LINE_TRIGGER_EDGE, polarity: abi::LINE_POLARITY_HIGH, controller: abi::LINE_CONTROLLER_GIC, _pad: 0 }
	}
	#[cfg(target_arch = "riscv64")]
	{
		let tree = arch::device_tree().expect("the test machine hands over a device tree");
		let route = tree.console_interrupt().expect("the tree names the console's line");
		// Read through the port's own conversion, which adopts the domain a claimed line is armed through.
		let console = arch::platform::wired_line(&tree, &route).expect("the console's line is the APLIC's");
		abi::WiredLine { number: 20, trigger: abi::LINE_TRIGGER_EDGE, polarity: abi::LINE_POLARITY_HIGH, ..console }
	}
}

// COM2 on x86_64: its transmitter-empty interrupt, raised by enabling it with the holding register empty and
// lowered by reading the identification register.
#[cfg(target_arch = "x86_64")]
mod com2 {
	const BASE: u16 = 0x2f8;
	const IER: u16 = BASE + 1;
	const IIR: u16 = BASE + 2;
	const MCR: u16 = BASE + 4;

	unsafe fn outb(port: u16, value: u8) {
		unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
	}

	unsafe fn inb(port: u16) -> u8 {
		let value: u8;
		unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
		value
	}

	pub fn raise() {
		// SAFETY: the test machine's second UART, which nothing else drives while this test runs.
		unsafe {
			outb(IER, 0);
			let _ = inb(IIR);
			outb(MCR, 0x08);
			outb(IER, 0x02);
		}
	}

	pub fn lower() {
		// SAFETY: as above.
		unsafe {
			outb(IER, 0);
			let _ = inb(IIR);
			outb(MCR, 0);
		}
	}
}

crate::tagged_test!(a_platform_row_is_claimed_with_its_resources_and_a_release_takes_every_one_back, [Drivers, Interrupt, Kernel], id = "kernel.platform_rows.a_platform_row_is_claimed_with_its_resources_and_a_release_takes_every_one_back", covers = ["kernel"]);
fn a_platform_row_is_claimed_with_its_resources_and_a_release_takes_every_one_back() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(row: u64) {
		claimed_and_released(row as usize);
		DONE.store(true, Ordering::SeqCst);
	}
	let line = claim_test_line();
	let mut description = synthetic_platform_description(b"kernel:test-platform-claim");
	assert!(description.add_mmio(free_window(0), 0x1000) && description.add_mmio(free_window(0x1000), 0x1000), "two ranges fit a row");
	#[cfg(target_arch = "x86_64")]
	assert!(description.add_port(0x2f8, 8), "and a port range");
	assert!(description.add_line(line), "and a wired line");
	let row = publish_synthetic_platform(description).expect("a claimable description over nothing anyone owns is published");
	// THE INFORMATION RECORD SAYS WHAT THE ROW IS, beside the PCI rows.
	let info = device::info(row).expect("the row answers SYS_DEVICE_INFO's question");
	assert_eq!(info.transport, abi::TRANSPORT_PLATFORM);
	assert_eq!(info.platform.kind, abi::ROW_KIND_PLATFORM);
	assert_eq!(info.platform.identity(), b"kernel:test-platform-claim");
	assert_eq!(info.platform.state, abi::PLATFORM_STATE_CLAIMABLE);
	assert_eq!(info.platform.mmio().len(), 2);
	assert_eq!(info.platform.lines(), &[line]);
	in_thread(body, row as u64, &DONE);
}

fn claimed_and_released(row: usize) {
	let line = claim_test_line();
	let grant = crate::tests::claim_device(row as u64).expect("the row is claimed under the entry that declares it");
	// RANGE 0 IS THE CLAIM'S OWN MEMORY; the others are minted by index.
	assert!(!syscall::sys_is_err(map(grant.memory)), "the claim's memory - the row's first range - maps");
	let second = acquire(grant.claim, abi::RESOURCE_KIND_MMIO, 1);
	assert!(second > 0, "the row's second range is minted from the claim ({second})");
	assert!(!syscall::sys_is_err(map(second as u64)), "and maps");
	assert_eq!(acquire(grant.claim, abi::RESOURCE_KIND_MMIO, 0), syscall::ERR_INVALID, "range 0 was minted with the claim");
	assert_eq!(acquire(grant.claim, abi::RESOURCE_KIND_MMIO, 2), syscall::ERR_INVALID, "an index past the row's ranges");
	#[cfg(target_arch = "x86_64")]
	let ports = {
		let ports = acquire(grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0);
		assert!(ports > 0, "the row's port range is minted from the claim ({ports})");
		ports
	};
	let handle = acquire(grant.claim, abi::RESOURCE_KIND_LINE, 0);
	assert!(handle > 0, "the row's line is bound from the claim ({handle})");
	let interrupt = interrupt_of(handle);
	let vector = interrupt.vector();
	assert!(arch::interrupts::is_bound(vector), "the line is bound");
	assert!(arch::interrupts::line_armed(&line), "and live at its controller");
	assert_eq!(acquire(grant.claim, abi::RESOURCE_KIND_LINE, 0), syscall::ERR_RESOURCE_EXHAUSTED, "one binding per line");
	assert_eq!(acquire(grant.claim, abi::RESOURCE_KIND_LINE, 1), syscall::ERR_INVALID, "an index past the row's lines");
	// AN EDGE, RAISED BY THE DEVICE ON x86_64.
	#[cfg(target_arch = "x86_64")]
	{
		com2::raise();
		let delivered = within(100, || interrupt.is_pending());
		com2::lower();
		assert!(delivered, "COM2's transmitter-empty interrupt reached the claim's line");
		assert_eq!(acknowledge(handle), 0);
		assert!(!interrupt.is_pending(), "the acknowledgement cleared it");
		assert!(arch::interrupts::line_armed(&line), "and an edge line is never masked");
	}
	// THE RELEASE TAKES EVERY RESOURCE BACK - the handles, and the line at its controller.
	crate::tests::release_device(&grant);
	assert!(interrupt.is_revoked(), "the release revoked the line's interrupt");
	assert!(!arch::interrupts::is_bound(vector), "and unbound its vector");
	assert!(!arch::interrupts::line_armed(&line), "and masked the line at its controller");
	assert!(revoked(handle as u64, ObjectType::Interrupt), "the line's handle reaches nothing");
	assert!(revoked(grant.memory, ObjectType::DeviceMemory), "nor does the claim's memory");
	assert!(revoked(second as u64, ObjectType::DeviceMemory), "nor the second range");
	#[cfg(target_arch = "x86_64")]
	assert!(revoked(ports as u64, ObjectType::PortRange), "nor the port range");
	// AND THE ROW IS CLAIMABLE AGAIN, its line with it.
	let again = crate::tests::claim_device(row as u64).expect("the released row is claimed again");
	let rebound = acquire(again.claim, abi::RESOURCE_KIND_LINE, 0);
	assert!(rebound > 0, "and its line is bound again ({rebound})");
	crate::tests::release_device(&again);
	assert!(!arch::interrupts::line_armed(&line), "and silenced again by that release");
}

// THE `edu` FUNCTION'S INTx AS A PLATFORM ROW'S LINE: on x86_64 by the ISA IRQ the firmware routed its pin to,
// on the ports through the tree's `interrupt-map`, read by the port's own conversion.
fn edu_line(edu: &crate::iommu::edu::Edu) -> abi::WiredLine {
	let (pin, line) = arch::pci::intx_pin_and_line(edu.bus, edu.dev, edu.func);
	assert!((1..=4).contains(&pin), "the edu function has an INTx pin ({pin})");
	#[cfg(target_arch = "x86_64")]
	{
		assert!(line < 16, "the firmware routed the edu function's pin to an ISA IRQ ({line:#x})");
		arch::platform::pci_intx_line(line)
	}
	#[cfg(not(target_arch = "x86_64"))]
	{
		let _ = line;
		let tree = arch::device_tree().expect("the test machine hands over a device tree");
		let route = tree.pci_intx_route(edu.bus, edu.dev, edu.func, pin).expect("the tree routes the edu function's pin");
		arch::platform::wired_line(&tree, &route).expect("the pin reaches the interrupt controller this kernel drives")
	}
}

// The `edu` function with its INTx enabled and nothing asserted, and its line published as a row's - or None
// where the machine has no `edu`, which the caller says.
fn edu_row(identity: &[u8]) -> Option<(usize, abi::WiredLine)> {
	let edu = crate::iommu::edu::find()?;
	assert!(edu.alive(), "the fixture answers on its own register window");
	edu.acknowledge_interrupt(u32::MAX);
	arch::pci::set_intx_disabled(edu.bus, edu.dev, edu.func, false);
	let line = edu_line(&edu);
	assert_eq!(line.trigger, abi::LINE_TRIGGER_LEVEL, "an INTx line is level-triggered");
	let mut description = synthetic_platform_description(identity);
	assert!(description.add_line(line));
	Some((publish_synthetic_platform(description).expect("the edu line's row is published"), line))
}

crate::tagged_test!(a_level_line_is_masked_until_its_driver_acknowledges_and_silenced_by_the_release, [Drivers, Interrupt, Kernel], id = "kernel.platform_rows.a_level_line_is_masked_until_its_driver_acknowledges_and_silenced_by_the_release", covers = ["kernel"]);
fn a_level_line_is_masked_until_its_driver_acknowledges_and_silenced_by_the_release() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(row: u64) {
		masked_until_acknowledged(row as usize);
		DONE.store(true, Ordering::SeqCst);
	}
	let Some((row, _line)) = edu_row(b"kernel:test-edu-level") else {
		crate::serial_println!("platform-line: absent (no edu function on this machine) - the level case did not run");
		return;
	};
	in_thread(body, row as u64, &DONE);
}

fn masked_until_acknowledged(row: usize) {
	let edu = crate::iommu::edu::find().expect("the edu function");
	let line = device::platform_line(row, 0).expect("the row's line");
	let grant = crate::tests::claim_device(row as u64).expect("the edu line's row is claimed");
	let handle = acquire(grant.claim, abi::RESOURCE_KIND_LINE, 0);
	assert!(handle > 0, "the edu function's line is bound from the claim ({handle})");
	let interrupt = interrupt_of(handle);
	let vector = interrupt.vector();
	assert!(arch::interrupts::line_armed(&line), "the claimed line is live at its controller");
	// RAISED AND HELD: one delivery, and the line masked - not a storm.
	edu.raise_interrupt(1);
	assert!(within(100, || interrupt.signals() >= 1), "the edu function's INTx reached its claim");
	settle(5);
	assert_eq!(interrupt.signals(), 1, "a level line still asserted is masked until its driver acknowledges - one delivery, not a storm");
	assert!(!arch::interrupts::line_armed(&line), "and the mask is at its controller");
	// ACKNOWLEDGED WHILE STILL ASSERTED: delivered again, because a level is not an edge.
	assert_eq!(acknowledge(handle), 0);
	assert!(within(100, || interrupt.signals() >= 2), "an acknowledged line that is still asserted is delivered again");
	settle(5);
	assert_eq!(interrupt.signals(), 2, "once, and masked again");
	// DEASSERTED, THEN ACKNOWLEDGED: quiet, and live.
	edu.acknowledge_interrupt(1);
	assert_eq!(edu.interrupt_status(), 0, "the device dropped its line");
	assert_eq!(acknowledge(handle), 0);
	settle(5);
	assert_eq!(interrupt.signals(), 2, "a line that dropped before the acknowledgement is not delivered again");
	assert!(arch::interrupts::line_armed(&line), "and it is live again");
	// ONE MORE ROUND, so the line the acknowledgement unmasked is shown to deliver.
	edu.raise_interrupt(2);
	assert!(within(100, || interrupt.signals() >= 3), "the unmasked line delivers the next assertion");
	edu.acknowledge_interrupt(2);
	assert_eq!(acknowledge(handle), 0);
	// RELEASED: the line is silenced though its source asserts.
	crate::tests::release_device(&grant);
	assert!(!arch::interrupts::is_bound(vector), "the release unbound the line");
	assert!(!arch::interrupts::line_armed(&line), "and masked it at its controller");
	edu.raise_interrupt(4);
	settle(5);
	assert_eq!(interrupt.signals(), 3, "a released line delivers nothing though its source is asserted");
	edu.acknowledge_interrupt(4);
	// AN ACKNOWLEDGEMENT THROUGH THE OLD OBJECT reaches no controller: the vector is nobody's now.
	assert!(!interrupt.owns_binding(), "the revoked interrupt owns no binding");
}

// THE HOLDER KILLED: a driver process that claimed the row, bound its line and had it raised - the line
// asserted and masked - is killed with its Domain, and the kill takes the claim, the line and its vector back.
static HOLDER_DOMAIN: SpinLock<Option<Arc<crate::object::domain::Domain>>> = SpinLock::new(None);
static HOLDER_READY: AtomicBool = AtomicBool::new(false);
static HOLDER_VECTOR: AtomicU32 = AtomicU32::new(0);
static HOLDER_SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn holder(row: u64) {
	let edu = crate::iommu::edu::find().expect("the edu function");
	let grant = crate::tests::claim_device(row).expect("the holder claims the row");
	let handle = acquire(grant.claim, abi::RESOURCE_KIND_LINE, 0);
	assert!(handle > 0, "the holder binds the line ({handle})");
	let interrupt = interrupt_of(handle);
	HOLDER_VECTOR.store(interrupt.vector(), Ordering::SeqCst);
	edu.raise_interrupt(8);
	HOLDER_SIGNALLED.store(within(100, || interrupt.signals() >= 1), Ordering::SeqCst);
	drop(interrupt);
	HOLDER_READY.store(true, Ordering::SeqCst);
	loop {
		sched::yield_now();
	}
}

extern "C" fn killer(_: u64) {
	while !HOLDER_READY.load(Ordering::SeqCst) {
		sched::yield_now();
	}
	if let Some(domain) = HOLDER_DOMAIN.lock().take() {
		domain.kill();
	}
}

crate::tagged_test!(a_killed_holder_loses_its_claimed_line, [Drivers, Interrupt, Kernel, Process], id = "kernel.platform_rows.a_killed_holder_loses_its_claimed_line", covers = ["kernel"]);
fn a_killed_holder_loses_its_claimed_line() {
	let Some((row, line)) = edu_row(b"kernel:test-edu-kill") else {
		crate::serial_println!("platform-line: absent (no edu function on this machine) - the kill case did not run");
		return;
	};
	HOLDER_READY.store(false, Ordering::SeqCst);
	HOLDER_SIGNALLED.store(false, Ordering::SeqCst);
	let domain = crate::object::domain::Domain::new(1 << 20, 8, 4);
	*HOLDER_DOMAIN.lock() = Some(domain.clone());
	sched::spawn_in(domain.clone(), holder, row as u64).expect("spawn the holder");
	sched::spawn(killer, 0);
	sched::run_until_idle();
	let edu = crate::iommu::edu::find().expect("the edu function");
	edu.acknowledge_interrupt(u32::MAX);
	assert!(HOLDER_READY.load(Ordering::SeqCst), "the holder bound its line before it was killed");
	assert!(HOLDER_SIGNALLED.load(Ordering::SeqCst), "and its line delivered to it");
	let vector = HOLDER_VECTOR.load(Ordering::SeqCst);
	assert!(!arch::interrupts::is_bound(vector), "the kill unbound the holder's line");
	assert!(!arch::interrupts::line_armed(&line), "and it is masked at its controller");
	assert_eq!(device::claim_state(row), Some(device::ClaimState::Free), "and the claim was released");
	assert_eq!(domain.account().handles().used(), 0, "and every handle the holder had is gone");
}

crate::tagged_test!(a_kernel_held_device_an_overlap_and_memory_or_a_bar_are_refused, [Drivers, Kernel], id = "kernel.platform_rows.a_kernel_held_device_an_overlap_and_memory_or_a_bar_are_refused", covers = ["kernel"]);
fn a_kernel_held_device_an_overlap_and_memory_or_a_bar_are_refused() {
	let entry = dma_policy::entry_field(b"synthetic-platform");
	// A KERNEL-HELD ROW IS PUBLISHED, SO THE MACHINE IS ACCOUNTED FOR, AND NEVER CLAIMED.
	let mut held = platform::Description::new(abi::PLATFORM_SOURCE_KERNEL, abi::PLATFORM_STATE_KERNEL_HELD, b"kernel:test-held").expect("fits");
	assert!(held.add_match(abi::MATCH_ID_HID, dma_policy::SYNTHETIC_PLATFORM_HID) && held.add_mmio(free_window(0x8000), 0x1000));
	let held_row = publish_synthetic_platform(held).expect("a kernel-held description is published");
	assert_eq!(device::info(held_row).map(|info| info.platform.state), Some(abi::PLATFORM_STATE_KERNEL_HELD));
	assert_eq!(device::claim(held_row, &entry), Err(device::ClaimError::Refused), "a kernel-held row is never claimed");
	// AND THE KERNEL'S OWN CONSOLE ROW, where the machine describes it statically.
	#[cfg(target_arch = "x86_64")]
	{
		let com1 = (0..device::count()).find(|&index| device::info(index).is_some_and(|info| info.platform.identity() == b"kernel:com1")).expect("the kernel publishes its console as a row");
		assert_eq!(device::info(com1).map(|info| info.platform.state), Some(abi::PLATFORM_STATE_KERNEL_HELD));
		assert_eq!(device::claim(com1, &dma_policy::entry_field(b"synthetic-none")), Err(device::ClaimError::Refused), "the console is the kernel's");
	}
	// AND ON THE DEVICE-TREE PORTS, whose console row is the tree's `/chosen/stdout-path` node and whose test kernel reads
	// no tree: no row carries the console flag, so no claim can take the UART the suite is judged by - the same rule COM1's
	// kernel-held row keeps on x86_64. The handoff's own rules run in `arch::common::console_uart`'s tests instead.
	#[cfg(not(target_arch = "x86_64"))]
	assert!(!(0..device::count()).any(|index| device::info(index).is_some_and(|info| info.platform.flags & abi::PLATFORM_FLAG_CONSOLE != 0)), "a test kernel publishes no console row its suite could lose the wire to");
	// AN OVERLAP THAT STARTS AT THE SAME BASE IS ONE DEVICE, MERGED; ANY OTHER IS REFUSED.
	let mut first = synthetic_platform_description(b"kernel:test-overlap-first");
	assert!(first.add_mmio(free_window(0x10000), 0x2000));
	let first_row = publish_synthetic_platform(first).expect("the first description is a row");
	let mut inside = synthetic_platform_description(b"kernel:test-overlap-inside");
	assert!(inside.add_mmio(free_window(0x11000), 0x1000));
	assert_eq!(publish_synthetic_platform(inside), None, "an overlap that does not start where the row does is refused");
	let mut same = synthetic_platform_description(b"kernel:test-overlap-same");
	assert!(same.add_mmio(free_window(0x10000), 0x1000));
	assert_eq!(publish_synthetic_platform(same), Some(first_row), "one starting at the row's base is that device");
	let merged = device::info(first_row).expect("the merged row");
	assert!(merged.platform.match_ids().iter().any(|id| id.kind == abi::MATCH_ID_IDENTITY && id.text() == b"kernel:test-overlap-same"), "and the second identity joined the row");
	assert_eq!((merged.platform.mmio()[0].base, merged.platform.mmio()[0].len), (free_window(0x10000), 0x2000), "which keeps the first description's range");
	// ONE ROW PER IDENTITY: the same identity again is the same row.
	let mut again = synthetic_platform_description(b"kernel:test-overlap-first");
	assert!(again.add_mmio(free_window(0x20000), 0x1000));
	assert_eq!(publish_synthetic_platform(again), Some(first_row), "a description under a published identity is that row");
	// MEMORY IS NOBODY'S DEVICE.
	let frame = mem::frame::allocate().expect("a frame");
	let mut ram = synthetic_platform_description(b"kernel:test-over-ram");
	assert!(ram.add_mmio(frame, 0x1000));
	assert_eq!(publish_synthetic_platform(ram), None, "a range over RAM is refused");
	// SAFETY: allocated above and never mapped or handed out.
	unsafe { mem::frame::deallocate(frame) };
	// NOR IS A PCI FUNCTION'S BAR.
	let bar = (0..device::count()).find_map(|index| device::with(index, |entry| (entry.platform.is_none() && entry.bar_len != 0).then_some(entry.bar_phys)).flatten()).expect("the test machine has a PCI function with a BAR");
	let mut over_bar = synthetic_platform_description(b"kernel:test-over-bar");
	assert!(over_bar.add_mmio(bar, 0x1000));
	assert_eq!(publish_synthetic_platform(over_bar), None, "a range over a PCI function's BAR is refused");
	// AND ON x86_64 A CLAIMABLE ROW OVER THE KERNEL'S PORTS.
	#[cfg(target_arch = "x86_64")]
	{
		let mut ports = synthetic_platform_description(b"kernel:test-over-com1");
		assert!(ports.add_port(0x3f8, 8));
		assert_eq!(publish_synthetic_platform(ports), None, "a claimable row over the console's ports is refused");
	}
}

// THE PROPERTY BLOCK REACHES THE DRIVER THROUGH THE WINDOW ITS CLAIM MINTED: DeviceManager holds the claim and
// cannot pass it on, so the driver reads the block with the `DeviceMemory` it was given - while that binding is
// the device's current one, and not after.
crate::tagged_test!(a_driver_reads_its_property_block_through_the_window_its_claim_minted, [Drivers, Kernel], id = "kernel.platform_rows.a_driver_reads_its_property_block_through_the_window_its_claim_minted", covers = ["kernel"]);
fn a_driver_reads_its_property_block_through_the_window_its_claim_minted() {
	static DONE: AtomicBool = AtomicBool::new(false);
	const BLOCK: &[u8] = b"\x00\x00\x05\x00\x04\x00\x00\x00clockLSKT";
	extern "C" fn body(row: u64) {
		read_through_the_claim_and_its_window(row as usize, BLOCK);
		DONE.store(true, Ordering::SeqCst);
	}
	let mut description = synthetic_platform_description(b"kernel:test-properties");
	assert!(description.add_mmio(free_window(0x30000), 0x1000));
	let row = device::add_synthetic_platform_row(device::Described { description, properties: BLOCK.to_vec(), targets: Vec::new(), registers: Vec::new() }).expect("the row is published");
	assert_eq!(device::info(row).map(|info| info.platform.properties_len), Some(BLOCK.len() as u32), "the information record states the block's length");
	in_thread(body, row as u64, &DONE);
}

fn properties(handle: u64, buf: &mut [u8]) -> i64 {
	unsafe { arch::syscall::invoke(abi::SYS_DEVICE_PROPERTIES, handle, buf.as_mut_ptr() as u64, buf.len() as u64, 0) as i64 }
}

fn read_through_the_claim_and_its_window(row: usize, block: &[u8]) {
	let grant = crate::tests::claim_device(row as u64).expect("the row is claimed");
	let mut buf = [0u8; 64];
	assert_eq!(properties(grant.claim, &mut buf), block.len() as i64, "the claim reads the block");
	assert_eq!(&buf[..block.len()], block);
	buf.fill(0);
	assert_eq!(properties(grant.memory, &mut buf), block.len() as i64, "and so does the window the claim minted - what the driver holds");
	assert_eq!(&buf[..block.len()], block);
	let mut short = [0u8; 4];
	assert_eq!(properties(grant.memory, &mut short), block.len() as i64, "a short buffer is told the whole length");
	assert_eq!(&short, &block[..4], "and given what fits");
	crate::tests::release_device(&grant);
	assert!(properties(grant.memory, &mut buf) < 0, "a window whose binding ended reads nothing");
	// A ROW WITH NO BLOCK SAYS SO.
	let mut bare = synthetic_platform_description(b"kernel:test-no-properties");
	assert!(bare.add_mmio(free_window(0x31000), 0x1000));
	let bare_row = publish_synthetic_platform(bare).expect("published");
	let bare_grant = crate::tests::claim_device(bare_row as u64).expect("claimed");
	assert_eq!(properties(bare_grant.memory, &mut buf), syscall::ERR_UNSUPPORTED, "a row with no property block has none to give");
	crate::tests::release_device(&bare_grant);
}

// EVERY IOMMU UNIT THE FIRMWARE NAMES IS ACCOUNTED FOR: a kernel-held row over its register window, never
// claimable. Said and skipped on a machine whose firmware names none - the default test machine has no DMAR.
#[cfg(target_arch = "x86_64")]
crate::tagged_test!(every_iommu_unit_the_firmware_names_is_a_kernel_held_row, [Drivers, Kernel, ArchX86_64], id = "kernel.platform_rows.every_iommu_unit_the_firmware_names_is_a_kernel_held_row", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn every_iommu_unit_the_firmware_names_is_a_kernel_held_row() {
	let rsdp = crate::boot_info().rsdp;
	let mut units: Vec<(&[u8; 4], acpi::IommuUnit)> = Vec::new();
	for signature in [b"DMAR", b"IVRS"] {
		if let Some(bytes) = crate::smp::acpi_table(rsdp, signature) {
			let _ = if signature == b"DMAR" { acpi::dmar_units(bytes, |unit| units.push((signature, unit))) } else { acpi::ivrs_units(bytes, |unit| units.push((signature, unit))) };
		}
	}
	if units.is_empty() {
		crate::serial_println!("platform-iommu: this machine's firmware names no IOMMU unit - nothing to account for");
		return;
	}
	for (signature, unit) in units.iter() {
		let row = (0..device::count()).find(|&index| device::info(index).is_some_and(|info| info.platform.kind == abi::ROW_KIND_PLATFORM && info.platform.mmio().iter().any(|range| range.base == unit.base && range.len == unit.len))).unwrap_or_else(|| panic!("the {} unit at {:#x} is a row", core::str::from_utf8(*signature).unwrap_or("?"), unit.base));
		let info = device::info(row).expect("the unit's row");
		assert_eq!(info.platform.state, abi::PLATFORM_STATE_KERNEL_HELD, "and the kernel's");
		assert!(info.platform.match_ids().iter().any(|id| id.kind == abi::MATCH_ID_TABLE && id.text() == *signature), "matched by its table");
		assert_eq!(device::claim(row, &dma_policy::entry_field(b"synthetic-none")), Err(device::ClaimError::Refused), "and never claimed");
	}
	crate::serial_println!("platform-iommu: {} IOMMU unit(s) the firmware names are kernel-held rows", units.len());
}

// THE FIXED-HARDWARE BUTTONS THE FADT DESCRIBES ARE CLAIMABLE ROWS THAT HOLD NOTHING, each matched by the id the
// `acpi-button` driver's rule names - QEMU's machine has a fixed power button and no fixed sleep button - and the claim
// that is where DeviceManager hands a press takes the row, and its release gives it back.
#[cfg(target_arch = "x86_64")]
crate::tagged_test!(the_fixed_buttons_are_claimable_rows_that_hold_nothing, [Drivers, Kernel, ArchX86_64], id = "kernel.platform_rows.the_fixed_buttons_are_claimable_rows_that_hold_nothing", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn the_fixed_buttons_are_claimable_rows_that_hold_nothing() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(row: u64) {
		let grant = crate::tests::claim_device(row).expect("the power button's row is claimed under the entry that declares it");
		assert_eq!(device::info(row as usize).map(|info| info.platform.state), Some(abi::PLATFORM_STATE_CLAIMABLE), "a claim does not change what the row is");
		assert_eq!(crate::tests::claim_device(row).map(|_| ()), Err(abi::ERR_ALREADY_CLAIMED), "one binding holds it");
		crate::tests::release_device(&grant);
		let again = crate::tests::claim_device(row).expect("and it is claimed again after the release");
		crate::tests::release_device(&again);
		DONE.store(true, Ordering::SeqCst);
	}
	let fadt = crate::smp::acpi_table(crate::boot_info().rsdp, b"FACP").and_then(|bytes| acpi::Fadt::new(bytes).ok()).expect("the test machine has a FADT");
	let fixed = arch::platform::fixed_of(&fadt);
	assert_eq!(fixed, abi::SLEEP_FIXED_POWER_BUTTON, "QEMU's machine declares a fixed power button and no fixed sleep button");
	let mut power = None;
	for (bit, identity, hid) in [
		(abi::SLEEP_FIXED_POWER_BUTTON, abi::PLATFORM_ROW_POWER_BUTTON, abi::PLATFORM_HID_POWER_BUTTON),
		(abi::SLEEP_FIXED_SLEEP_BUTTON, abi::PLATFORM_ROW_SLEEP_BUTTON, abi::PLATFORM_HID_SLEEP_BUTTON),
	] {
		let row = (0..device::count()).find(|&index| device::info(index).is_some_and(|info| info.platform.identity() == identity));
		let name = core::str::from_utf8(identity).unwrap_or("?");
		if fixed & bit == 0 {
			assert!(row.is_none(), "{name} is published for a button the FADT does not describe as fixed");
			continue;
		}
		let row = row.unwrap_or_else(|| panic!("{name} is not published"));
		let info = device::info(row).expect("the button's row");
		assert_eq!((info.platform.kind, info.platform.source, info.platform.state), (abi::ROW_KIND_PLATFORM, abi::PLATFORM_SOURCE_KERNEL, abi::PLATFORM_STATE_CLAIMABLE), "{name} is a claimable row of the kernel's");
		assert!(info.platform.match_ids().iter().any(|id| id.kind == abi::MATCH_ID_HID && id.text() == hid), "{name} carries its match id");
		assert!(info.platform.mmio().is_empty() && info.platform.lines().is_empty() && info.port_count == 0, "{name} holds nothing");
		let entry = crate::tests::entry_for_device(row as u64).unwrap_or_else(|| panic!("no rule of the manifest admits {name}"));
		assert!(entry.starts_with(b"acpi_button\0"), "{name} is the acpi-button driver's");
		power = Some(row);
	}
	in_thread(body, power.expect("the power button's row") as u64, &DONE);
}

// THE HPET THE TABLE DESCRIBES IS ACCOUNTED FOR, as QEMU writes the table - its base a window of width zero, which
// a reader that took it for half a register refused, leaving the timer's block owned by nobody.
#[cfg(target_arch = "x86_64")]
crate::tagged_test!(the_hpet_table_s_window_is_a_kernel_held_row, [Drivers, Kernel, ArchX86_64], id = "kernel.platform_rows.the_hpet_table_s_window_is_a_kernel_held_row", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn the_hpet_table_s_window_is_a_kernel_held_row() {
	let Some(hpet) = crate::smp::acpi_table(crate::boot_info().rsdp, b"HPET").map(|bytes| acpi::Hpet::new(bytes).expect("this machine's HPET table is read")) else {
		crate::serial_println!("platform-hpet: this machine has no HPET table");
		return;
	};
	let row = (0..device::count()).find(|&index| device::info(index).is_some_and(|info| info.platform.identity() == b"table:HPET#0")).expect("the HPET table's device is a row");
	let info = device::info(row).expect("the row");
	assert_eq!(info.platform.state, abi::PLATFORM_STATE_KERNEL_HELD);
	assert_eq!(info.platform.mmio().first().map(|range| range.base), Some(hpet.base.address), "over the table's window");
}
