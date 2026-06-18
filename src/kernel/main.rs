#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

mod arch;
mod fault;
mod mem;
mod object;
mod panic;
mod product;
mod sched;
mod smp;
mod sync;
mod syscall;

use limine::request::{HhdmRequest, MemoryMapRequest, MpRequest, RequestsEndMarker, RequestsStartMarker};
use limine::BaseRevision;

// Limine boot protocol: request declarations.
// Base revision tells the bootloader which protocol revision the kernel speaks.
#[used]
#[link_section = ".limine_requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

// HHDM: Limine maps all physical memory at a fixed higher-half offset.
#[used]
#[link_section = ".limine_requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

// Physical memory map: usable regions become the frame allocator's free list.
#[used]
#[link_section = ".limine_requests"]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

// Multiprocessor: ask Limine to start the other cores (parked until we wake them).
#[used]
#[link_section = ".limine_requests"]
static MP_REQUEST: MpRequest = MpRequest::new();

// Start/end markers delimit the request block so Limine can locate it.
#[used]
#[link_section = ".limine_requests_start"]
static _REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".limine_requests_end"]
static _REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

// print macros (architecture-independent, target arch::serial::SerialWriter)
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = core::write!($crate::arch::serial::SerialWriter, $($arg)*);
    }};
}

#[macro_export]
macro_rules! serial_println {
    () => { $crate::serial_print!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = core::writeln!($crate::arch::serial::SerialWriter, $($arg)*);
    }};
}

// kernel entry point (ELF entry, see ENTRY(kmain) in the linker script)
#[no_mangle]
unsafe extern "C" fn kmain() -> ! {
	arch::serial::init();
	arch::init();
	init_memory();
	arch::init_interrupts();
	arch::init_tsc();
	arch::enable_interrupts();
	arch::init_syscalls();
	init_smp();
	sched::init();

	#[cfg(test)]
	test_main();

	#[cfg(not(test))]
	boot_main();

	arch::halt_loop()
}

// Bring up physical frames, paging and the kernel heap from the Limine
// responses. Runs before the test/boot split so `alloc` is available in tests.
fn init_memory() {
	let hhdm = HHDM_REQUEST.get_response().expect("Limine: no HHDM response");
	let memory_map = MEMORY_MAP_REQUEST.get_response().expect("Limine: no memory map response");
	mem::init(memory_map, hhdm.offset());
}

// Wake the application processors and wait for every core to report in. Runs
// before the test/boot split so SMP is up for both paths.
fn init_smp() {
	let mp = MP_REQUEST.get_response().expect("Limine: no MP response");
	smp::init(mp);
}

#[cfg(not(test))]
fn boot_main() {
	serial_println!("M0: hello from the kernel");
	serial_println!("{} {} - {}", product::NAME, product::VERSION, product::WEBSITE);
	if !BASE_REVISION.is_supported() {
		serial_println!("ERROR: Limine base revision not supported");
		return;
	}
	serial_println!("arch: x86_64 | bootloader: Limine | base revision OK");
	serial_println!("GDT + IDT installed");
	// sanity check the IDT: trigger a breakpoint exception and recover from it
	unsafe { core::arch::asm!("int3") };
	serial_println!("recovered from breakpoint exception");
	serial_println!("memory: {} physical frames free", mem::frame::free_count());
	let mut numbers = alloc::vec::Vec::new();
	for i in 0u64..16 {
		numbers.push(i);
	}
	let sum: u64 = numbers.iter().sum();
	serial_println!("heap: summed {} Vec elements, total {}", numbers.len(), sum);
	let start = arch::apic::ticks();
	while arch::apic::ticks() < start + 5 {
		core::hint::spin_loop();
	}
	serial_println!("timer: LAPIC periodic timer counted {} ticks", arch::apic::ticks());
	serial_println!("smp: {} of {} cores online", smp::online_count(), smp::cpu_count());
	scheduler_demo();
	syscall_demo();
	channel_ipc_demo();
	userspace_demo();
	userspace_fault_demo();
	domain_lifecycle_demo();
	ipc_bench();
	serial_println!("boot OK, halting");
}

// Spawn a few cooperative kernel threads on this core and run them to completion,
// demonstrating that multiple threads multiplex over one core via the scheduler.
#[cfg(not(test))]
fn scheduler_demo() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static COMPLETED: AtomicU32 = AtomicU32::new(0);
	extern "C" fn demo_thread(id: u64) {
		for step in 0..2 {
			serial_println!("kthread {}: step {}", id, step);
			sched::yield_now();
		}
		COMPLETED.fetch_add(1, Ordering::SeqCst);
	}
	for id in 0..3 {
		sched::spawn(demo_thread, id);
	}
	sched::run_until_idle();
	serial_println!("scheduler: {} kernel threads completed", COMPLETED.load(Ordering::SeqCst));
}

// Exercise the syscall ABI. The stateless calls run directly from the boot
// context; the object/handle/mapping calls need a current thread's handle table,
// so they run inside a spawned kernel thread.
#[cfg(not(test))]
fn syscall_demo() {
	let echo = unsafe { arch::syscall::invoke(syscall::SYS_DEBUG_NOOP, 0xabcd, 0, 0, 0) };
	serial_println!("syscall: debug_noop echoed {:#x}", echo);
	let ticks = unsafe { arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0) };
	serial_println!("syscall: clock_get returned {} ticks", ticks);
	sched::spawn(syscall_demo_thread, 0);
	sched::run_until_idle();
}

#[cfg(not(test))]
extern "C" fn syscall_demo_thread(_arg: u64) {
	use object::rights::Rights;
	unsafe {
		let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 8192, 0, 0, 0);
		let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
		let ptr = virt as *mut u64;
		ptr.write_volatile(0x1234_5678);
		serial_println!("syscall: mapped object at {:#x}, read back {:#x}", virt, ptr.read_volatile());
		let dup = arch::syscall::invoke(syscall::SYS_HANDLE_DUPLICATE, handle, Rights::READ.bits() as u64, 0, 0);
		arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0);
		arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0);
		arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, dup, 0, 0, 0);
	}
	serial_println!("syscall: object/handle round-trip done");
}

// Exercise IPC: two threads, each holding one end of a channel, exchange a
// message and a capability. The sender stores a marker in a memory object and
// transfers it; the receiver reads the marker back through its new handle.
#[cfg(not(test))]
fn channel_ipc_demo() {
	let (ep0, ep1) = object::channel::Channel::create();
	sched::spawn_with_object(ipc_sender, ep0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(ipc_receiver, ep1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
}

#[cfg(not(test))]
extern "C" fn ipc_sender(ch: u64) {
	unsafe {
		let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
		let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
		(virt as *mut u64).write_volatile(0xcafe_d00d);
		arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
		let payload = *b"ping";
		let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, mo);
		serial_println!("ipc: sender sent message + capability ({})", sent as i64);
	}
}

#[cfg(not(test))]
extern "C" fn ipc_receiver(ch: u64) {
	unsafe {
		let mut buf = [0u8; 16];
		let mut xfer: u64 = 0;
		// Non-blocking recv: cooperatively yield and retry until a message arrives.
		loop {
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, ch, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			if !syscall::sys_is_err(n) {
				let text = core::str::from_utf8(&buf[..n as usize]).unwrap_or("<bad>");
				let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
				let marker = (virt as *const u64).read_volatile();
				serial_println!("ipc: receiver got \"{}\" + capability, marker {:#x}", text, marker);
				arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
				arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
				break;
			}
			sched::yield_now();
		}
	}
}

// Phase-0 gate: measure IPC round-trip latency and confirm large-buffer transfer
// is zero-copy. The concept treats a fast local call as a prerequisite before
// services are layered on top of IPC, so the numbers are printed at boot.
#[cfg(not(test))]
fn ipc_bench() {
	use object::channel::{Channel, Message};
	serial_println!("ipc: TSC calibrated at {} MHz", arch::tsc::hz() / 1_000_000);

	// Raw channel primitive: a request/reply round-trip is send + recv + send +
	// recv. One pre-built message is bounced around the pair, so nothing is
	// allocated inside the timed loop - this is the IPC data-path floor (lock,
	// queue push/pop, message move), independent of the scheduler.
	let (client, server) = Channel::create();
	let mut msg = Some(Message::new(alloc::vec![0u8; 64], alloc::vec::Vec::new(), 0));
	let iters: u64 = 200_000;
	for _ in 0..1_000 {
		client.send(msg.take().unwrap()).unwrap();
		let bounced = server.recv().unwrap();
		server.send(bounced).unwrap();
		msg = Some(client.recv().unwrap());
	}
	let start = arch::tsc::now();
	for _ in 0..iters {
		client.send(msg.take().unwrap()).unwrap();
		let bounced = server.recv().unwrap();
		server.send(bounced).unwrap();
		msg = Some(client.recv().unwrap());
	}
	report_latency("ipc: raw channel round-trip", arch::tsc::now() - start, iters);

	// The same round-trip through the syscall ABI (entry, handle lookup, the IPC
	// primitive), then an explicit zero-copy buffer transfer. Both need a current
	// thread's handle table, so they run inside spawned kernel threads.
	sched::spawn(ipc_bench_syscall_thread, 0);
	sched::run_until_idle();
	sched::spawn(ipc_zero_copy_thread, 0);
	sched::run_until_idle();
}

// Print a per-round-trip latency line from a total cycle count.
#[cfg(not(test))]
fn report_latency(label: &str, total_cycles: u64, iters: u64) {
	let per_cycles = total_cycles / iters;
	let per_ns = arch::tsc::cycles_to_ns(total_cycles) / iters;
	serial_println!("{}: {} ns / {} cycles per round-trip ({} iters)", label, per_ns, per_cycles, iters);
}

// Time a request/reply round-trip driven entirely through the syscall ABI. The
// thread creates a channel pair in its own table, then loops the four calls a
// real caller would make: client send, server recv, server send, client recv.
#[cfg(not(test))]
extern "C" fn ipc_bench_syscall_thread(_arg: u64) {
	unsafe {
		let mut client: u64 = 0;
		let mut server: u64 = 0;
		let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, &mut client as *mut u64 as u64, &mut server as *mut u64 as u64, 0, 0);
		if syscall::sys_is_err(created) {
			serial_println!("ipc: syscall round-trip setup failed ({})", created as i64);
			return;
		}
		let payload = *b"reqreply";
		let pp = payload.as_ptr() as u64;
		let pl = payload.len() as u64;
		let mut buf = [0u8; 16];
		let bp = buf.as_mut_ptr() as u64;
		let bl = buf.len() as u64;
		let mut xfer: u64 = 0;
		let xp = &mut xfer as *mut u64 as u64;
		let iters: u64 = 100_000;
		for _ in 0..1_000 {
			arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, client, pp, pl, 0);
			arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, server, bp, bl, xp);
			arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, server, pp, pl, 0);
			arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, client, bp, bl, xp);
		}
		let start = arch::tsc::now();
		for _ in 0..iters {
			arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, client, pp, pl, 0);
			arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, server, bp, bl, xp);
			arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, server, pp, pl, 0);
			arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, client, bp, bl, xp);
		}
		report_latency("ipc: syscall round-trip", arch::tsc::now() - start, iters);
	}
}

// Demonstrate zero-copy: a large shared buffer is handed to the peer by moving a
// capability, never by copying its bytes through the channel. Produce a 1 MiB
// memory object, mark its far end, send the handle with a tiny note, then map it
// on the receiving endpoint and read the mark back - same physical pages, no copy.
#[cfg(not(test))]
extern "C" fn ipc_zero_copy_thread(_arg: u64) {
	const BUF_LEN: u64 = 0x10_0000; // 1 MiB
	const MARK: u64 = 0xfeed_face_c0de_d00d;
	unsafe {
		let mut client: u64 = 0;
		let mut server: u64 = 0;
		if syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, &mut client as *mut u64 as u64, &mut server as *mut u64 as u64, 0, 0)) {
			return;
		}
		let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, BUF_LEN, 0, 0, 0);
		let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
		((virt + BUF_LEN - 8) as *mut u64).write_volatile(MARK);
		arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
		let note = *b"BIG";
		arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, client, note.as_ptr() as u64, note.len() as u64, mo);
		let mut buf = [0u8; 8];
		let mut xfer: u64 = 0;
		let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, server, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
		let virt2 = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
		let marker = ((virt2 + BUF_LEN - 8) as *const u64).read_volatile();
		serial_println!("ipc: zero-copy - moved a {} KiB buffer with a {}-byte message, peer read marker {:#x} at the far end", BUF_LEN / 1024, n, marker);
		arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
		arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
	}
}

// Userspace (ring 3) page layout for the demo and test: one USER page for the
// program, one for its stack, mapped into the low half of the shared address
// space (per-process page tables / CR3 isolation are a later milestone).
const USER_CODE_VA: u64 = 0x0000_0000_4000_0000;
const USER_STACK_VA: u64 = 0x0000_0000_4001_0000;

// Kernel-thread body that runs a ring-3 program. It maps a USER code and stack
// page, copies the embedded position-independent program in, and drops to ring 3
// with its bootstrap Channel handle. The program makes a capability-gated channel
// send and a debug-write, then exits back here, where we tear the mapping down.
extern "C" fn user_thread_body(handle: u64) {
	use mem::frame::{self, PAGE_SIZE};
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags);
	let program = arch::usermode::program_bytes();
	unsafe {
		core::ptr::copy_nonoverlapping(program.as_ptr(), USER_CODE_VA as *mut u8, program.len());
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, handle);
	}
	arch::paging::unmap_page(USER_CODE_VA);
	arch::paging::unmap_page(USER_STACK_VA);
	frame::deallocate(code);
	frame::deallocate(stack);
}

// Run the first userspace program: hand it one end of a channel as its bootstrap
// capability, drop it to ring 3, and read back the message it sends from user
// mode through the kernel-held peer endpoint.
#[cfg(not(test))]
fn userspace_demo() {
	let (ep0, ep1) = object::channel::Channel::create();
	sched::spawn_with_object(user_thread_body, ep0, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	match ep1.recv() {
		Ok(message) => serial_println!("userspace: ring-3 program sent \"{}\" over a channel", core::str::from_utf8(&message.bytes).unwrap_or("<bad>")),
		Err(_) => serial_println!("userspace: ERROR - no message from ring 3"),
	}
}

// Statics the fault-probe body records into; read back by the demo and the test.
static FAULT_GOT: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
static FAULT_KIND: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static FAULT_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Kernel-thread body that drops to ring 3 running the fault-probe program. Before
// entering it opens a MemoryObject - charging its Domain's memory and a handle -
// and deliberately leaves it open, so that tearing the process down (when this
// thread is reaped) is what refunds it. The ring-3 program writes to an unmapped
// address and faults; the kernel records the fault, terminates the process, and
// longjmps back here, where we read the recorded fault and free the user mapping.
extern "C" fn user_fault_thread_body(_arg: u64) {
	use core::sync::atomic::Ordering;
	use mem::frame::{self, PAGE_SIZE};
	let _mo = unsafe { arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, PAGE_SIZE, 0, 0, 0) };
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags);
	let program = arch::usermode::program_fault_bytes();
	unsafe {
		core::ptr::copy_nonoverlapping(program.as_ptr(), USER_CODE_VA as *mut u8, program.len());
		// Drops to ring 3; the program faults and the kernel returns control here.
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, 0);
	}
	// Back from the ring-3 fault: read the fault the kernel recorded for us.
	let mut info = fault::FaultInfo { kind: 0, error_code: 0, address: 0, instruction_pointer: 0 };
	let got = unsafe { arch::syscall::invoke(syscall::SYS_FAULT_INFO_GET, &mut info as *mut fault::FaultInfo as u64, core::mem::size_of::<fault::FaultInfo>() as u64, 0, 0) };
	FAULT_GOT.store(got as i64, Ordering::SeqCst);
	FAULT_KIND.store(info.kind, Ordering::SeqCst);
	FAULT_ADDR.store(info.address, Ordering::SeqCst);
	// Tear the user mapping down. The MemoryObject handle stays open on purpose, so
	// process teardown is what frees it.
	arch::paging::unmap_page(USER_CODE_VA);
	arch::paging::unmap_page(USER_STACK_VA);
	frame::deallocate(code);
	frame::deallocate(stack);
}

// Run the fault-isolation demo: spawn a thread that drops to ring 3 and faults.
// The kernel must terminate just that process and keep running; report the fault
// it recorded to show the excursion was caught and cleaned up.
#[cfg(not(test))]
fn userspace_fault_demo() {
	use core::sync::atomic::Ordering;
	sched::spawn(user_fault_thread_body, 0);
	sched::run_until_idle();
	if FAULT_GOT.load(Ordering::SeqCst) == 1 {
		serial_println!("userspace: caught a ring-3 fault (kind {}, addr {:#x}); process terminated, kernel survived", FAULT_KIND.load(Ordering::SeqCst), FAULT_ADDR.load(Ordering::SeqCst));
	} else {
		serial_println!("userspace: ERROR - expected a ring-3 fault but none was recorded");
	}
}

// A kernel thread that holds a resource and parks until its Domain is killed. It
// opens a MemoryObject (charged to its Domain) and then yields forever; once its
// Domain is killed, it observes the kill at the next yield and exits, releasing
// the object. Shared by the domain-lifecycle demo and test.
extern "C" fn domain_parker(_arg: u64) {
	let _mo = unsafe { arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, mem::frame::PAGE_SIZE, 0, 0, 0) };
	loop {
		sched::yield_now();
	}
}

// The killer thread for the domain-lifecycle demo: it is seeded with a handle to
// a Domain and kills it (and its whole subtree) through the real syscall.
#[cfg(not(test))]
extern "C" fn domain_killer(domain_handle: u64) {
	unsafe {
		arch::syscall::invoke(syscall::SYS_DOMAIN_KILL, domain_handle, 0, 0, 0);
	}
}

// Run the domain-lifecycle demo: build a Domain subtree, run two parked processes
// under the child that each hold a MemoryObject, then kill the parent. The whole
// subtree is torn down and every resource refunded - the kernel reclaims a process
// group by killing its Domain. Report the parent's account to show it returned to
// zero.
#[cfg(not(test))]
fn domain_lifecycle_demo() {
	use object::domain::Domain;
	use object::rights::Rights;
	let parent = Domain::new(1 << 20, 16, 8);
	let child = Domain::new_child(&parent, 1 << 20, 16, 8);
	let _ = sched::spawn_in(child.clone(), domain_parker, 0);
	let _ = sched::spawn_in(child.clone(), domain_parker, 0);
	sched::spawn_with_object(domain_killer, parent.clone(), Rights::MANAGE, 0);
	sched::run_until_idle();
	serial_println!("domain: killed a subtree; parent account reclaimed (memory {}, handles {}, threads {})", parent.account().memory().used(), parent.account().handles().used(), parent.account().threads().used());
}

// test harness (custom_test_frameworks, runs under `cargo test` in QEMU)
#[cfg(test)]
trait Testable {
	fn run(&self);
}

#[cfg(test)]
impl<T: Fn()> Testable for T {
	fn run(&self) {
		serial_print!("{}...\t", core::any::type_name::<T>());
		self();
		serial_println!("[ok]");
	}
}

#[cfg(test)]
fn test_runner(tests: &[&dyn Testable]) {
	serial_println!("running {} tests", tests.len());
	for test in tests {
		test.run();
	}
	arch::exit_qemu(true);
}

#[cfg(test)]
#[test_case]
fn trivial_assertion() {
	assert_eq!(1 + 1, 2);
}

#[cfg(test)]
#[test_case]
fn breakpoint_exception_returns() {
	// reaching the next line proves the IDT breakpoint handler returned cleanly
	unsafe { core::arch::asm!("int3") };
}

#[cfg(test)]
#[test_case]
fn frame_alloc_distinct() {
	let a = mem::frame::allocate().expect("frame a");
	let b = mem::frame::allocate().expect("frame b");
	assert_ne!(a, b);
	mem::frame::deallocate(a);
	mem::frame::deallocate(b);
}

#[cfg(test)]
#[test_case]
fn paging_map_unmap() {
	let phys = mem::frame::allocate().expect("scratch frame");
	let virt: u64 = 0xffff_f000_0000_0000;
	arch::paging::map_page(virt, phys, arch::paging::WRITABLE);
	let ptr = virt as *mut u64;
	unsafe {
		ptr.write_volatile(0xdead_beef);
		assert_eq!(ptr.read_volatile(), 0xdead_beef);
	}
	let unmapped = arch::paging::unmap_page(virt).expect("was mapped");
	assert_eq!(unmapped, phys);
	mem::frame::deallocate(phys);
}

#[cfg(test)]
#[test_case]
fn heap_box_vec() {
	let boxed = alloc::boxed::Box::new(42u64);
	assert_eq!(*boxed, 42);
	let mut v = alloc::vec::Vec::new();
	for i in 0u64..1000 {
		v.push(i);
	}
	let sum: u64 = v.iter().sum();
	assert_eq!(sum, 1000 * 999 / 2);
}

#[cfg(test)]
#[test_case]
fn timer_ticks_advance() {
	// Interrupts are enabled by kmain before the tests run, so the periodic
	// LAPIC timer must keep incrementing the tick counter.
	let start = arch::apic::ticks();
	while arch::apic::ticks() == start {
		core::hint::spin_loop();
	}
	assert!(arch::apic::ticks() > start);
}

#[cfg(test)]
#[test_case]
fn handler_registration_dispatch() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static FIRED: AtomicBool = AtomicBool::new(false);
	fn handler(_vector: u8) {
		FIRED.store(true, Ordering::SeqCst);
	}
	// Register on an unused device vector and trigger it with a software
	// interrupt: proves registration and dispatch wiring without a device.
	arch::interrupts::register(47, handler);
	unsafe { core::arch::asm!("int 0x2f", options(nomem, nostack)) };
	assert!(FIRED.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn smp_all_cores_online() {
	// init_smp ran before the tests and waited for every core to report in, so
	// the online count must equal the managed core count (and exceed one when
	// QEMU is given more than a single CPU).
	assert_eq!(smp::online_count(), smp::cpu_count());
}

// A minimal kernel object used only to exercise the object/capability core.
#[cfg(test)]
struct TestObject {
	header: object::ObjectHeader,
	value: u64,
}

#[cfg(test)]
impl TestObject {
	fn new(value: u64) -> alloc::sync::Arc<Self> {
		alloc::sync::Arc::new(Self { header: object::ObjectHeader::new(), value })
	}

	fn value(&self) -> u64 {
		self.value
	}
}

#[cfg(test)]
impl object::KernelObject for TestObject {
	fn header(&self) -> &object::ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> object::ObjectType {
		object::ObjectType::Event
	}

	fn as_any(&self) -> &dyn core::any::Any {
		self
	}
}

#[cfg(test)]
#[test_case]
fn handle_create_lookup_close() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let obj = TestObject::new(42);
	let h = table.insert_object(obj, Rights::READ | Rights::WRITE, 0);
	assert_eq!(table.len(), 1);
	let looked = table.lookup(h, Rights::READ).expect("lookup");
	assert_eq!(looked.as_any().downcast_ref::<TestObject>().unwrap().value(), 42);
	table.close(h).expect("close");
	assert_eq!(table.len(), 0);
	// A closed handle must no longer resolve.
	assert!(matches!(table.lookup(h, Rights::READ), Err(HandleError::BadHandle)));
}

#[cfg(test)]
#[test_case]
fn handle_rights_enforced() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(7), Rights::READ, 0);
	assert!(table.lookup(h, Rights::READ).is_ok());
	// A right the handle does not carry is denied.
	assert!(matches!(table.lookup(h, Rights::WRITE), Err(HandleError::AccessDenied)));
}

#[cfg(test)]
#[test_case]
fn handle_duplicate_attenuates() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(1), Rights::READ | Rights::WRITE | Rights::DUPLICATE, 0);
	// A duplicate may drop rights...
	let weak = table.duplicate(h, Rights::READ).expect("duplicate");
	assert!(table.lookup(weak, Rights::READ).is_ok());
	assert!(matches!(table.lookup(weak, Rights::WRITE), Err(HandleError::AccessDenied)));
	// ...but never gain a right the original lacked.
	assert!(matches!(table.duplicate(h, Rights::EXECUTE), Err(HandleError::AccessDenied)));
	// Without the DUPLICATE right, duplication itself is denied.
	let plain = table.insert_object(TestObject::new(2), Rights::READ, 0);
	assert!(matches!(table.duplicate(plain, Rights::READ), Err(HandleError::AccessDenied)));
}

#[cfg(test)]
#[test_case]
fn handle_revocation_invalidates() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let obj = TestObject::new(99);
	let h = table.insert_object(obj.clone(), Rights::READ, 0);
	assert!(table.lookup(h, Rights::READ).is_ok());
	// Revoking the object invalidates every existing handle to it at once.
	obj.header.revoke();
	assert!(matches!(table.lookup(h, Rights::READ), Err(HandleError::Revoked)));
}

#[cfg(test)]
#[test_case]
fn handle_type_sealing() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	use object::ObjectType;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(5), Rights::READ, 0);
	assert!(table.lookup_typed(h, ObjectType::Event, Rights::READ).is_ok());
	// The same handle cannot be used where a different object type is expected.
	assert!(matches!(table.lookup_typed(h, ObjectType::Channel, Rights::READ), Err(HandleError::WrongType)));
}

#[cfg(test)]
#[test_case]
fn handle_refcount_lifetime() {
	use alloc::sync::Arc;
	use object::handle::HandleTable;
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let obj = TestObject::new(3);
	assert_eq!(Arc::strong_count(&obj), 1);
	let h = table.insert_object(obj.clone(), Rights::READ, 0);
	assert_eq!(Arc::strong_count(&obj), 2);
	let looked = table.lookup(h, Rights::READ).expect("lookup");
	assert_eq!(Arc::strong_count(&obj), 3);
	drop(looked);
	assert_eq!(Arc::strong_count(&obj), 2);
	// Closing the handle drops the kernel's last reference held via the table.
	table.close(h).expect("close");
	assert_eq!(Arc::strong_count(&obj), 1);
}

#[cfg(test)]
#[test_case]
fn thread_object_basics() {
	use object::address_space::AddressSpace;
	use object::process::Process;
	use object::thread::{Thread, ThreadState};
	use object::{KernelObject, ObjectType};
	extern "C" fn noop(_arg: u64) {}
	let process = Process::new(AddressSpace::kernel(), sched::root_domain());
	let thread = Thread::new(noop, 0, process);
	assert_eq!(thread.object_type(), ObjectType::Thread);
	assert_eq!(thread.state(), ThreadState::Ready);
	assert!(thread.tid() >= 1);
}

#[cfg(test)]
#[test_case]
fn scheduler_multiplexes_threads() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static COUNTER: AtomicU32 = AtomicU32::new(0);
	static DONE: AtomicU32 = AtomicU32::new(0);
	extern "C" fn worker(iters: u64) {
		// Yield between increments so the threads genuinely interleave rather
		// than each running to completion in one go.
		for _ in 0..iters {
			COUNTER.fetch_add(1, Ordering::SeqCst);
			sched::yield_now();
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	let threads = 4u32;
	let iters = 10u32;
	for _ in 0..threads {
		sched::spawn(worker, iters as u64);
	}
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), threads);
	assert_eq!(COUNTER.load(Ordering::SeqCst), threads * iters);
}

#[cfg(test)]
#[test_case]
fn scheduler_runs_across_cores() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static CROSS: AtomicU32 = AtomicU32::new(0);
	extern "C" fn ap_worker(_arg: u64) {
		CROSS.fetch_add(1, Ordering::SeqCst);
	}
	// Spawn one thread onto every application processor; each runs the worker in
	// its idle loop. With a single core this is a no-op and the test trivially
	// holds.
	let others = smp::cpu_count() - 1;
	for cpu in 1..smp::cpu_count() {
		sched::spawn_on(cpu, ap_worker, 0);
	}
	// Wait (bounded) for every AP to run its thread on its own core.
	let mut spins = 0u64;
	while (CROSS.load(Ordering::SeqCst) as usize) < others {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 2_000_000_000, "AP threads did not run");
	}
	assert_eq!(CROSS.load(Ordering::SeqCst) as usize, others);
}

#[cfg(test)]
#[test_case]
fn process_isolation_and_per_process_tables() {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame;
	use object::address_space::AddressSpace;
	use object::process::Process;
	use object::rights::Rights;

	// A single user virtual address that both processes map - to different frames.
	const VA: u64 = 0x0000_0000_3000_0000;
	// Each reader records the CR3 it ran on and the value it saw at VA, indexed by
	// the discriminator it is spawned with.
	static CR3: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	static SEEN: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	extern "C" fn reader(which: u64) {
		let cr3 = arch::context::read_cr3();
		let value = unsafe { (VA as *const u64).read_volatile() };
		CR3[which as usize].store(cr3, Ordering::SeqCst);
		SEEN[which as usize].store(value, Ordering::SeqCst);
	}

	// Two processes, each with its own page tables, in the root Domain.
	let p1 = Process::new(AddressSpace::create().expect("address space 1"), sched::root_domain());
	let p2 = Process::new(AddressSpace::create().expect("address space 2"), sched::root_domain());

	// Back the same VA with a distinct physical frame in each process, and stamp a
	// distinct value into each frame through the HHDM before mapping it.
	let f1 = frame::allocate().expect("frame 1");
	let f2 = frame::allocate().expect("frame 2");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	unsafe {
		((f1 + mem::hhdm_offset()) as *mut u64).write_volatile(0x1111_1111);
		((f2 + mem::hhdm_offset()) as *mut u64).write_volatile(0x2222_2222);
	}
	p1.address_space().map(VA, f1, flags);
	p2.address_space().map(VA, f2, flags);

	// Run a reader in each process and let them both finish.
	sched::thread_create(p1.clone(), reader, 0);
	sched::thread_create(p2.clone(), reader, 1);
	sched::run_until_idle();

	// Same VA, different physical frames: each reader saw only its own process's
	// memory - the address spaces are isolated.
	assert_eq!(SEEN[0].load(Ordering::SeqCst), 0x1111_1111);
	assert_eq!(SEEN[1].load(Ordering::SeqCst), 0x2222_2222);

	// The readers ran on different page-table roots, each its own process's CR3 -
	// proof the context switch reloaded CR3.
	let cr3_1 = CR3[0].load(Ordering::SeqCst);
	let cr3_2 = CR3[1].load(Ordering::SeqCst);
	assert_ne!(cr3_1, cr3_2);
	assert_eq!(cr3_1, p1.address_space().cr3());
	assert_eq!(cr3_2, p2.address_space().cr3());

	// Handle tables are per-process: a capability installed in one process is
	// invisible to the other.
	let (endpoint, _peer) = object::channel::Channel::create();
	p1.install(endpoint, Rights::ALL, 0);
	assert_eq!(p1.handles().lock().len(), 1);
	assert_eq!(p2.handles().lock().len(), 0);

	// Reclaim the data frames. Dropping the address spaces frees their page
	// tables, but these leaf frames are ours to release.
	assert_eq!(p1.address_space().unmap(VA), Some(f1));
	assert_eq!(p2.address_space().unmap(VA), Some(f2));
	frame::deallocate(f1);
	frame::deallocate(f2);
}

#[cfg(test)]
#[test_case]
fn syscall_roundtrip_stateless() {
	// Stateless syscalls round-trip from the test (idle) context: there is no
	// current thread, but these calls do not need one.
	unsafe {
		// A call returns there and back, carrying a value across the boundary.
		assert_eq!(arch::syscall::invoke(syscall::SYS_DEBUG_NOOP, 0x1234, 0, 0, 0), 0x1234);
		// An unknown syscall number is rejected with the error sentinel.
		let bad = arch::syscall::invoke(9999, 0, 0, 0, 0);
		assert_eq!(bad as i64, syscall::ERR_BAD_SYSCALL);
		assert!(syscall::sys_is_err(bad));
		// The kernel clock is monotonic across two reads.
		let first = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
		let second = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
		assert!(second >= first);
		assert!(!syscall::sys_is_err(first));
	}
}

#[cfg(test)]
#[test_case]
fn syscall_object_and_handle_ops() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The object/handle/mapping syscalls operate on the current thread's handle
	// table, so the sequence runs inside a spawned kernel thread. A failed
	// assertion here panics the thread, which fails the test run.
	extern "C" fn body(_arg: u64) {
		use object::rights::Rights;
		unsafe {
			// object create -> a handle into the caller's table
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			// address-space op: map it, then write and read back through the mapping
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt));
			let ptr = virt as *mut u64;
			ptr.write_volatile(0xfeed_face);
			assert_eq!(ptr.read_volatile(), 0xfeed_face);
			// mapping the same object twice is rejected (one mapping in M6)
			let again = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_INVALID);
			// handle op: duplicate with attenuated rights (READ only)
			let dup = arch::syscall::invoke(syscall::SYS_HANDLE_DUPLICATE, handle, Rights::READ.bits() as u64, 0, 0);
			assert!(!syscall::sys_is_err(dup));
			// the READ-only duplicate lacks MAP, so mapping through it is denied
			let dup_map = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, dup, 0, 0, 0);
			assert!(syscall::sys_is_err(dup_map));
			// unmap and close both handles
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, dup, 0, 0, 0) as i64, 0);
			// a closed handle no longer resolves
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn channel_message_and_capability_transfer() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	static OK: AtomicBool = AtomicBool::new(false);
	static MARKER: AtomicU64 = AtomicU64::new(0);
	// One thread holds each end of a channel. The sender writes a marker into a
	// memory object and transfers it alongside a byte payload; the receiver reads
	// the bytes and the marker back through the handle it is granted. A failed
	// assertion inside a thread panics it, which fails the test run.
	extern "C" fn sender(ch: u64) {
		unsafe {
			let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
			(virt as *mut u64).write_volatile(0x5151_5151);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
			let payload = *b"hi";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, mo);
			assert!(!syscall::sys_is_err(sent));
			// the transferred handle was consumed on success
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, mo, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE);
		}
	}
	extern "C" fn receiver(ch: u64) {
		unsafe {
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let mut n;
			loop {
				n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, ch, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
				if !syscall::sys_is_err(n) {
					break;
				}
				assert_eq!(n as i64, syscall::ERR_WOULD_BLOCK);
				sched::yield_now();
			}
			assert_eq!(&buf[..n as usize], b"hi");
			assert_ne!(xfer, 0);
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
			MARKER.store((virt as *const u64).read_volatile(), Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
			OK.store(true, Ordering::SeqCst);
		}
	}
	let (ep0, ep1) = object::channel::Channel::create();
	sched::spawn_with_object(sender, ep0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(receiver, ep1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(MARKER.load(Ordering::SeqCst), 0x5151_5151);
}

#[cfg(test)]
#[test_case]
fn channel_endpoint_semantics() {
	use object::channel::{Channel, ChannelError, Message};
	let (a, b) = Channel::create();
	// an empty inbox reports would-block while the peer is open
	assert!(matches!(b.recv(), Err(ChannelError::Empty)));
	// a message carries its byte payload and the sender badge to the peer
	a.send(Message::new(alloc::vec![1, 2, 3], alloc::vec::Vec::new(), 0x99)).unwrap();
	let message = b.recv().unwrap();
	assert_eq!(message.bytes, alloc::vec![1, 2, 3]);
	assert_eq!(message.badge, 0x99);
	// once the peer is dropped and the inbox drained, recv reports peer-closed
	drop(a);
	assert!(b.is_peer_closed());
	assert!(matches!(b.recv(), Err(ChannelError::PeerClosed)));
}

#[cfg(test)]
#[test_case]
fn event_timer_objects() {
	use object::event::Event;
	use object::timer::Timer;
	let event = Event::create();
	assert!(!event.is_signaled());
	event.signal();
	assert!(event.is_signaled());
	event.clear();
	assert!(!event.is_signaled());

	let timer = Timer::create();
	// not armed -> never expired
	assert!(!timer.is_expired());
	let deadline = arch::apic::ticks() + 2;
	timer.set(deadline);
	// bounded wait for the tick counter to reach the deadline
	let mut spins = 0u64;
	while !timer.is_expired() {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 2_000_000_000, "timer never expired");
	}
	assert!(timer.is_expired());
	timer.cancel();
	assert!(!timer.is_expired());
}

#[cfg(test)]
#[test_case]
fn event_timer_syscalls() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// Event and Timer driven through the syscall path need a current thread's
	// handle table, so they run inside a spawned kernel thread.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let event = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(event));
			assert_eq!(arch::syscall::invoke(syscall::SYS_EVENT_POLL, event, 0, 0, 0), 0);
			arch::syscall::invoke(syscall::SYS_EVENT_SIGNAL, event, 0, 0, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_EVENT_POLL, event, 0, 0, 0), 1);

			let timer = arch::syscall::invoke(syscall::SYS_TIMER_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(timer));
			// not armed -> not expired
			assert_eq!(arch::syscall::invoke(syscall::SYS_TIMER_POLL, timer, 0, 0, 0), 0);
			// a deadline already reached reports expired immediately
			let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_TIMER_SET, timer, now, 0, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_TIMER_POLL, timer, 0, 0, 0), 1);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn userspace_runs_and_ipcs() {
	use object::channel::Channel;
	// Hand a fresh kernel thread one end of a channel and let it drop to ring 3
	// running the embedded user program. The program makes a capability-gated
	// channel send (a syscall from userspace) and exits; the kernel reads the
	// message back through the peer endpoint it kept.
	let (ep0, ep1) = Channel::create();
	sched::spawn_with_object(user_thread_body, ep0, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = ep1.recv().expect("ring-3 program sent a message");
	assert_eq!(&message.bytes[..], b"OK");
}

#[cfg(test)]
#[test_case]
fn fault_isolation_kills_only_process() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// A ring-3 thread dereferences a bad pointer. The kernel must terminate only
	// that process - not panic - and teardown must refund every resource it held to
	// its Domain. The thread runs in a bounded Domain and is not retained here, so
	// reaping it drops the Process and runs the refunds.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_fault_thread_body, 0).expect("spawn faulting thread");
	sched::run_until_idle();
	// Reaching here means the kernel survived the ring-3 fault and resumed
	// scheduling. The fault was recorded with the expected cause and address.
	assert_eq!(FAULT_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(FAULT_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	assert_eq!(FAULT_ADDR.load(Ordering::SeqCst), arch::usermode::FAULT_PROBE_ADDR);
	// Teardown refunded the open MemoryObject (memory + handle) and the thread slot.
	assert_eq!(domain.account().memory().used(), 0, "memory refunded");
	assert_eq!(domain.account().handles().used(), 0, "handles refunded");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

#[cfg(test)]
#[test_case]
fn domain_quota_enforced_cleanly() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::Domain;
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread accounted to a bounded Domain exercises the create-boundary
	// quotas. Reaching a cap must return ERR_RESOURCE_EXHAUSTED, not crash. The
	// create syscalls charge the current thread's Domain, so the sequence runs
	// inside a spawned thread; a failed assertion panics it and fails the run.
	extern "C" fn body(_arg: u64) {
		unsafe {
			// memory: the cap is 8192 bytes = two pages. Two objects fit exactly,
			// the third is refused cleanly without allocating anything.
			let m0 = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(m0));
			let m1 = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(m1));
			let m2 = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert_eq!(m2 as i64, syscall::ERR_RESOURCE_EXHAUSTED);
			// closing the two objects refunds their memory and their handles
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, m0, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, m1, 0, 0, 0) as i64, 0);
			// handles: the cap is 4. Four events fit, the fifth is refused cleanly.
			for _ in 0..4 {
				let e = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
				assert!(!syscall::sys_is_err(e));
			}
			let over = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert_eq!(over as i64, syscall::ERR_RESOURCE_EXHAUSTED);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	// 8192 bytes of memory (two pages), 4 handles, 4 threads.
	let domain = Domain::new(8192, 4, 4);
	// Do not keep the returned Arc, so the thread is free to be reaped (and its
	// charges refunded) once it exits.
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	// Tearing the thread down returned every resource: the four still-open events
	// are refunded by the handle table's drop and the thread slot by the thread's
	// drop, so the bounded Domain is back to zero - clean refusal, no leak.
	assert_eq!(domain.account().memory().used(), 0);
	assert_eq!(domain.account().handles().used(), 0);
	assert_eq!(domain.account().threads().used(), 0);
}

#[cfg(test)]
#[test_case]
fn domain_hierarchy_limits_aggregate() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	static THIRD: AtomicI64 = AtomicI64::new(0);
	// A child Domain's charges also count against its parent, and the parent's
	// aggregate limit binds even when the child itself is unbounded. The parent
	// caps memory at two pages; the unbounded child may charge two pages but not a
	// third. Part one checks the Domain API directly (deterministic, no thread).
	let parent = Domain::new(8192, UNLIMITED, UNLIMITED);
	let child = Domain::new_child(&parent, UNLIMITED, UNLIMITED, UNLIMITED);
	assert!(child.try_charge_memory(4096));
	assert_eq!(parent.account().memory().used(), 4096, "charge propagates to the parent");
	assert!(child.try_charge_memory(4096)); // parent now full at two pages
	assert!(!child.try_charge_memory(4096), "parent aggregate binds though the child is unbounded");
	assert_eq!(child.account().memory().used(), 8192, "the refused charge was rolled back at the child");
	assert_eq!(parent.account().memory().used(), 8192, "and left the parent unchanged");
	child.uncharge_memory(8192);
	assert_eq!(parent.account().memory().used(), 0, "uncharge propagates to the parent");
	// Part two checks the same limit through the create syscall: a process in the
	// unbounded child is refused the third page because the parent caps memory at
	// two. It records the third result and exits; teardown refunds the rest.
	extern "C" fn body(_arg: u64) {
		unsafe {
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0)));
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0)));
			THIRD.store(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0) as i64, Ordering::SeqCst);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	assert!(sched::spawn_in(child.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(THIRD.load(Ordering::SeqCst), syscall::ERR_RESOURCE_EXHAUSTED, "parent limit binds through the syscall path");
	// The body exited without closing its objects; teardown refunds them.
	assert_eq!(child.account().memory().used(), 0, "child memory refunded");
	assert_eq!(parent.account().memory().used(), 0, "parent aggregate memory refunded");
}

#[cfg(test)]
#[test_case]
fn domain_kill_frees_subtree() {
	use core::sync::atomic::{AtomicI64, Ordering};
	use object::domain::Domain;
	use object::rights::Rights;
	static KILL_RET: AtomicI64 = AtomicI64::new(-100);
	// Build a Domain subtree parent -> child, run two parked processes under the
	// child that each hold a MemoryObject, then kill the PARENT through the real
	// domain_kill syscall. The whole subtree must be torn down: the parkers'
	// resources refunded and their threads reaped, leaving both Domains' accounts
	// at zero. Killing a parent thus terminates every descendant process.
	let parent = Domain::new(1 << 20, 16, 8);
	let child = Domain::new_child(&parent, 1 << 20, 16, 8);
	// The killer runs in the root Domain (so it is not itself killed); it is
	// seeded with a handle to the parent Domain and kills it.
	extern "C" fn killer(domain_handle: u64) {
		let ret = unsafe { arch::syscall::invoke(syscall::SYS_DOMAIN_KILL, domain_handle, 0, 0, 0) };
		KILL_RET.store(ret as i64, Ordering::SeqCst);
	}
	// Spawn the parkers before the killer so they run first: they create their
	// objects and park, and only then does the killer tear the subtree down.
	sched::spawn_in(child.clone(), domain_parker, 0).expect("spawn parker 0");
	sched::spawn_in(child.clone(), domain_parker, 0).expect("spawn parker 1");
	sched::spawn_with_object(killer, parent.clone(), Rights::MANAGE, 0);
	sched::run_until_idle();
	// The kill syscall succeeded and the subtree was fully reclaimed: the killed
	// processes' handles (and the memory those objects pinned) were freed eagerly,
	// and the parked threads self-terminated and were reaped.
	assert_eq!(KILL_RET.load(Ordering::SeqCst), 0, "domain_kill returned ok");
	assert_eq!(child.account().memory().used(), 0, "child memory refunded");
	assert_eq!(child.account().handles().used(), 0, "child handles refunded");
	assert_eq!(child.account().threads().used(), 0, "child threads refunded");
	assert_eq!(parent.account().memory().used(), 0, "parent aggregate memory refunded");
	assert_eq!(parent.account().handles().used(), 0, "parent aggregate handles refunded");
	assert_eq!(parent.account().threads().used(), 0, "parent aggregate threads refunded");
}

#[cfg(test)]
#[test_case]
fn ipc_round_trip_and_zero_copy() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	use object::channel::{Channel, Message};

	// Round-trip correctness: a request and a reply each deliver their exact
	// bytes through the channel primitive (the path the latency benchmark times).
	let (client, server) = Channel::create();
	client.send(Message::new(alloc::vec::Vec::from(*b"req"), alloc::vec::Vec::new(), 0)).unwrap();
	let request = server.recv().unwrap();
	assert_eq!(&request.bytes[..], b"req");
	server.send(Message::new(alloc::vec::Vec::from(*b"reply"), alloc::vec::Vec::new(), 0)).unwrap();
	let reply = client.recv().unwrap();
	assert_eq!(&reply.bytes[..], b"reply");

	// Zero-copy: a 1 MiB buffer is transferred as a capability, not copied. The
	// producer marks the far end of the buffer and sends only a 3-byte note plus
	// the handle; the consumer maps the same object and reads the mark back. That
	// the far-end mark survives while only 3 bytes crossed the channel proves the
	// pages were shared, not copied. Runs in a thread (syscalls need a handle table).
	static DONE: AtomicBool = AtomicBool::new(false);
	static MARKER: AtomicU64 = AtomicU64::new(0);
	static NOTE_LEN: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		const BUF_LEN: u64 = 0x10_0000; // 1 MiB
		const MARK: u64 = 0xa5a5_0000_5a5a_1111;
		unsafe {
			let mut client: u64 = 0;
			let mut server: u64 = 0;
			let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, &mut client as *mut u64 as u64, &mut server as *mut u64 as u64, 0, 0);
			assert!(!syscall::sys_is_err(created));
			// produce: mark the last 8 bytes of a 1 MiB object, then unmap it
			let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, BUF_LEN, 0, 0, 0);
			assert!(!syscall::sys_is_err(mo));
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt));
			((virt + BUF_LEN - 8) as *mut u64).write_volatile(MARK);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
			// transfer the capability with a tiny note instead of the buffer bytes
			let note = *b"BIG";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, client, note.as_ptr() as u64, note.len() as u64, mo);
			assert!(!syscall::sys_is_err(sent));
			// consume: receive the note + handle, map the object, read the far mark
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, server, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			assert!(!syscall::sys_is_err(n));
			NOTE_LEN.store(n as u64, Ordering::SeqCst);
			assert_ne!(xfer, 0);
			let virt2 = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt2));
			MARKER.store(((virt2 + BUF_LEN - 8) as *const u64).read_volatile(), Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	// the far-end mark came through intact, and only the 3-byte note crossed the
	// channel: the 1 MiB buffer was shared by capability, never copied.
	assert_eq!(MARKER.load(Ordering::SeqCst), 0xa5a5_0000_5a5a_1111);
	assert_eq!(NOTE_LEN.load(Ordering::SeqCst), 3);
}
