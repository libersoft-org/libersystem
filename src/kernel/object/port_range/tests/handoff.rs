// THE COM1 HANDOFF'S MACHINERY, ON THE SECOND UART AS A SECOND INSTANCE OF THE KERNEL'S 16550 CODE.
//
// Its base is a parameter, so nothing here touches COM1 - the wire this suite is judged by. What the kernel does
// to the instance is read from the test build's record of every access to its registers, which is what shows the
// ORDER of a reacquisition and that no kernel path touched the UART while a probe held it. The instance is
// "adopted" for each test - its ports installed in the reserved set under the console's item, as COM1's are
// from the kernel's first line - and given up again at the end, because the rest of the port-range suite uses the
// same UART as an ordinary device.

use super::*;
use crate::arch::serial::{Access, COM2, Owner, Path};

const COM2_BASE: u16 = 0x2F8;
const RBR_THR: u16 = 0;
const IER: u16 = 1;
const FCR: u16 = 2;
const LCR: u16 = 3;
const MCR: u16 = 4;

// THE KERNEL DRIVES THE INSTANCE, as it drives COM1: its ports reserved to the console, every register programmed.
fn adopt(receive: bool) -> usize {
	COM2.reset_for_test();
	COM2.set_receive(receive);
	grants::install(grants::KERNEL_CONSOLE, COM2_BASE, 8).expect("the second UART's ports are free to reserve");
	COM2.init_for_test();
	let _ = COM2.take_read_out();
	device::synthetic_console_row(COM2_BASE)
}

// AND GIVES IT UP, so the rest of the suite finds the UART free.
fn give_up() {
	assert!(grants::uninstall(grants::KERNEL_CONSOLE, COM2_BASE, 8), "the console's hold on the second UART is back in the reserved set");
	COM2.reset_for_test();
	COM2.set_receive(false);
}

fn claim(row: usize) -> abi::ClaimKey {
	device::claim(row, &crate::dma_policy::entry_field(b"synthetic-platform")).expect("the console row is claimed")
}

// The claim's port range, through the handoff's mint.
fn console_range(key: abi::ClaimKey) -> Arc<PortRange> {
	let range = PortRange::mint_from_install(grants::KERNEL_CONSOLE, COM2_BASE, 8, key).expect("the claim's range is the one mint the reserved set admits");
	let weak: alloc::sync::Weak<dyn KernelObject> = Arc::downgrade(&(range.clone() as Arc<dyn KernelObject>));
	assert!(device::register_derived(key, weak), "the range is derived from the live claim");
	range
}

fn writes_since(from: usize) -> alloc::vec::Vec<Access> {
	COM2.record_from(from).into_iter().filter(|access| access.write).collect()
}

// THE BOOT SEQUENCE, as the record shows a write of it: interrupt enables off, the latch, the divisor, the latch
// closed with 8N1, the FIFOs enabled AND CLEARED, modem control with loopback off - and the receive interrupt last.
fn is_boot_sequence(writes: &[Access], receive: bool) -> bool {
	let expected: &[(u16, u8)] = &[(IER, 0), (LCR, 0x80), (RBR_THR, 0x03), (IER, 0x00), (LCR, 0x03), (FCR, 0xC7), (MCR, 0x0B)];
	writes.len() >= expected.len() && writes.iter().zip(expected).all(|(access, &(offset, value))| access.offset == offset && access.value == value) && (writes.get(expected.len()).map(|access| (access.offset, access.value)) == Some((IER, 1))) == receive
}

crate::tagged_test!(the_tap_moves_the_console_ring_out_in_order_and_counts_what_its_bound_dropped, [Object, Kernel, Syscall, ArchX86_64], id = "kernel.object.port_range.handoff.the_tap_moves_the_console_ring_out_in_order_and_counts_what_its_bound_dropped", covers = ["kernel"]);
fn the_tap_moves_the_console_ring_out_in_order_and_counts_what_its_bound_dropped() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(row: u64) {
		tap_in_order(row as usize);
		DONE.store(true, Ordering::SeqCst);
	}
	let row = adopt(false);
	DONE.store(false, Ordering::SeqCst);
	sched::spawn(body, row as u64);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the test's thread ran to its end");
	give_up();
}

fn tap_read(tap: i64, buf: &mut [u8]) -> (i64, u64) {
	let mut dropped = 0u64;
	let n = unsafe { crate::arch::syscall::invoke(abi::SYS_CONSOLE_TAP_READ, tap as u64, buf.as_mut_ptr() as u64, buf.len() as u64, &mut dropped as *mut u64 as u64) } as i64;
	(n, dropped)
}

fn tap_in_order(row: usize) {
	let grant = crate::tests::claim_device(row as u64).expect("the console row is claimed through the syscall");
	assert!(matches!(COM2.owner(), Owner::Driver(generation) if generation == grant.key.generation), "the claim flipped the owner before anything was minted");
	let tap = unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_CONSOLE_TAP, 0, 0) } as i64;
	assert!(tap > 0, "the claim mints its tap ({tap})");
	let tap_object = {
		let thread = sched::current_thread().expect("a current thread");
		let object = thread.handles().lock().lookup_typed(Handle::from_raw(tap as u64), ObjectType::ConsoleTap, Rights::READ).expect("a tap handle");
		object.into_any_arc().downcast::<crate::object::console_tap::ConsoleTap>().ok().expect("a ConsoleTap")
	};
	let before = COM2.record_len();
	// OUTPUT IN ORDER, AND NOTHING ON THE UART: the kernel's own lines and a debug write, as the ring carries them.
	for &byte in b"first line\n" {
		if byte == b'\n' {
			COM2.write_byte(b'\r');
		}
		COM2.write_byte(byte);
	}
	assert_eq!(COM2.write_bytes(b"a debug write\n"), 14);
	assert!(!tap_object.is_pending(), "not told yet - the signal waits for a context that holds no lock");
	COM2.drain_tx();
	assert!(tap_object.is_pending(), "the tick's drain tells the tap the ring holds bytes");
	let mut buf = [0u8; 64];
	let (n, dropped) = tap_read(tap, &mut buf);
	assert_eq!(&buf[..n as usize], b"first line\r\na debug write\r\n", "the tap moves the ring out in order");
	assert_eq!(dropped, 0);
	assert!(!tap_object.is_pending(), "a read that emptied the ring clears the tap");
	assert_eq!(tap_read(tap, &mut buf), (0, 0), "and an empty ring reads nothing");
	// PAST THE BOUND: the ring holds sixteen kilobytes; what does not fit is dropped and counted, never drained to
	// the UART by the kernel.
	const CAP: usize = 16384;
	for at in 0..CAP + 100 {
		COM2.write_byte(b'a' + (at % 26) as u8);
	}
	let mut total = 0usize;
	let mut reported = 0u64;
	let mut chunk = [0u8; 1024];
	loop {
		let (n, dropped) = tap_read(tap, &mut chunk);
		assert!(n >= 0, "the claim still holds the UART");
		reported += dropped;
		if n == 0 {
			break;
		}
		assert!(chunk[..n as usize].iter().enumerate().all(|(at, &byte)| byte == b'a' + ((total + at) % 26) as u8), "the oldest bytes, in order");
		total += n as usize;
	}
	assert_eq!(total, CAP, "the ring gave up everything it held");
	assert_eq!(reported, 100, "and said how many its bound dropped, once");
	assert_eq!(COM2.record_len(), before, "and no kernel path touched the UART while the claim held it");
	// THE RELEASE ENDS THE TAP and gives the UART back.
	crate::tests::release_device(&grant);
	assert_eq!(COM2.owner(), Owner::Kernel, "the release gave the UART back to the kernel");
	assert_eq!(tap_read(tap, &mut buf).0, syscall::ERR_BAD_HANDLE, "the release revoked the tap's capability, and it reads nothing");
	close(tap);
}

crate::tagged_test!(the_console_uart_s_ports_are_the_kernel_s_until_its_claim_mints_them_and_again_after_the_release, [Object, Kernel, Syscall, ArchX86_64], id = "kernel.object.port_range.handoff.the_console_uart_s_ports_are_the_kernel_s_until_its_claim_mints_them_and_again_after_the_release", covers = ["kernel"]);
fn the_console_uart_s_ports_are_the_kernel_s_until_its_claim_mints_them_and_again_after_the_release() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(row: u64) {
		ports_follow_the_owner(row as usize);
		DONE.store(true, Ordering::SeqCst);
	}
	let row = adopt(false);
	DONE.store(false, Ordering::SeqCst);
	sched::spawn(body, row as u64);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the test's thread ran to its end");
	give_up();
}

fn ports_follow_the_owner(row: usize) {
	let privilege = firmware_privilege();
	// WHILE THE KERNEL DRIVES IT: in the reserved set, refused to the firmware interpreter's path and to a row.
	assert!(matches!(grants::recordable(COM2_BASE, 8), Err(Refusal::Reserved(Part::Installed))), "the ports are the console's install");
	assert_eq!(firmware_mint(privilege, COM2_BASE as u64, 8), syscall::ERR_ACCESS_DENIED, "the firmware interpreter cannot take them");
	assert_eq!(firmware_mint(privilege, COM2_BASE as u64 + 7, 1), syscall::ERR_ACCESS_DENIED, "not even one of them");
	// THE CLAIM'S RANGE IS THE ONE MINT THE SET ADMITS.
	let grant = crate::tests::claim_device(row as u64).expect("the console row is claimed");
	let range = unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0) } as i64;
	assert!(range > 0, "the claim mints the console UART's range ({range})");
	assert!(matches!(grants::recordable(COM2_BASE, 8), Err(Refusal::Granted)), "the ports moved out of the reserved set into the claim's grant");
	assert_eq!(firmware_mint(privilege, COM2_BASE as u64, 8), syscall::ERR_RESOURCE_EXHAUSTED, "and the firmware path meets that grant");
	assert_eq!(unsafe { crate::arch::syscall::invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0) } as i64, syscall::ERR_RESOURCE_EXHAUSTED, "a second range of one claim is a second grant of granted ports");
	// THE RELEASE PUTS THEM BACK INTO THE RESERVED SET, in the same step as the grant ends.
	crate::tests::release_device(&grant);
	assert!(matches!(grants::recordable(COM2_BASE, 8), Err(Refusal::Reserved(Part::Installed))), "back in the reserved set after the release");
	assert_eq!(firmware_mint(privilege, COM2_BASE as u64, 8), syscall::ERR_ACCESS_DENIED, "and refused to every mint again");
	close(range);
	close(privilege as i64);
}

crate::tagged_test!(while_a_probe_holds_the_console_uart_no_kernel_path_touches_it_and_its_release_reads_out_before_the_reset, [Object, Kernel, Process, ArchX86_64], id = "kernel.object.port_range.handoff.while_a_probe_holds_the_console_uart_no_kernel_path_touches_it_and_its_release_reads_out_before_the_reset", covers = ["kernel"]);
fn while_a_probe_holds_the_console_uart_no_kernel_path_touches_it_and_its_release_reads_out_before_the_reset() {
	if crate::smp::cpu_count() < 2 {
		crate::serial_println!("NOT RUN: a probe holding the UART on another core needs two cores");
		return;
	}
	let row = adopt(true);
	let key = claim(row);
	let range = console_range(key);
	// A PROBE LEAVES THE UART AS A DRIVER MIGHT: bytes in its receiver - sent to itself in loopback - and the
	// divisor latch open, which hides the receiver behind the divisor.
	let probe = &PROBES[0];
	probe.arm_script(&[(COM2_BASE + LCR, 0x03), (COM2_BASE + MCR, 0x10), (COM2_BASE, b'A'), (COM2_BASE, b'B'), (COM2_BASE, b'C'), (COM2_BASE + LCR, 0x83)], range.clone());
	launch(0, 1);
	probe.wait_counting(1);
	// EVERY ORDINARY KERNEL PATH, while the probe holds it: the record stays still.
	let before = COM2.record_len();
	assert_eq!(COM2.write_bytes(b"queued while the probe held it\n"), 31);
	COM2.write_byte(b'!');
	COM2.drain_tx();
	COM2.drain_sync();
	assert!(COM2.read_byte().is_none(), "the kernel feeds no byte of a driver's UART to anyone");
	let _ = COM2.tx_pending();
	assert_eq!(COM2.record_len(), before, "no kernel path touched the registers while the probe held them");
	assert_eq!(COM2.stray(), 0, "and none tried");
	// THE ONE ACCESS PATH COUNTS AND REFUSES what an ordinary path would do behind the owner's back.
	let _ = COM2.stray_read_for_test(5);
	assert_eq!(COM2.stray(), 1, "an ordinary path's access while a driver holds the port is counted");
	assert_eq!(COM2.record_len(), before, "and refused - the register was not read");
	// THE RELEASE: the range revoked from the probe, the ports back in the reserved set, and the UART the kernel's.
	let from = COM2.record_len();
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free), "the release confirmed");
	assert_eq!(COM2.owner(), Owner::Kernel);
	let accesses = COM2.record_from(from);
	// DLAB CLEARED FIRST, then the receiver read out, and only then the boot sequence with its FIFO reset.
	let dlab_cleared = accesses.iter().position(|access| access.write && access.offset == LCR && access.value & 0x80 == 0).expect("the latch is closed");
	let first_read = accesses.iter().position(|access| !access.write && access.offset == RBR_THR).expect("the receiver is read");
	let fifo_reset = accesses.iter().position(|access| access.write && access.offset == FCR).expect("the FIFO is reset");
	assert!(dlab_cleared < first_read && first_read < fifo_reset, "the latch closed, then the read-out, then the FIFO reset: {accesses:?}");
	assert_eq!(COM2.take_read_out(), b"ABC", "the bytes the probe left in the receiver were read out, not reset away");
	let writes: alloc::vec::Vec<Access> = accesses.iter().copied().filter(|access| access.write && access.path == Path::Kernel).collect();
	let boot = writes.iter().position(|access| access.offset == IER && access.value == 0).expect("the boot sequence ran");
	assert!(is_boot_sequence(&writes[boot..], true), "the whole boot initialisation ran, with the receive interrupt: {writes:?}");
	// AND WHAT QUEUED MEANWHILE DRAINS NOW, in order.
	let from = COM2.record_len();
	COM2.drain_sync();
	let sent: alloc::vec::Vec<u8> = writes_since(from).iter().filter(|access| access.offset == RBR_THR).map(|access| access.value).collect();
	assert_eq!(sent, b"queued while the probe held it\r\n!", "the backlog reached the wire after the reacquisition");
	probe.release();
	probe.wait_done();
	assert_eq!(COM2.stray(), 0, "the reacquisition reported the count and started it again");
	give_up();
}

crate::tagged_test!(a_quarantined_release_gives_the_console_uart_back_and_the_row_is_not_claimed_again, [Object, Kernel, ArchX86_64], id = "kernel.object.port_range.handoff.a_quarantined_release_gives_the_console_uart_back_and_the_row_is_not_claimed_again", covers = ["kernel"]);
fn a_quarantined_release_gives_the_console_uart_back_and_the_row_is_not_claimed_again() {
	let row = adopt(false);
	let key = claim(row);
	assert!(matches!(COM2.owner(), Owner::Driver(_)));
	// A TEARDOWN THAT MISSED ITS DEADLINE: the claim latches `Quarantined`, and the late completion releases
	// nothing - but the UART is the kernel's again all the same.
	device::begin_release_for_test(key).expect("the teardown begins");
	device::expire_release_for_test(row);
	let snapshot = device::snapshot(row).expect("the row answers");
	assert_eq!(snapshot.state, device::ClaimState::Quarantined as u32, "the deadline quarantined the claim");
	assert_eq!(device::finish_release_for_test(row, true), device::ClaimState::Quarantined);
	assert_eq!(COM2.owner(), Owner::Kernel, "a quarantined release still returns the UART to the kernel");
	assert!(matches!(grants::recordable(COM2_BASE, 8), Err(Refusal::Reserved(Part::Installed))), "and its ports stay the kernel's");
	assert_eq!(device::claim(row, &crate::dma_policy::entry_field(b"synthetic-platform")), Err(device::ClaimError::Quarantined), "the row is not claimed again this boot");
	device::forget_quarantine_for_test(row);
	give_up();
}

crate::tagged_test!(the_sleep_entry_follows_the_owner_it_finds_and_a_wake_from_s3_programs_the_uart_before_its_first_line, [Object, Kernel, ArchX86_64], id = "kernel.object.port_range.handoff.the_sleep_entry_follows_the_owner_it_finds_and_a_wake_from_s3_programs_the_uart_before_its_first_line", covers = ["kernel"]);
fn the_sleep_entry_follows_the_owner_it_finds_and_a_wake_from_s3_programs_the_uart_before_its_first_line() {
	// THE KERNEL OWNS IT: the entry drains the ring to the wire and the owner stays KERNEL.
	let row = adopt(true);
	assert_eq!(COM2.write_bytes(b"before\n"), 7);
	let from = COM2.record_len();
	COM2.sleep_begin();
	assert_eq!(COM2.owner(), Owner::Kernel, "a kernel-owned UART stays the kernel's");
	assert_eq!(COM2.queued(), 0, "the ring was drained");
	let drained: alloc::vec::Vec<u8> = writes_since(from).iter().filter(|access| access.offset == RBR_THR).map(|access| access.value).collect();
	assert_eq!(drained, b"before\r\n");
	// A WAKE FROM S3: the boot initialisation - with the receive interrupt - before the resumed line's first byte.
	COM2.sleep_wake(true);
	let from = COM2.record_len();
	assert_eq!(COM2.write_bytes(b"sleep: resumed\n"), 15, "a line in the window goes to the wire synchronously");
	let writes = writes_since(from);
	assert!(is_boot_sequence(&writes, true), "the UART is programmed again first, the receive interrupt enabled: {writes:?}");
	let first_byte = writes.iter().position(|access| access.offset == RBR_THR && access.value == b's').expect("the resumed line");
	assert!(first_byte >= 8, "and only then the line");
	COM2.sleep_end();
	assert_eq!(COM2.write_bytes(b"after\n"), 6);
	assert_eq!(COM2.queued(), 7, "after the window, lines queue in the ring again");
	COM2.drain_sync();
	// A PROBE'S CLAIM HOLDS IT: the entry LENDS it to the kernel - owner SLEEP, the boot initialisation with the
	// receive interrupt left off, the backlog drained - and gives it back to the claim at its end.
	let key = claim(row);
	assert_eq!(COM2.write_bytes(b"queued for the tap\n"), 19);
	let from = COM2.record_len();
	COM2.sleep_begin();
	// THE DRIVER'S SETTINGS, read as the loan began: the first three reads are its IER, LCR and MCR.
	let read_at_loan: alloc::vec::Vec<(u16, u8)> = COM2.record_from(from).iter().filter(|access| !access.write).take(3).map(|access| (access.offset, access.value)).collect();
	assert_eq!(read_at_loan.iter().map(|(offset, _)| *offset).collect::<alloc::vec::Vec<u16>>(), [IER, LCR, MCR], "the loan reads the driver's settings before it programs anything");
	let driver_ier: u8 = read_at_loan[0].1;
	assert_eq!(COM2.owner(), Owner::Sleep(key.generation), "lent for the window");
	let writes = writes_since(from);
	assert!(is_boot_sequence(&writes, false), "programmed with the receive interrupt left off: {writes:?}");
	let sent: alloc::vec::Vec<u8> = writes.iter().skip(7).filter(|access| access.offset == RBR_THR).map(|access| access.value).collect();
	assert_eq!(sent, b"queued for the tap\r\n", "the backlog went to the wire in the window");
	assert!(COM2.record_from(from).iter().all(|access| access.path == Path::Sleep), "every access is the sleep entry's");
	COM2.sleep_wake(true);
	let from = COM2.record_len();
	assert_eq!(COM2.write_bytes(b"sleep: resumed\n"), 15);
	let writes = writes_since(from);
	assert!(is_boot_sequence(&writes, false), "the wake programs it again, the receive interrupt still off while lent: {writes:?}");
	let handed_back = COM2.record_len();
	COM2.sleep_end();
	assert_eq!(COM2.owner(), Owner::Driver(key.generation), "the claim holds it again");
	// AND WITH THE DRIVER'S SETTINGS: the interrupt enables it had are the last thing written - a UART handed back with
	// the receive interrupt off is a console that answers nothing typed after a suspend to idle.
	let last_ier = writes_since(handed_back).iter().rev().find(|access| access.offset == IER).map(|access| access.value);
	assert_eq!(last_ier, Some(driver_ier), "the driver's IER is written back last");
	let before = COM2.record_len();
	assert_eq!(COM2.write_bytes(b"for the tap\n"), 12);
	assert_eq!(COM2.record_len(), before, "and later lines queue for the tap");
	assert_eq!(COM2.stray(), 0, "nothing the window did is counted against the claim's hold");
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free));
	assert_eq!(COM2.owner(), Owner::Kernel);
	COM2.drain_sync();
	give_up();
}

crate::tagged_test!(the_terminal_writer_takes_the_uart_from_a_probe_that_broke_it_and_puts_the_backlog_and_the_drops_on_the_wire, [Object, Kernel, Process, ArchX86_64], id = "kernel.object.port_range.handoff.the_terminal_writer_takes_the_uart_from_a_probe_that_broke_it_and_puts_the_backlog_and_the_drops_on_the_wire", covers = ["kernel"]);
fn the_terminal_writer_takes_the_uart_from_a_probe_that_broke_it_and_puts_the_backlog_and_the_drops_on_the_wire() {
	if crate::smp::cpu_count() < 2 {
		crate::serial_println!("NOT RUN: a probe holding the UART on another core needs two cores");
		return;
	}
	// A PROBE BREAKS THE UART every way a driver can: the FIFO off, loopback on, the latch open and another divisor.
	let row = adopt(false);
	let key = claim(row);
	let range = console_range(key);
	let probe = &PROBES[0];
	probe.arm_script(&[(COM2_BASE + FCR, 0x00), (COM2_BASE + MCR, 0x10), (COM2_BASE + LCR, 0x80), (COM2_BASE, 0x01), (COM2_BASE + IER, 0x00)], range.clone());
	launch(0, 1);
	probe.wait_counting(1);
	assert_eq!(COM2.write_bytes(b"backlog\n"), 8);
	let from = COM2.record_len();
	COM2.terminal();
	assert_eq!(COM2.owner(), Owner::Terminal, "the terminal writer took the UART and keeps it");
	let writes = writes_since(from);
	assert!(is_boot_sequence(&writes, false), "the whole initialisation re-ran - latch closed, divisor back, FIFO on, loopback off: {writes:?}");
	let sent: alloc::vec::Vec<u8> = writes.iter().skip(7).filter(|access| access.offset == RBR_THR).map(|access| access.value).collect();
	assert_eq!(sent, b"backlog\r\n", "then the backlog");
	// ITS MARKER goes to the wire synchronously - read from the record, because loopback is off now and the wire
	// is a null device.
	let from = COM2.record_len();
	assert_eq!(COM2.write_bytes(b"*** marker ***\n"), 15);
	let marker: alloc::vec::Vec<u8> = writes_since(from).iter().filter(|access| access.offset == RBR_THR).map(|access| access.value).collect();
	assert_eq!(marker, b"*** marker ***\r\n", "the terminal path writes to the wire, not the ring");
	assert_eq!(COM2.queued(), 0);
	probe.release();
	probe.wait_done();
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free));
	assert_eq!(COM2.owner(), Owner::Terminal, "a release does not take the terminal owner's UART back");
	// A FRESH INSTANCE WHOSE HOLDER LET THE RING PASS ITS BOUND: the backlog, then the dropped count, then the
	// marker - none of it lost.
	give_up();
	let row = adopt(false);
	let key = claim(row);
	for at in 0..16384 + 57 {
		COM2.write_byte(b'a' + (at % 26) as u8);
	}
	let from = COM2.record_len();
	COM2.terminal();
	assert_eq!(COM2.write_bytes(b"*** marker ***\n"), 15);
	let tail: alloc::vec::Vec<u8> = writes_since(from).iter().filter(|access| access.offset == RBR_THR).map(|access| access.value).collect();
	let text = core::str::from_utf8(&tail).expect("the wire's bytes are text");
	assert!(text.ends_with("\r\nconsole: 57 byte(s) of kernel output were dropped at the ring's bound before this point\r\n*** marker ***\r\n"), "the drops were counted and the marker followed them: ...{}", &text[text.len().saturating_sub(160)..]);
	let backlog = &tail[..tail.len() - text.rsplit_once("\r\nconsole: ").map_or(0, |(_, rest)| rest.len() + 11)];
	assert!(backlog.len() >= 1024 && backlog.iter().rev().take(1024).all(|&byte| byte.is_ascii_lowercase()), "the backlog's last bytes came before the count");
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free));
	give_up();
}
