// THE PORT-RANGE SUITE: the grant table and the mint paths on every port, and on x86_64 the permission
// bitmap every core loads, driven through ring-3 probes.
//
// EVERY PROBE THAT DRIVES A UART DRIVES THE TEST MACHINE'S SECOND ONE - an `isa-serial` at 0x2F8 on IRQ 3
// with a null chardev behind it - in loopback, so the oracle is read inside the guest and nothing is written
// into the serial log the suite is judged by. The probes that only need permission, not a device, loop on
// ports nothing answers at: an access is what is being tested, not what comes back.

use alloc::sync::Arc;

use super::grants::{self, Part, Refusal};
use super::*;
use crate::object::handle::Handle;
use crate::object::rights::Rights;
use crate::sync::SpinLock;
use crate::{device, sched, syscall};

#[cfg(target_arch = "x86_64")]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// A handle in the calling thread's table, as the object behind it.
fn range_of(handle: i64) -> Arc<PortRange> {
	let thread = sched::current_thread().expect("a current thread");
	let object = thread.handles().lock().lookup_typed(Handle::from_raw(handle as u64), ObjectType::PortRange, Rights::MAP).expect("a port-range handle");
	object.into_any_arc().downcast::<PortRange>().ok().expect("a PortRange")
}

// A FirmwareInterpreter privilege in the calling thread's table, as the ACPI service would hold one.
fn firmware_privilege() -> u64 {
	use crate::object::privilege::{Privilege, PrivilegeKind};
	let thread = sched::current_thread().expect("a current thread");
	let privilege = Privilege::create(PrivilegeKind::FirmwareInterpreter).expect("a test privilege");
	thread.handles().lock().try_insert_object(privilege, Rights::ALL).expect("the privilege installs").raw()
}

fn firmware_mint(privilege: u64, base: u64, len: u64) -> i64 {
	unsafe { crate::arch::syscall::invoke(abi::SYS_PORT_RANGE_FIRMWARE, privilege, base, len, 0) as i64 }
}

fn close(handle: i64) {
	let thread = sched::current_thread().expect("a current thread");
	thread.handles().lock().close(Handle::from_raw(handle as u64)).expect("the handle closes");
}

crate::tagged_test!(a_run_time_install_is_refused_while_granted_and_counts_its_holds, [Object, Kernel], id = "kernel.object.port_range.a_run_time_install_is_refused_while_granted_and_counts_its_holds", covers = ["kernel"]);
fn a_run_time_install_is_refused_while_granted_and_counts_its_holds() {
	if !crate::arch::ioports::supported() {
		return;
	}
	const ITEM: u32 = 0x7e57;
	const BASE: u16 = 0x2E8;
	// GRANTED FIRST: the install is refused while the grant lives.
	let range = PortRange::mint(BASE, 8, None).expect("a free range mints");
	assert_eq!(grants::install(ITEM, BASE, 8), Err(Refusal::Granted), "a port in a live grant cannot be installed");
	drop(range);
	// INSTALLED: every later mint of the port is refused, and so is a range that touches it by one port.
	assert_eq!(grants::install(ITEM, BASE, 8), Ok(()));
	assert!(matches!(PortRange::mint(BASE + 7, 1, None), Err(Refusal::Reserved(Part::Installed))), "an installed port refuses every mint");
	assert!(matches!(PortRange::mint(BASE - 1, 2, None), Err(Refusal::Reserved(Part::Installed))), "and a range overlapping it by one port");
	// INSTALLED TWICE BY THE SAME ITEM, it leaves the set only with the second uninstall.
	assert_eq!(grants::install(ITEM, BASE, 8), Ok(()), "the same item installs its own range again");
	assert_eq!(grants::install(ITEM + 1, BASE, 8), Err(Refusal::Reserved(Part::Installed)), "another item may not");
	assert!(grants::uninstall(ITEM, BASE, 8));
	assert!(PortRange::mint(BASE, 8, None).is_err(), "one hold is left, and the port is still reserved");
	assert!(grants::uninstall(ITEM, BASE, 8));
	assert!(!grants::uninstall(ITEM, BASE, 8), "a hold the item does not have is not released");
	let again = PortRange::mint(BASE, 8, None).expect("with the last hold gone the port mints again");
	drop(again);
}

crate::tagged_test!(every_reserved_port_is_refused_at_the_mint_and_every_terminal_one_is_not, [Object, Kernel, Syscall], id = "kernel.object.port_range.every_reserved_port_is_refused_at_the_mint_and_every_terminal_one_is_not", covers = ["kernel"]);
fn every_reserved_port_is_refused_at_the_mint_and_every_terminal_one_is_not() {
	if !crate::arch::ioports::supported() {
		return;
	}
	static DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		refused_and_terminal_ports();
		DONE.store(true, core::sync::atomic::Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

fn refused_and_terminal_ports() {
	let privilege = firmware_privilege();
	// THE FIXED PART, every entry at its first and its last port - the ISA DMA controllers' channel and
	// control registers among them, the first one's alias at 0x10 included - through the firmware
	// interpreter's path, the one call that names ports by address.
	for &(base, count, what) in grants::FIXED {
		for port in [base, base + count - 1] {
			assert_eq!(firmware_mint(privilege, port as u64, 1), syscall::ERR_ACCESS_DENIED, "port {port:#x} ({what}) is the kernel's");
		}
	}
	assert_eq!(firmware_mint(privilege, 0x10, 1), syscall::ERR_ACCESS_DENIED, "the first DMA controller's alias at 0x10");
	assert_eq!(firmware_mint(privilege, 0xF4, 4), syscall::ERR_ACCESS_DENIED, "and, in the test build, the exit device");
	// THE DMA CONTROLLERS' PAGE REGISTERS CANNOT START A TRANSFER, and stay mintable - as port 0x80 does.
	for port in [0x80u64, 0x81] {
		let page = firmware_mint(privilege, port, 1);
		assert!(page > 0, "port {port:#x} mints ({page})");
		close(page);
	}
	// THE FADT'S PART, recorded before the boot scan.
	let mut blocks = [(0u16, 0u16); 16];
	let count = crate::arch::ioports::firmware_blocks(&mut blocks);
	assert!(count > 0, "this machine's FADT names fixed-hardware blocks");
	for &(base, len) in &blocks[..count] {
		assert_eq!(firmware_mint(privilege, base as u64, len as u64), syscall::ERR_ACCESS_DENIED, "the firmware's block at {base:#x} is the kernel's");
		assert!(matches!(grants::recordable(base, len), Err(Refusal::Reserved(Part::Firmware))), "and it is in the firmware's part of the set");
	}
	// COM1, WHICH THE KERNEL CONSOLE DRIVES, through the firmware path and through a claim.
	assert_eq!(firmware_mint(privilege, 0x3F8, 8), syscall::ERR_ACCESS_DENIED, "COM1 is the kernel console's");
	let com1_row = device::add_synthetic_device_with_ports(&[(0x3F8, 8, abi::PORT_SOURCE_PLATFORM)]);
	let grant = crate::tests::claim_device(com1_row as u64).expect("the synthetic row is claimed");
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0) } as i64, syscall::ERR_ACCESS_DENIED, "and a claim's row cannot take it either");
	crate::tests::release_device(&grant);
	// A RANGE THAT TOUCHES A RESERVED ONE BY A SINGLE PORT is refused whole.
	assert_eq!(firmware_mint(privilege, 0x3F0, 9), syscall::ERR_ACCESS_DENIED, "0x3F0..0x3F8 overlaps COM1 by one port");
	// A SECOND GRANT OF A GRANTED PORT.
	let held = firmware_mint(privilege, 0x2E8, 8);
	assert!(held > 0);
	assert_eq!(firmware_mint(privilege, 0x2EF, 1), syscall::ERR_RESOURCE_EXHAUSTED, "a port is in one grant at a time");
	close(held);
	let after = firmware_mint(privilege, 0x2EF, 1);
	assert!(after > 0, "and free again once that grant is dropped");
	close(after);
	// PAST THE PORT SPACE, and nothing at all.
	assert_eq!(firmware_mint(privilege, 0xFFFF, 2), syscall::ERR_INVALID, "a range running past 0xFFFF");
	assert_eq!(firmware_mint(privilege, 0x1_0000, 1), syscall::ERR_INVALID, "a base past 0xFFFF");
	assert_eq!(firmware_mint(privilege, 0x2E8, 0), syscall::ERR_INVALID, "no ports");
	// ONLY THE FIRMWARE INTERPRETER NAMES PORTS BY ADDRESS: DeviceManager's privilege does not.
	assert_eq!(firmware_mint(crate::tests::device_privilege(), 0x2E8, 8), syscall::ERR_ACCESS_DENIED, "another privilege is refused");
	// THE TERMINAL-PATH CATEGORY IS MINTABLE - where the FADT does not already reserve the port.
	for &(base, count, what) in grants::TERMINAL {
		if matches!(grants::recordable(base, count), Err(Refusal::Reserved(Part::Firmware))) {
			continue;
		}
		let terminal = firmware_mint(privilege, base as u64, count as u64);
		assert!(terminal > 0, "{what} at {base:#x} mints ({terminal})");
		close(terminal);
	}
}

#[cfg(not(target_arch = "x86_64"))]
crate::tagged_test!(a_machine_with_no_port_space_mints_nothing, [Object, Kernel, Syscall], id = "kernel.object.port_range.a_machine_with_no_port_space_mints_nothing", covers = ["kernel"]);
#[cfg(not(target_arch = "x86_64"))]
fn a_machine_with_no_port_space_mints_nothing() {
	// EVERY CALL ANSWERS UNSUPPORTED, and no row carries a port resource.
	assert!(!crate::arch::ioports::supported());
	for number in [abi::SYS_DEVICE_RESOURCE_ACQUIRE, abi::SYS_PORT_RANGE_MAP, abi::SYS_PORT_RANGE_UNMAP, abi::SYS_PORT_RANGE_FIRMWARE] {
		assert_eq!(unsafe { crate::arch::syscall::invoke(number, 0, 0, 0, 0) } as i64, syscall::ERR_UNSUPPORTED, "syscall {number}");
	}
	for index in 0..crate::device::count() {
		assert_eq!(device::with(index, |row| row.port_count), Some(0), "row {index} carries no port");
	}
}

// ------------------------------------------------------------------------------------ the probes

#[cfg(target_arch = "x86_64")]
const UART: u16 = 0x2F8;

// Each probe slot has its own code, stack and data pages, so probes on several cores never share one.
#[cfg(target_arch = "x86_64")]
const PROBE_VA: u64 = 0x0000_0000_5000_0000;

#[cfg(target_arch = "x86_64")]
const LOOPBACK: u64 = 0;
#[cfg(target_arch = "x86_64")]
const LOOP: u64 = 1;
#[cfg(target_arch = "x86_64")]
const WAIT: u64 = 2;

// One ring-3 probe: what it runs, the ports it reaches, the range it maps before it drops to ring 3, and
// what came back.
#[cfg(target_arch = "x86_64")]
struct Probe {
	program: AtomicU64,
	port: AtomicU64,
	last: AtomicU64,
	range: SpinLock<Option<Arc<PortRange>>>,
	data: AtomicU64,
	fault: AtomicU64,
	read: AtomicU64,
	count: AtomicU64,
	done: AtomicBool,
}

#[cfg(target_arch = "x86_64")]
impl Probe {
	const fn new() -> Self {
		Self { program: AtomicU64::new(0), port: AtomicU64::new(0), last: AtomicU64::new(0), range: SpinLock::new(None), data: AtomicU64::new(0), fault: AtomicU64::new(0), read: AtomicU64::new(0), count: AtomicU64::new(0), done: AtomicBool::new(false) }
	}

	fn arm(&self, program: u64, port: u16, last: u16, range: Option<Arc<PortRange>>) {
		self.program.store(program, Ordering::SeqCst);
		self.port.store(port as u64, Ordering::SeqCst);
		self.last.store(last as u64, Ordering::SeqCst);
		*self.range.lock() = range;
		self.data.store(0, Ordering::SeqCst);
		self.fault.store(0, Ordering::SeqCst);
		self.read.store(0, Ordering::SeqCst);
		self.count.store(0, Ordering::SeqCst);
		self.done.store(false, Ordering::SeqCst);
	}

	// The probe's data page through the kernel's own map, once the probe has published it.
	fn page(&self) -> *mut u64 {
		let data = self.data.load(Ordering::SeqCst);
		assert!(data != 0, "the probe has not published its page");
		(crate::mem::hhdm_offset() + data) as *mut u64
	}

	// Raise the probe's flag.
	fn release(&self) {
		unsafe { self.page().add(1).write_volatile(1) };
	}

	// Wait until the probe's counter has moved by `steps` - proof its loop is running in ring 3.
	fn wait_counting(&self, steps: u64) {
		for _ in 0..200_000_000u64 {
			if self.data.load(Ordering::SeqCst) != 0 && unsafe { self.page().add(3).read_volatile() } >= steps {
				return;
			}
			pause();
		}
		panic!("the probe's loop never counted to {steps}");
	}

	fn wait_done(&self) {
		for _ in 0..2_000_000_000u64 {
			if self.done.load(Ordering::SeqCst) {
				return;
			}
			pause();
		}
		panic!("the probe never finished");
	}
}

#[cfg(target_arch = "x86_64")]
static PROBES: [Probe; 4] = [const { Probe::new() }; 4];

// One step of a wait: yield when waiting on a thread - a probe on this thread's own core runs only if it
// does - and spin when waiting from a test's own body, which is no thread and has nothing to yield to.
#[cfg(target_arch = "x86_64")]
fn pause() {
	if sched::current_thread().is_some() {
		sched::yield_now();
	} else {
		core::hint::spin_loop();
	}
}

#[cfg(target_arch = "x86_64")]
extern "C" fn probe_body(slot: u64) {
	use crate::arch::paging::{NO_EXECUTE, PRESENT, USER, WRITABLE};
	use crate::mem::frame::{self, PAGE_SIZE};
	let probe = &PROBES[slot as usize];
	let base = PROBE_VA + slot * 0x10_0000;
	let (code_va, stack_va, data_va) = (base, base + 0x1_0000, base + 0x2_0000);
	let code = frame::allocate().expect("probe code frame");
	let stack = frame::allocate().expect("probe stack frame");
	let data = frame::allocate().expect("probe data frame");
	let page = (crate::mem::hhdm_offset() + data) as *mut u64;
	unsafe {
		core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE as usize);
		page.write_volatile(probe.port.load(Ordering::SeqCst));
		page.add(4).write_volatile(probe.last.load(Ordering::SeqCst));
	}
	let flags = PRESENT | WRITABLE | USER;
	crate::arch::paging::map_page(code_va, code, flags);
	crate::arch::paging::map_page(stack_va, stack, flags | NO_EXECUTE);
	crate::arch::paging::map_page(data_va, data, flags | NO_EXECUTE);
	let program = match probe.program.load(Ordering::SeqCst) {
		LOOPBACK => crate::arch::usermode::program_port_loopback_bytes(),
		LOOP => crate::arch::usermode::program_port_loop_bytes(),
		_ => crate::arch::usermode::program_port_wait_bytes(),
	};
	unsafe { crate::arch::paging::copy_to_user_page(code_va, program) };
	{
		let thread = sched::current_thread().expect("a current thread");
		if let Some(range) = probe.range.lock().take() {
			range.map_into(thread.process()).expect("the probe maps its range");
		}
	}
	probe.data.store(data, Ordering::SeqCst);
	unsafe { crate::arch::usermode::enter(code_va, stack_va + PAGE_SIZE, data_va) };
	let mut info = crate::fault::FaultInfo { kind: 0, error_code: 0, address: 0, instruction_pointer: 0 };
	let got = unsafe { crate::arch::syscall::invoke(syscall::SYS_FAULT_INFO_GET, &mut info as *mut crate::fault::FaultInfo as u64, core::mem::size_of::<crate::fault::FaultInfo>() as u64, 0, 0) } as i64;
	probe.fault.store(if got > 0 { info.kind } else { 0 }, Ordering::SeqCst);
	unsafe {
		probe.read.store(page.add(2).read_volatile(), Ordering::SeqCst);
		probe.count.store(page.add(3).read_volatile(), Ordering::SeqCst);
	}
	probe.data.store(0, Ordering::SeqCst);
	crate::arch::paging::unmap_page(code_va);
	crate::arch::paging::unmap_page(stack_va);
	crate::arch::paging::unmap_page(data_va);
	unsafe {
		frame::deallocate(code);
		frame::deallocate(stack);
		frame::deallocate(data);
	}
	probe.done.store(true, Ordering::SeqCst);
}

// Start probe `slot` on core `cpu`, waking that core.
#[cfg(target_arch = "x86_64")]
fn launch(slot: u64, cpu: usize) {
	sched::spawn_on(cpu, probe_body, slot);
}

// A claimed synthetic row carrying one port resource, and the range the claim mints from it - through the
// kernel's own calls, as the syscall does, for the tests whose subject is what happens AFTER the mint.
#[cfg(target_arch = "x86_64")]
fn claimed_range(base: u16, len: u16) -> (usize, abi::ClaimKey, Arc<PortRange>) {
	let index = device::add_synthetic_device_with_ports(&[(base, len, abi::PORT_SOURCE_PLATFORM)]);
	let key = device::claim(index, &crate::dma_policy::synthetic_entry()).expect("a fresh synthetic row claims");
	let resource = device::port_resource(index, 0).expect("the row's port resource");
	let range = PortRange::mint(resource.base, resource.len, Some(key)).expect("the claim mints its row's range");
	let weak: alloc::sync::Weak<dyn KernelObject> = Arc::downgrade(&(range.clone() as Arc<dyn KernelObject>));
	assert!(device::register_derived(key, weak), "the range is derived from the live claim");
	(index, key, range)
}

// RUN `body` ON A THREAD OF ITS OWN, on this core, to its end: the syscall paths need a caller with a handle
// table, and a test's own body has none.
fn in_thread(body: extern "C" fn(u64), done: &'static core::sync::atomic::AtomicBool) {
	done.store(false, core::sync::atomic::Ordering::SeqCst);
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(done.load(core::sync::atomic::Ordering::SeqCst), "the test's thread ran to its end");
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_claimed_range_reaches_its_uart_and_a_port_beside_it_ends_the_probe, [Object, Kernel, Syscall, Process, ArchX86_64], id = "kernel.object.port_range.a_claimed_range_reaches_its_uart_and_a_port_beside_it_ends_the_probe", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_claimed_range_reaches_its_uart_and_a_port_beside_it_ends_the_probe() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		through_the_syscalls();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

// THE WHOLE PATH A DRIVER'S RANGE TAKES, through the syscalls DeviceManager and the driver make.
#[cfg(target_arch = "x86_64")]
fn through_the_syscalls() {
	let row = device::add_synthetic_device_with_ports(&[(UART, 8, abi::PORT_SOURCE_PLATFORM)]);
	let grant = crate::tests::claim_device(row as u64).expect("the synthetic row is claimed");
	let handle = unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0) } as i64;
	assert!(handle > 0, "the claim mints its row's range ({handle})");
	let range = range_of(handle);
	assert_eq!((range.base, range.len), (UART, 8), "the row's resource, by index");
	// WHAT THE HANDLE CARRIES: the right to map it and to pass it on, and nothing else.
	let thread = sched::current_thread().expect("a current thread");
	assert_eq!(thread.handles().lock().rights_of(Handle::from_raw(handle as u64)), Ok(Rights::MAP | Rights::TRANSFER));
	drop(thread);
	// A SECOND MINT WHILE THE FIRST LIVES is refused: a port is in one grant at a time.
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0) } as i64, syscall::ERR_RESOURCE_EXHAUSTED);
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 1, 0) } as i64, syscall::ERR_INVALID, "an index past the row's resources");
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, 2, 0, 0) } as i64, syscall::ERR_INVALID, "a kind nothing mints");
	// MAPPED AND UNMAPPED BY ITS HOLDER through the syscalls: the unmap answers that every core confirmed.
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_PORT_RANGE_MAP, handle as u64, 0, 0, 0) } as i64, 0);
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_PORT_RANGE_MAP, handle as u64, 0, 0, 0) } as i64, syscall::ERR_RESOURCE_EXHAUSTED, "a range is mapped into one process at a time");
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_PORT_RANGE_UNMAP, handle as u64, 0, 0, 0) } as i64, 1);
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_PORT_RANGE_UNMAP, handle as u64, 0, 0, 0) } as i64, syscall::ERR_INVALID, "and an unmap of what is not mapped here is refused");
	// THE POSITIVE CASE: a ring-3 probe that mapped the range round-trips a byte through the UART's
	// loopback - and then reads the port just BELOW the range, which faults and ends that probe alone.
	let probe = &PROBES[0];
	probe.arm(LOOPBACK, UART, UART - 1, Some(range.clone()));
	launch(0, sched::current_cpu_id());
	probe.wait_done();
	assert_eq!(probe.read.load(Ordering::SeqCst), 0x5A, "the byte came back through the loopback");
	assert_eq!(probe.fault.load(Ordering::SeqCst), crate::fault::FAULT_GENERAL_PROTECTION, "a port beside the range is a general protection fault");
	// NOTHING ELSE ENDED: the claim is still this binding's, and the range is back with its holder - the
	// dead probe's teardown gave it back.
	assert_eq!(device::claim_state(row), Some(device::ClaimState::Claimed));
	assert!(matches!(*range.state.lock(), State::Idle), "a terminated holder gives the range back");
	// THE RELEASE REVOKES IT, and the ports are free for the next claimant.
	crate::tests::release_device(&grant);
	assert_eq!(device::claim_state(row), Some(device::ClaimState::Free));
	assert!(matches!(*range.state.lock(), State::Ended), "the claim's release ended the grant");
	drop(range);
	close(handle);
	let again = PortRange::mint(UART, 8, None).expect("the ports are free once the release confirmed");
	drop(again);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_range_released_under_a_thread_looping_on_it_faults_its_next_access, [Object, Kernel, Smp, Process, ArchX86_64], id = "kernel.object.port_range.a_range_released_under_a_thread_looping_on_it_faults_its_next_access", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_range_released_under_a_thread_looping_on_it_faults_its_next_access() {
	if crate::smp::cpu_count() < 2 {
		crate::serial_println!("NOT RUN: a range revoked on another core needs two cores");
		return;
	}
	let (row, key, range) = claimed_range(UART, 8);
	let probe = &PROBES[0];
	// The loop reads the UART's scratch register, which answers without side effects.
	probe.arm(LOOP, UART + 7, UART + 7, Some(range.clone()));
	launch(0, 1);
	probe.wait_counting(1000);
	// THE RELEASE, from this core, while core 1 is inside the loop: the round makes core 1 copy the cleared
	// bitmap, so its next access faults - and the release confirms.
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free), "the revocation round confirmed on every core");
	assert_eq!(device::claim_state(row), Some(device::ClaimState::Free));
	probe.wait_done();
	assert_eq!(probe.fault.load(Ordering::SeqCst), crate::fault::FAULT_GENERAL_PROTECTION, "the loop's next access faulted");
	drop(range);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_release_whose_round_one_core_does_not_answer_is_quarantined, [Object, Kernel, Smp, Process, ArchX86_64], id = "kernel.object.port_range.a_release_whose_round_one_core_does_not_answer_is_quarantined", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_release_whose_round_one_core_does_not_answer_is_quarantined() {
	if crate::smp::cpu_count() < 2 {
		crate::serial_println!("NOT RUN: a round one core does not answer needs two cores");
		return;
	}
	// ITS OWN PORTS: this test retires them for the rest of the boot.
	const RETIRED: u16 = 0x3E8;
	let (row, key, range) = claimed_range(RETIRED, 8);
	let probe = &PROBES[1];
	probe.arm(LOOP, RETIRED, RETIRED, Some(range.clone()));
	launch(1, 1);
	probe.wait_counting(1000);
	// CORE 1 STOPS ANSWERING, and the release cannot confirm that it stopped letting the probe through.
	crate::mem::tlb::hold_back_for_test(Some(1));
	let state = device::release_claim(key);
	crate::mem::tlb::hold_back_for_test(None);
	assert_eq!(state, Ok(device::ClaimState::Quarantined), "an unconfirmed round quarantines the claim");
	assert_eq!(device::claim_state(row), Some(device::ClaimState::Quarantined));
	assert!(grants::retired_covering(RETIRED) && grants::retired_covering(RETIRED + 7), "and its ports are out of every later grant this boot");
	assert!(matches!(PortRange::mint(RETIRED, 8, None), Err(Refusal::Retired)));
	// The probe may still hold its stale copy - which is what the quarantine is for - so it is stopped by
	// hand; the next round core 1 answers makes it copy again.
	probe.release();
	probe.wait_done();
	drop(range);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_release_confirms_while_the_looping_core_s_scheduler_lock_is_held, [Object, Kernel, Smp, Scheduler, Process, ArchX86_64], id = "kernel.object.port_range.a_release_confirms_while_the_looping_core_s_scheduler_lock_is_held", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_release_confirms_while_the_looping_core_s_scheduler_lock_is_held() {
	if crate::smp::cpu_count() < 3 {
		crate::serial_println!("NOT RUN: holding one core's scheduler lock from another needs three cores");
		return;
	}
	static HELD: AtomicBool = AtomicBool::new(false);
	static LET_GO: AtomicBool = AtomicBool::new(false);
	extern "C" fn holder(cpu: u64) {
		sched::hold_run_queue_lock(cpu as usize, &HELD, &LET_GO);
	}
	HELD.store(false, Ordering::SeqCst);
	LET_GO.store(false, Ordering::SeqCst);
	let (row, key, range) = claimed_range(UART, 8);
	let probe = &PROBES[2];
	probe.arm(LOOP, UART + 7, UART + 7, Some(range.clone()));
	launch(2, 1);
	probe.wait_counting(1000);
	// CORE 2 TAKES CORE 1'S SCHEDULER LOCK AND KEEPS IT.
	sched::spawn_on(2, holder, 1);
	while !HELD.load(Ordering::SeqCst) {
		core::hint::spin_loop();
	}
	// THE ROUND STILL CONFIRMS: core 1's service step reads only its own record.
	let state = device::release_claim(key);
	LET_GO.store(true, Ordering::SeqCst);
	assert_eq!(state, Ok(device::ClaimState::Free), "the revocation confirmed with the looping core's scheduler lock held elsewhere");
	assert_eq!(device::claim_state(row), Some(device::ClaimState::Free));
	probe.wait_done();
	assert_eq!(probe.fault.load(Ordering::SeqCst), crate::fault::FAULT_GENERAL_PROTECTION, "and the loop's next access faulted");
	drop(range);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(after_a_kill_the_next_process_on_that_core_faults_on_the_range, [Object, Kernel, Process, ArchX86_64], id = "kernel.object.port_range.after_a_kill_the_next_process_on_that_core_faults_on_the_range", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn after_a_kill_the_next_process_on_that_core_faults_on_the_range() {
	let range = PortRange::mint(UART, 8, None).expect("the range mints");
	let here = sched::current_cpu_id();
	// THE HOLDER reaches its range and is then killed by a port outside it.
	let holder = &PROBES[0];
	holder.arm(LOOPBACK, UART, UART - 1, Some(range.clone()));
	launch(0, here);
	sched::run_until_idle();
	holder.wait_done();
	assert_eq!((holder.read.load(Ordering::SeqCst), holder.fault.load(Ordering::SeqCst)), (0x5A, crate::fault::FAULT_GENERAL_PROTECTION));
	// THE NEXT PROCESS ON THAT CORE holds no range, and faults on the first port of the one before.
	let next = &PROBES[1];
	next.arm(LOOP, UART + 7, UART + 7, None);
	launch(1, here);
	sched::run_until_idle();
	next.wait_done();
	assert_eq!(next.fault.load(Ordering::SeqCst), crate::fault::FAULT_GENERAL_PROTECTION, "the core did not keep the killed process's bitmap for the next one");
	assert_eq!(next.count.load(Ordering::SeqCst), 0, "not one access got through");
	drop(range);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(two_processes_with_different_ranges_alternate_on_one_core, [Object, Kernel, Process, Scheduler, ArchX86_64], id = "kernel.object.port_range.two_processes_with_different_ranges_alternate_on_one_core", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn two_processes_with_different_ranges_alternate_on_one_core() {
	if crate::smp::cpu_count() < 2 {
		crate::serial_println!("NOT RUN: the two processes run on a core of their own");
		return;
	}
	let first = PortRange::mint(UART, 8, None).expect("the first range");
	let second = PortRange::mint(0x2E8, 8, None).expect("the second range");
	// BOTH LOOP ON THEIR OWN PORTS on core 1, preempted in turn by the timer - every switch between them
	// loads the other's bitmap - and when released each reads the OTHER's port once.
	let a = &PROBES[0];
	let b = &PROBES[1];
	a.arm(LOOP, UART + 7, 0x2EF, Some(first.clone()));
	b.arm(LOOP, 0x2EF, UART + 7, Some(second.clone()));
	launch(0, 1);
	launch(1, 1);
	a.wait_counting(5000);
	b.wait_counting(5000);
	// Both counted, with the other running between their turns: each reached its own range on a core that
	// kept switching between the two.
	a.release();
	b.release();
	a.wait_done();
	b.wait_done();
	assert_eq!(a.fault.load(Ordering::SeqCst), crate::fault::FAULT_GENERAL_PROTECTION, "the first faulted on the second's port");
	assert_eq!(b.fault.load(Ordering::SeqCst), crate::fault::FAULT_GENERAL_PROTECTION, "and the second on the first's");
	drop(first);
	drop(second);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_sibling_thread_already_running_reaches_a_range_mapped_after_it_started, [Object, Kernel, Smp, Process, ArchX86_64], id = "kernel.object.port_range.a_sibling_thread_already_running_reaches_a_range_mapped_after_it_started", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_sibling_thread_already_running_reaches_a_range_mapped_after_it_started() {
	if crate::smp::cpu_count() < 2 {
		crate::serial_println!("NOT RUN: a sibling on another core needs two cores");
		return;
	}
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		sibling_pulls_the_grant();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

#[cfg(target_arch = "x86_64")]
fn sibling_pulls_the_grant() {
	let range = PortRange::mint(UART, 8, None).expect("the range mints");
	// THE SIBLING, a second thread of THIS thread's process, starts on core 1 and spins in ring 3 touching no
	// port.
	let sibling = &PROBES[3];
	sibling.arm(WAIT, UART + 7, UART + 7, None);
	let process = sched::current_thread().expect("a current thread").process().clone();
	let thread = crate::object::thread::Thread::new_for_cpu(probe_body, 3, process.clone(), Some(1)).expect("a sibling thread");
	sched::start_thread_on(1, &thread);
	crate::arch::apic::send_wake_ipi(crate::smp::lapic_id(1));
	drop(thread);
	while sibling.data.load(Ordering::SeqCst) == 0 {
		core::hint::spin_loop();
	}
	// ITS CORE HAS LOADED THE PROCESS WITH NO RANGE. The range is mapped from here, with no round - and the
	// sibling's first access faults once, pulls the new bitmap and retries.
	range.map_into(&process).expect("this process maps the range");
	sibling.release();
	sibling.wait_done();
	assert_eq!(sibling.fault.load(Ordering::SeqCst), 0, "the sibling was not killed");
	assert_eq!(sibling.count.load(Ordering::SeqCst), 1, "and reached the port");
	assert_eq!(range.unmap_from(&process), Ok(true), "and the range comes back, confirmed");
	drop(range);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(the_scan_recorded_the_smbus_io_bar_and_the_tco_sub_range_and_refused_pm1, [Object, Kernel, Pci, ArchX86_64], id = "kernel.object.port_range.the_scan_recorded_the_smbus_io_bar_and_the_tco_sub_range_and_refused_pm1", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn the_scan_recorded_the_smbus_io_bar_and_the_tco_sub_range_and_refused_pm1() {
	// Q35'S ICH9 SMBUS: its I/O BAR (BAR 4) recorded as its configuration space holds it.
	let smbus = (0..device::count()).find(|&index| device::with(index, |row| (row.vendor, row.product) == (0x8086, 0x2930)).unwrap_or(false)).expect("q35 carries an ICH9 SMBus controller");
	let (bus, dev, func, ports) = device::with(smbus, |row| (row.bus, row.dev, row.func, row.ports[..row.port_count as usize].to_vec())).expect("the row");
	let bar = crate::arch::pci::config_read32(bus, dev, func, 0x20);
	let base = (bar & 0xFFFC) as u16;
	assert!(base != 0, "firmware placed the SMBus I/O BAR");
	assert!(ports.iter().any(|port| port.base == base && port.source == abi::PORT_SOURCE_IO_BAR && port.index == 4 && port.len >= 32), "the row records BAR 4 at {base:#x}: {ports:?}");
	// Q35'S ICH9 LPC BRIDGE: the test build's derivation rows over its ACPI PM block. The TCO sub-range is
	// recorded; PM1's, which the FADT puts in the reserved set, is not.
	let lpc = (0..device::count()).find(|&index| device::with(index, |row| (row.vendor, row.product) == (0x8086, 0x2918)).unwrap_or(false)).expect("q35 carries an ICH9 LPC bridge");
	let (bus, dev, func, ports) = device::with(lpc, |row| (row.bus, row.dev, row.func, row.ports[..row.port_count as usize].to_vec())).expect("the row");
	let pm_base = (crate::arch::pci::config_read32(bus, dev, func, 0x40) & 0xFF80) as u16;
	assert!(pm_base != 0, "the PM block has a base");
	assert!(ports.iter().any(|port| port.base == pm_base + 0x60 && port.len == 32 && port.source == abi::PORT_SOURCE_DERIVED), "the TCO sub-range is recorded: {ports:?}");
	assert!(!ports.iter().any(|port| port.base == pm_base), "and PM1's is not: {ports:?}");
	assert!(matches!(grants::recordable(pm_base, 4), Err(Refusal::Reserved(Part::Firmware))), "PM1 is the kernel's, inside the same block");
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_claimed_io_bar_is_minted_with_the_claim_and_revoked_by_its_release, [Object, Kernel, Pci, Syscall, ArchX86_64], id = "kernel.object.port_range.a_claimed_io_bar_is_minted_with_the_claim_and_revoked_by_its_release", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_claimed_io_bar_is_minted_with_the_claim_and_revoked_by_its_release() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		an_io_bar_through_its_claim();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

#[cfg(target_arch = "x86_64")]
fn an_io_bar_through_its_claim() {
	let row = device::add_synthetic_device_with_ports(&[(UART, 8, abi::PORT_SOURCE_IO_BAR)]);
	let grant = crate::tests::claim_device(row as u64).expect("the synthetic row is claimed");
	let handle = unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0) } as i64;
	assert!(handle > 0, "the claim mints its I/O BAR ({handle})");
	let range = range_of(handle);
	let probe = &PROBES[0];
	probe.arm(LOOPBACK, UART, UART + 7, Some(range.clone()));
	launch(0, sched::current_cpu_id());
	probe.wait_done();
	assert_eq!((probe.read.load(Ordering::SeqCst), probe.fault.load(Ordering::SeqCst)), (0x5A, 0), "the probe reached the device through its BAR");
	crate::tests::release_device(&grant);
	assert!(matches!(*range.state.lock(), State::Ended), "the release revoked the range");
	let thread = sched::current_thread().expect("a current thread");
	assert!(thread.handles().lock().lookup_typed(Handle::from_raw(handle as u64), ObjectType::PortRange, Rights::MAP).is_err(), "and the handle no longer resolves");
	drop(thread);
	drop(range);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_thread_switch_is_measured_with_and_without_a_mapped_range, [Object, Kernel, Scheduler, ArchX86_64], id = "kernel.object.port_range.a_thread_switch_is_measured_with_and_without_a_mapped_range", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_thread_switch_is_measured_with_and_without_a_mapped_range() {
	// THE COST OF THE SWITCH-TIME COPY, measured rather than assumed: two threads of two processes yield to
	// each other on one core, first with neither holding a range, then with one of them holding COM1-sized
	// eight ports - so every switch into it copies its bitmap and every switch out resets nothing, since
	// the other process has no range at all.
	static STARTED: AtomicU64 = AtomicU64::new(0);
	static ENDED: AtomicU64 = AtomicU64::new(0);
	static HOLDER: SpinLock<Option<Arc<PortRange>>> = SpinLock::new(None);
	const ROUNDS: u64 = 20_000;
	extern "C" fn bouncer(holds: u64) {
		if holds != 0
			&& let Some(range) = HOLDER.lock().take()
		{
			let thread = sched::current_thread().expect("a current thread");
			range.map_into(thread.process()).expect("the bouncer maps its range");
		}
		let _ = STARTED.compare_exchange(0, crate::arch::tsc::now(), Ordering::AcqRel, Ordering::Acquire);
		for _ in 0..ROUNDS {
			sched::yield_now();
		}
		ENDED.fetch_max(crate::arch::tsc::now(), Ordering::AcqRel);
	}
	for (label, holds) in [("no range", 0u64), ("one side holds a range", 1)] {
		let mut best = u64::MAX;
		for _ in 0..3 {
			STARTED.store(0, Ordering::Release);
			ENDED.store(0, Ordering::Release);
			if holds != 0 {
				*HOLDER.lock() = Some(PortRange::mint(UART, 8, None).expect("the measured range"));
			}
			sched::spawn(bouncer, holds);
			sched::spawn(bouncer, 0);
			sched::run_until_idle();
			let ns = crate::arch::tsc::cycles_to_ns(ENDED.load(Ordering::Acquire).wrapping_sub(STARTED.load(Ordering::Acquire))) / (2 * ROUNDS);
			best = best.min(ns);
		}
		crate::serial_println!("switch-cost: {label}: {best} ns per switch (best of three, {} switches each)", 2 * ROUNDS);
	}
}
