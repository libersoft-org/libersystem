#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

mod arch;
mod cli;
mod console;
mod console_input;
mod device;
mod elf;
mod fault;
mod graph;
mod loader;
mod mem;
mod memlayout;
mod object;
mod panic;
mod pkg;
mod product;
mod sched;
mod smp;
mod sync;
mod syscall;

use limine::BaseRevision;
use limine::request::{FramebufferRequest, HhdmRequest, MemoryMapRequest, ModuleRequest, MpRequest, RequestsEndMarker, RequestsStartMarker};

// Limine boot protocol: request declarations.
// Base revision tells the bootloader which protocol revision the kernel speaks.
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

// HHDM: Limine maps all physical memory at a fixed higher-half offset.
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

// Physical memory map: usable regions become the frame allocator's free list.
#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

// Multiprocessor: ask Limine to start the other cores (parked until we wake them).
#[used]
#[unsafe(link_section = ".limine_requests")]
static MP_REQUEST: MpRequest = MpRequest::new();

// Init package: a Limine module (boot/init.pkg) holding the first userspace
// programs - SystemManager for now - which the kernel ELF-loads and runs.
#[used]
#[unsafe(link_section = ".limine_requests")]
static MODULE_REQUEST: ModuleRequest = ModuleRequest::new();

// Framebuffer: a linear RGB video mode for the on-screen console (M15).
#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

// Start/end markers delimit the request block so Limine can locate it.
#[used]
#[unsafe(link_section = ".limine_requests_start")]
static _REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static _REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

// print macros (architecture-independent, target arch::serial::SerialWriter)
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::_print(core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! serial_println {
    () => {
        $crate::_print(core::format_args!("\n"))
    };
    ($($arg:tt)*) => {{
        $crate::_print(core::format_args!($($arg)*));
        $crate::_print(core::format_args!("\n"));
    }};
}

// Write formatted output to the serial port (always) and mirror it to the
// framebuffer console (if one is initialized). Backs serial_print!/serial_println!
// so every log line reaches both sinks. Hidden from docs; call via the macros.
#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments<'_>) {
	use core::fmt::Write as _;
	let _ = core::write!(arch::serial::SerialWriter, "{}", args);
	console::write_fmt(args);
}

// kernel entry point (ELF entry, see ENTRY(kmain) in the linker script)
#[unsafe(no_mangle)]
unsafe extern "C" fn kmain() -> ! {
	arch::serial::init();
	arch::init();
	init_framebuffer();
	init_memory();
	arch::init_interrupts();
	arch::init_tsc();
	arch::enable_interrupts();
	arch::init_syscalls();
	init_smp();
	sched::init();
	device::init();

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

// Bring up the framebuffer console from the Limine framebuffer response, so the
// kernel log is mirrored to the screen alongside serial. A no-op (serial only) if
// the bootloader provided no framebuffer. Runs before the test/boot split so the
// console is up for both paths; it needs no heap or paging (Limine maps the
// framebuffer and the font is static), only that GDT/IDT are installed first.
fn init_framebuffer() {
	let Some(response) = FRAMEBUFFER_REQUEST.get_response() else {
		return;
	};
	let Some(fb) = response.framebuffers().next() else {
		return;
	};
	console::init(console::FbInfo { addr: fb.addr(), width: fb.width() as usize, height: fb.height() as usize, pitch: fb.pitch() as usize, bytes_per_pixel: fb.bpp() as usize / 8, red_shift: fb.red_mask_shift(), red_size: fb.red_mask_size(), green_shift: fb.green_mask_shift(), green_size: fb.green_mask_size(), blue_shift: fb.blue_mask_shift(), blue_size: fb.blue_mask_size() });
}

// Wake the application processors and wait for every core to report in. Runs
// before the test/boot split so SMP is up for both paths.
fn init_smp() {
	let mp = MP_REQUEST.get_response().expect("Limine: no MP response");
	smp::init(mp);
}

#[cfg(not(test))]
fn boot_main() {
	let title = alloc::format!("{} {}", product::NAME, product::VERSION);
	// Three labelled URLs with their values aligned on a common column.
	let label_web = "Web:";
	let label_github = "GitHub:";
	let label_vendor = alloc::format!("by {}:", product::VENDOR);
	let label_w = label_web.len().max(label_github.len()).max(label_vendor.len());
	let web = alloc::format!("{:<w$} {}", label_web, product::WEBSITE, w = label_w);
	let github = alloc::format!("{:<w$} {}", label_github, product::GITHUB, w = label_w);
	let vendor = alloc::format!("{:<w$} {}", label_vendor, product::VENDOR_URL, w = label_w);
	print_banner(&[title.as_str(), "", web.as_str(), github.as_str(), vendor.as_str()]);
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
	storage_demo();
	pci_demo();
	ipc_bench();
	cli::demo();
	serial_println!("boot OK - entering the userspace shell (type 'help', or 'exit' to halt)");
	boot_userspace_with_recovery();
	serial_println!("halting");
}

// Drive the interactive userspace shell. The boot chain has already started it as
// its last component and the shell has registered a console channel; this pumps
// serial keystrokes to it a byte at a time, running the cooperative schedule after
// each so the shell (and any service it calls) makes progress. Returns when the
// shell exits (the user typed `exit`) or never attached.
#[cfg(not(test))]
fn console_shell_loop() {
	if !console_input::shell_listening() {
		serial_println!("shell: no interactive shell attached");
		return;
	}
	// Nudge the shell to print its first prompt, then forward input until it exits.
	console_input::feed(b'\n');
	sched::run_until_idle();
	while console_input::shell_listening() {
		let byte = arch::serial::read_byte_blocking();
		if !console_input::feed(byte) {
			break;
		}
		sched::run_until_idle();
	}
}

// Print a product banner inside an ASCII frame (plain +/-/| so it renders on both
// the serial port and the framebuffer font, which carries only basic latin). The
// frame is sized to the longest line; each line is left-aligned and padded.
#[cfg(not(test))]
fn print_banner(lines: &[&str]) {
	let mut width = 0;
	for line in lines {
		if line.len() > width {
			width = line.len();
		}
	}
	let mut border = alloc::string::String::from("+");
	for _ in 0..width + 2 {
		border.push('-');
	}
	border.push('+');
	serial_println!("{}", border);
	for line in lines {
		serial_println!("| {:<width$} |", *line, width = width);
	}
	serial_println!("{}", border);
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
use crate::memlayout::{USER_CODE_VA, USER_STACK_VA};

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

// The init package bytes, located among the Limine modules by filename. Returns
// None if the bootloader passed no module whose path ends in "init.pkg".
fn init_package_bytes() -> Option<&'static [u8]> {
	let response = MODULE_REQUEST.get_response()?;
	for module in response.modules() {
		if module.path().to_bytes().ends_with(product::INIT_PACKAGE.as_bytes()) {
			// The module memory is mapped in the HHDM and is 'static for the kernel.
			let bytes = unsafe { core::slice::from_raw_parts(module.addr(), module.size() as usize) };
			return Some(bytes);
		}
	}
	None
}

// The ramdisk volume package bytes, located among the Limine modules by filename.
// Returns None if the bootloader passed no module whose path ends in "volume.pkg".
fn volume_package_bytes() -> Option<&'static [u8]> {
	let response = MODULE_REQUEST.get_response()?;
	for module in response.modules() {
		if module.path().to_bytes().ends_with(product::VOLUME_PACKAGE.as_bytes()) {
			// The module memory is mapped in the HHDM and is 'static for the kernel.
			let bytes = unsafe { core::slice::from_raw_parts(module.addr(), module.size() as usize) };
			return Some(bytes);
		}
	}
	None
}

// Load SystemManager from the init package into a new ring-3 process, handing it
// one end of a fresh channel as its bootstrap capability and, over that channel,
// the init package itself as a shared buffer so it can spawn the services it
// supervises. Returns the kernel-held peer endpoint (on which the boot chain's
// reports arrive) and the SystemManager process's koid (which the recovery
// supervisor watches for a fault). Shared by the boot path and the test.
fn spawn_system_manager() -> Result<(alloc::sync::Arc<object::channel::Channel>, u64), &'static str> {
	use alloc::sync::Arc;
	use object::KernelObject;
	use object::channel::Message;
	use object::handle::Capability;
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	let bytes = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(bytes).ok_or("init package is malformed")?;
	let elf_image = package.lookup(b"system_manager").ok_or("system_manager missing from init package")?;
	let (kernel_ep, user_ep) = object::channel::Channel::create();
	let process = loader::spawn_elf_process(sched::root_domain(), elf_image, user_ep, Rights::ALL, 0).map_err(|_| "failed to load SystemManager")?;
	let sm_koid = process.header().koid();

	// Hand SystemManager the init package as a read-only shared buffer: the kernel
	// copies the package bytes into a MemoryObject and sends "PACKAGE" + length
	// with that capability, so SystemManager can find and spawn ServiceManager and
	// then delegate the package onward to it (TRANSFER) to start the rest. DUPLICATE
	// lets ServiceManager share it further (with DeviceManager, which spawns drivers
	// from it) without giving up its own handle.
	let package_obj = MemoryObject::create(bytes.len()).ok_or("no memory for the init package")?;
	copy_into_object(&package_obj, bytes);
	let mut msg = alloc::vec::Vec::with_capacity(7 + 8);
	msg.extend_from_slice(b"PACKAGE");
	msg.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
	let cap = Capability::new(package_obj as Arc<dyn KernelObject>, Rights::READ | Rights::MAP | Rights::TRANSFER | Rights::DUPLICATE, 0);
	kernel_ep.send(Message::new(msg, alloc::vec![cap], 0)).map_err(|_| "failed to hand SystemManager the init package")?;

	// Hand SystemManager the ramdisk volume the same way, so it can be delegated
	// down to the StorageService the boot chain brings up. "RAMDISK" + length with a
	// read-only buffer capability the StorageService will map and serve files from.
	let volume = volume_package_bytes().ok_or("volume package module not found")?;
	let ramdisk = MemoryObject::create(volume.len()).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);
	let mut rdmsg = alloc::vec::Vec::with_capacity(7 + 8);
	rdmsg.extend_from_slice(b"RAMDISK");
	rdmsg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	let rdcap = Capability::new(ramdisk as Arc<dyn KernelObject>, Rights::READ | Rights::MAP | Rights::TRANSFER, 0);
	kernel_ep.send(Message::new(rdmsg, alloc::vec![rdcap], 0)).map_err(|_| "failed to hand SystemManager the ramdisk")?;
	Ok((kernel_ep, sm_koid))
}

// Drain the crash-notify channel and report whether the process `koid` faulted.
// Each record fault::notify_crash sends is [koid u64 LE][kind u64 LE].
fn crash_seen(crash_rx: &object::channel::Channel, koid: u64) -> bool {
	let mut found = false;
	while let Ok(message) = crash_rx.recv() {
		if message.bytes.len() >= 8 {
			let crashed = u64::from_le_bytes([message.bytes[0], message.bytes[1], message.bytes[2], message.bytes[3], message.bytes[4], message.bytes[5], message.bytes[6], message.bytes[7]]);
			if crashed == koid {
				found = true;
			}
		}
	}
	found
}

// Supervise a critical process (SystemManager) through the recovery ladder: each
// round, `spawn` it (returning its process koid, or 0 if it could not be spawned),
// run the system to a quiescent point, and check the crash channel. Returns true
// as soon as a round completes without the process faulting (the system is up), or
// false once every attempt - the original plus `max_restarts` recovery restarts -
// has faulted, at which point the caller escalates (reboot as the last resort).
// This is the kernel's one minimal rescue mechanism, the single exception to
// "the kernel is pure mechanism".
fn supervise(crash_rx: &object::channel::Channel, max_restarts: u32, mut spawn: impl FnMut() -> u64) -> bool {
	for attempt in 0..=max_restarts {
		let koid = spawn();
		sched::run_until_idle();
		if koid == 0 || !crash_seen(crash_rx, koid) {
			return true;
		}
		serial_println!("recovery: SystemManager (koid {}) faulted - starting a recovery SystemManager (attempt {} of {})", koid, attempt + 1, max_restarts + 1);
	}
	false
}

// Bring up the userspace system under SystemManager-crash recovery, then hand
// control to the interactive shell. The kernel registers a crash-notify channel
// and supervises SystemManager: if it faults before the system is up, the kernel
// starts a recovery SystemManager, up to a few times, and reboots as the last
// resort. On a clean start it prints the boot-chain reports and runs the shell.
#[cfg(not(test))]
fn boot_userspace_with_recovery() {
	use alloc::sync::Arc;
	const MAX_RESTARTS: u32 = 3;
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let mut kernel_ep: Option<Arc<object::channel::Channel>> = None;
	let up = supervise(&crash_rx, MAX_RESTARTS, || match spawn_system_manager() {
		Ok((ep, koid)) => {
			kernel_ep = Some(ep);
			koid
		}
		Err(reason) => {
			serial_println!("recovery: could not start SystemManager: {}", reason);
			0
		}
	});
	if up {
		// SystemManager came up without faulting: print the boot-chain reports and
		// hand control to the interactive shell it started.
		if let Some(ep) = &kernel_ep {
			while let Ok(message) = ep.recv() {
				serial_println!("userspace: {}", core::str::from_utf8(&message.bytes).unwrap_or("<bad>"));
			}
		}
		console_shell_loop();
		fault::clear_crash_notify();
	} else {
		fault::clear_crash_notify();
		serial_println!("recovery: SystemManager could not be stabilized after {} attempts - rebooting", MAX_RESTARTS + 1);
		arch::reset();
	}
}

// Fill a MemoryObject's frames with `data` (the tail of the last page is left as
// allocated) by writing through the HHDM. The object is not mapped into any
// address space here, so its physical frames are reached directly.
fn copy_into_object(object: &alloc::sync::Arc<object::memory_object::MemoryObject>, data: &[u8]) {
	let hhdm = mem::hhdm_offset();
	let page = mem::frame::PAGE_SIZE as usize;
	for (i, &phys) in object.frames().iter().enumerate() {
		let start = i * page;
		if start >= data.len() {
			break;
		}
		let end = core::cmp::min(start + page, data.len());
		let chunk = &data[start..end];
		unsafe {
			core::ptr::copy_nonoverlapping(chunk.as_ptr(), (hhdm + phys) as *mut u8, chunk.len());
		}
	}
}

// Build the M16 storage topology and run it to completion. A MemoryObject holds
// the ramdisk volume; the StorageService process maps it and serves files over a
// service channel; a client process opens vol://system/hello.txt through the
// service, receives a shared-buffer capability to the file's bytes, maps it, and
// reports the contents back over its bootstrap channel. The kernel only brokers
// the initial capabilities - the open, the resolve, and the zero-copy read all
// happen in userspace. Returns (expected, actual): the file straight from the
// volume archive, and the bytes the client read through the service. Shared by
// the boot demo and the test.
fn run_storage_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use alloc::sync::Arc;
	use object::KernelObject;
	use object::channel::{Channel, Message};
	use object::handle::Capability;
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	// the volume archive backing the ramdisk, and the file we expect served
	let volume = volume_package_bytes().ok_or("volume package module not found")?;
	let expected = pkg::Package::parse(volume).and_then(|p| p.lookup(b"hello.txt").map(|b| b.to_vec())).ok_or("hello.txt missing from the volume package")?;

	// the userspace programs, from the init package
	let init = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(init).ok_or("init package is malformed")?;
	let service_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let client_elf = package.lookup(b"storage_client").ok_or("storage_client missing from the init package")?;

	// the ramdisk: a MemoryObject filled with the volume archive via the HHDM
	let ramdisk = MemoryObject::create(volume.len()).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);

	// channels: a bootstrap per process, plus the service<->client request channel
	let (service_boot_kernel, service_boot_user) = Channel::create();
	let (client_boot_kernel, client_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	// spawn the two processes with their bootstrap endpoints
	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), service_elf, service_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain, client_elf, client_boot_user, Rights::ALL, 0).map_err(|_| "failed to load the storage client")?;

	// hand the service its ramdisk (with the volume length) and its service
	// endpoint, then hand the client the other end of that service channel. These
	// are object-level sends: the kernel attaches the capabilities directly.
	let mut ramdisk_msg = alloc::vec::Vec::with_capacity(7 + 8);
	ramdisk_msg.extend_from_slice(b"RAMDISK");
	ramdisk_msg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	let ramdisk_cap = Capability::new(ramdisk as Arc<dyn KernelObject>, Rights::READ | Rights::MAP, 0);
	service_boot_kernel.send(Message::new(ramdisk_msg, alloc::vec![ramdisk_cap], 0)).map_err(|_| "service ramdisk bootstrap failed")?;
	let service_server_cap = Capability::new(service_server as Arc<dyn KernelObject>, Rights::ALL, 0);
	service_boot_kernel.send(Message::new(b"SERVE".to_vec(), alloc::vec![service_server_cap], 0)).map_err(|_| "service serve bootstrap failed")?;
	let service_client_cap = Capability::new(service_client as Arc<dyn KernelObject>, Rights::ALL, 0);
	client_boot_kernel.send(Message::new(b"CONNECT".to_vec(), alloc::vec![service_client_cap], 0)).map_err(|_| "client connect bootstrap failed")?;

	// run the cooperative schedule until everyone is done, then read the result
	sched::run_until_idle();
	let result = client_boot_kernel.recv().map_err(|_| "the client reported no result")?;
	Ok((expected, result.bytes))
}

// Run the M16 storage scenario and report whether the client read the file's
// bytes through the StorageService intact.
#[cfg(not(test))]
fn storage_demo() {
	match run_storage_scenario() {
		Ok((expected, actual)) => {
			if actual == expected {
				serial_println!("storage: client read \"{}\" from vol://system/hello.txt via StorageService", core::str::from_utf8(&actual).unwrap_or("<bad>").trim_end());
			} else {
				serial_println!("storage: ERROR - client read {} bytes, expected {}", actual.len(), expected.len());
			}
		}
		Err(reason) => serial_println!("storage: ERROR - {}", reason),
	}
}

// Scan the PCI bus and report the devices found, flagging the virtio devices the
// driver milestones will drive. DeviceManager will later do the same enumeration
// over a syscall and hand each driver its device's capabilities.
#[cfg(not(test))]
fn pci_demo() {
	let devices = arch::pci::scan();
	serial_println!("pci: {} device(s) on bus 0", devices.len());
	for d in &devices {
		match d.virtio_type() {
			Some(t) => serial_println!("pci: {:02x}:{:02x}.{} virtio-{} (id {:#06x}) bar0 {:#010x}", d.bus, d.dev, d.func, arch::pci::virtio_type_name(t), d.device_id, d.bars[0]),
			None => serial_println!("pci: {:02x}:{:02x}.{} vendor {:#06x} device {:#06x} class {:#04x}.{:#04x}", d.bus, d.dev, d.func, d.vendor, d.device_id, d.class, d.subclass),
		}
	}
	for v in arch::pci::scan_virtio() {
		serial_println!("virtio-{}: bar{} phys {:#x} len {:#x} common@{:#x} notify@{:#x}(x{}) isr@{:#x} device@{:#x}", arch::pci::virtio_type_name(v.virtio_type), v.bar, v.bar_phys, v.region_len, v.common.offset, v.notify.offset, v.notify.notify_multiplier, v.isr.offset, v.device.offset);
	}
}

// Read a file from a vol:// volume by driving the StorageService as the kernel's
// own client - the path the CLI's `cat` command uses. Spawns the service, hands
// it the ramdisk and a service channel, sends one open request plus an empty quit
// sentinel (so the service exits and the cooperative schedule drains), runs the
// schedule to completion, then receives the reply and reads the returned shared
// buffer through the HHDM. Returns the file's bytes, or an error string.
fn storage_read(uri: &[u8]) -> Result<alloc::vec::Vec<u8>, &'static str> {
	use alloc::sync::Arc;
	use object::KernelObject;
	use object::channel::{Channel, Message};
	use object::handle::Capability;
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	let volume = volume_package_bytes().ok_or("volume package module not found")?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(init).ok_or("init package is malformed")?;
	let service_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;

	// the ramdisk: a MemoryObject filled with the volume archive via the HHDM
	let ramdisk = MemoryObject::create(volume.len()).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);

	let (service_boot_kernel, service_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	loader::spawn_elf_process(sched::root_domain(), service_elf, service_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;

	// bootstrap the service: the ramdisk (with its length) and the service endpoint
	let mut ramdisk_msg = alloc::vec::Vec::with_capacity(7 + 8);
	ramdisk_msg.extend_from_slice(b"RAMDISK");
	ramdisk_msg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	let ramdisk_cap = Capability::new(ramdisk as Arc<dyn KernelObject>, Rights::READ | Rights::MAP, 0);
	service_boot_kernel.send(Message::new(ramdisk_msg, alloc::vec![ramdisk_cap], 0)).map_err(|_| "service ramdisk bootstrap failed")?;
	let service_server_cap = Capability::new(service_server as Arc<dyn KernelObject>, Rights::ALL, 0);
	service_boot_kernel.send(Message::new(b"SERVE".to_vec(), alloc::vec![service_server_cap], 0)).map_err(|_| "service serve bootstrap failed")?;

	// the open request - [rights u32 LE][vol:// URI] - then an empty quit sentinel,
	// which the service treats as end-of-session and exits on
	let want_rights = (Rights::READ | Rights::MAP).bits();
	let mut request = alloc::vec::Vec::with_capacity(4 + uri.len());
	request.extend_from_slice(&want_rights.to_le_bytes());
	request.extend_from_slice(uri);
	service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).map_err(|_| "open request failed")?;
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).map_err(|_| "quit sentinel failed")?;

	sched::run_until_idle();

	let reply = service_client.recv().map_err(|_| "the service sent no reply")?;
	if reply.bytes.len() < 12 {
		return Err("malformed reply");
	}
	let status = u32::from_le_bytes([reply.bytes[0], reply.bytes[1], reply.bytes[2], reply.bytes[3]]);
	let size = u64::from_le_bytes([reply.bytes[4], reply.bytes[5], reply.bytes[6], reply.bytes[7], reply.bytes[8], reply.bytes[9], reply.bytes[10], reply.bytes[11]]) as usize;
	if status != 0 {
		return Err("the service denied or could not find the file");
	}
	let cap = reply.caps.first().ok_or("the service granted no buffer")?;
	let object = cap.object();
	let memory = object.as_any().downcast_ref::<MemoryObject>().ok_or("the granted capability was not a buffer")?;
	Ok(read_from_object(memory, size))
}

// Read `len` bytes out of a MemoryObject's frames through the HHDM (the reverse of
// copy_into_object). The object need not be mapped: its physical frames are read
// directly.
fn read_from_object(object: &object::memory_object::MemoryObject, len: usize) -> alloc::vec::Vec<u8> {
	let hhdm = mem::hhdm_offset();
	let page = mem::frame::PAGE_SIZE as usize;
	let mut out = alloc::vec::Vec::with_capacity(len);
	for (i, &phys) in object.frames().iter().enumerate() {
		let start = i * page;
		if start >= len {
			break;
		}
		let end = core::cmp::min(start + page, len);
		let chunk = unsafe { core::slice::from_raw_parts((hhdm + phys) as *const u8, end - start) };
		out.extend_from_slice(chunk);
	}
	out
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

// A bindable test IRQ vector (33..47, distinct from the interrupt_bind test's) a
// crashing "driver" holds before it faults.
#[cfg(test)]
const DRIVER_IRQ_VECTOR: u64 = 0x2d;

// Kernel-thread body for the driver-crash test: it acquires real driver resources
// - a bound IRQ and a DMA buffer - then drops to ring 3 and faults, leaving both
// open so the kernel's crash cleanup is what detaches the IRQ and refunds the DMA.
// Mirrors user_fault_thread_body's ring-3 fault, plus the held driver resources.
#[cfg(test)]
extern "C" fn driver_crash_thread_body(_arg: u64) {
	use mem::frame::{self, PAGE_SIZE};
	unsafe {
		let irq = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, DRIVER_IRQ_VECTOR, 0, 0, 0);
		assert!((irq as i64) > 0, "driver should bind its IRQ");
		let dma = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, PAGE_SIZE, 0, 0, 0);
		assert!((dma as i64) > 0, "driver should create its DMA buffer");
	}
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags);
	let program = arch::usermode::program_fault_bytes();
	unsafe {
		core::ptr::copy_nonoverlapping(program.as_ptr(), USER_CODE_VA as *mut u8, program.len());
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, 0);
	}
	// Back from the crash: drop the raw code/stack mappings. The IRQ and DMA handles
	// stay open, so the kernel's process teardown is what releases them.
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

	fn into_any_arc(self: alloc::sync::Arc<Self>) -> alloc::sync::Arc<dyn core::any::Any + Send + Sync> {
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
	use object::ObjectType;
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
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
fn preemption_preempts_a_cpu_bound_thread() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static STOP: AtomicBool = AtomicBool::new(false);
	static MATE_RAN: AtomicBool = AtomicBool::new(false);
	// A CPU-bound thread that NEVER yields: it spins until another thread sets STOP.
	// Only timer-driven preemption can let that other thread run, so without
	// preemption this spins forever and hangs the test.
	extern "C" fn hog(_arg: u64) {
		while !STOP.load(Ordering::SeqCst) {
			core::hint::spin_loop();
		}
	}
	// The cohabiting thread: records that it ran, then releases the hog so the run
	// queue can drain.
	extern "C" fn mate(_arg: u64) {
		MATE_RAN.store(true, Ordering::SeqCst);
		STOP.store(true, Ordering::SeqCst);
	}
	STOP.store(false, Ordering::SeqCst);
	MATE_RAN.store(false, Ordering::SeqCst);
	// Both land on this core's run queue; the hog runs first and never yields.
	sched::spawn(hog, 0);
	sched::spawn(mate, 0);
	sched::run_until_idle();
	assert!(MATE_RAN.load(Ordering::SeqCst), "the cohabiting thread never ran: the never-yielding thread was not preempted");
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
fn device_memory_maps_mmio_region() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;
	const MARK: u64 = 0xfeed_face_dead_beef;
	static DONE: AtomicBool = AtomicBool::new(false);
	// A driver maps a DeviceMemory capability (a physical MMIO region) into its
	// address space and reads/writes through the mapping. A freshly allocated RAM
	// frame is a controllable stand-in for device registers; only the uncacheable
	// mapping is exercised (no concurrent cached access to the same frame).
	extern "C" fn body(device_handle: u64) {
		unsafe {
			let va = arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
			assert!(!syscall::sys_is_err(va), "device memory did not map");
			let ptr = va as *mut u64;
			ptr.write_volatile(MARK);
			assert_eq!(ptr.read_volatile(), MARK, "the mapped MMIO region is not read/write");
			// A second map of the same region is rejected (one mapping per object).
			let again = arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_INVALID);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let phys = mem::frame::allocate().expect("a frame for the stand-in MMIO region");
	let device = DeviceMemory::new(phys, mem::frame::PAGE_SIZE as usize);
	// Hand the capability to the driver thread as its bootstrap handle.
	sched::spawn_with_object(body, device, Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "device-memory mapping thread did not finish");
	// The thread (and its handle table) is reaped by run_until_idle, dropping the
	// DeviceMemory and tearing its mapping down, so the frame is free to reclaim.
	mem::frame::deallocate(phys);
}

#[cfg(test)]
#[test_case]
fn random_get_fills_distinct_bytes() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut a = [0u8; 32];
			let mut b = [0u8; 32];
			let na = arch::syscall::invoke(syscall::SYS_RANDOM_GET, a.as_mut_ptr() as u64, a.len() as u64, 0, 0);
			let nb = arch::syscall::invoke(syscall::SYS_RANDOM_GET, b.as_mut_ptr() as u64, b.len() as u64, 0, 0);
			assert_eq!(na as usize, a.len(), "random_get did not fill the whole buffer");
			assert_eq!(nb as usize, b.len());
			// The buffer was actually written, and two draws differ (a false failure
			// is a 1-in-2^256 event).
			assert_ne!(a, [0u8; 32], "random_get left the buffer zeroed");
			assert_ne!(a, b, "two random draws were identical");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn interrupt_bind_delivers_to_driver() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// Vector 0x2c (IRQ 12) is a bindable device-IRQ vector (not the timer at 0x20).
	const VECTOR: u64 = 0x2c;
	extern "C" fn body(_arg: u64) {
		unsafe {
			let h = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, VECTOR, 0, 0, 0);
			assert!(!syscall::sys_is_err(h), "interrupt_bind failed");
			// Simulate the device IRQ firing with a software interrupt; the dispatch
			// path marks the bound Interrupt pending and wakes any waiter.
			core::arch::asm!("int 0x2c");
			// The interrupt is now pending, so a wait observes it and returns.
			let r = arch::syscall::invoke(syscall::SYS_WAIT, h, 0, 0, 0);
			assert_eq!(r as i64, 0, "wait did not observe the delivered interrupt");
			// Binding the same vector again while ours lives is refused.
			let again = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, VECTOR, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_RESOURCE_EXHAUSTED);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn object_property_set_names_an_object() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::KernelObject;
	use object::event::Event;
	use object::rights::Rights;
	static DONE: AtomicBool = AtomicBool::new(false);
	const NAME: &[u8] = b"irq-driver";
	extern "C" fn body(handle: u64) {
		unsafe {
			let r = arch::syscall::invoke(syscall::SYS_OBJECT_PROPERTY_SET, handle, syscall::PROP_NAME, NAME.as_ptr() as u64, NAME.len() as u64);
			assert_eq!(r as i64, 0, "set name failed");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let event = Event::create();
	// The driver thread holds a handle to this same Event; the test keeps an Arc to
	// read the label back after the thread names it.
	sched::spawn_with_object(body, event.clone(), Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(event.header().name().as_deref(), Some("irq-driver"));
}

#[cfg(test)]
#[test_case]
fn object_property_set_bounds_a_domain() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::{Domain, UNLIMITED};
	use object::rights::Rights;
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(handle: u64) {
		unsafe {
			// Set the Domain's memory limit to 8192 bytes via the property syscall.
			let r = arch::syscall::invoke(syscall::SYS_OBJECT_PROPERTY_SET, handle, syscall::PROP_MEMORY_LIMIT, 8192, 0);
			assert_eq!(r as i64, 0, "set memory limit failed");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	sched::spawn_with_object(body, domain.clone(), Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(domain.account().memory().limit(), 8192);
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
fn blocking_wait_wakes_on_message() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static OK: AtomicBool = AtomicBool::new(false);
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	// The server blocks in SYS_WAIT on its (empty) channel - descheduled, not
	// spinning. The client then sends, which wakes the server; it returns from
	// wait with success and recv's the message. Exercises block_on + wake_object +
	// the reschedule Block path end to end.
	extern "C" fn server(ch: u64) {
		unsafe {
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, ch, 0, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, ch, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			assert!(!syscall::sys_is_err(n));
			assert_eq!(&buf[..n as usize], b"ping");
			OK.store(true, Ordering::SeqCst);
		}
	}
	extern "C" fn client(ch: u64) {
		unsafe {
			let payload = *b"ping";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, 0);
			assert!(!syscall::sys_is_err(sent));
		}
	}
	let (ep0, ep1) = object::channel::Channel::create();
	// Spawn the server first so it runs and blocks before the client sends.
	sched::spawn_with_object(server, ep0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(client, ep1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 0);
}

#[cfg(test)]
#[test_case]
fn blocking_wait_times_out_on_deadline() {
	use core::sync::atomic::{AtomicI64, Ordering};
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	// A thread waits on an event that is never signaled, with a short absolute
	// deadline. The wait must wake itself when the deadline passes and report
	// ERR_TIMED_OUT - the timed-wait path (the scheduler's deadline check).
	extern "C" fn waiter(_arg: u64) {
		unsafe {
			let ev = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
			let deadline = now + 3; // ~30 ms at the 100 Hz tick
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, ev, deadline, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
		}
	}
	sched::spawn(waiter, 0);
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), syscall::ERR_TIMED_OUT);
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
fn pci_scan_finds_virtio_devices() {
	// QEMU is launched (see qemu-run.sh) with virtio-blk, virtio-net, and a virtio
	// serial device on the PCI bus. The kernel's PCI scan must find them: at least
	// one device carrying virtio's PCI vendor id, and each such modern virtio device
	// must report a recognizable device type.
	let devices = arch::pci::scan();
	let virtio: alloc::vec::Vec<_> = devices.iter().filter(|d| d.is_virtio()).collect();
	assert!(!virtio.is_empty(), "the PCI scan should find the QEMU virtio devices");
	for d in &virtio {
		assert!(d.virtio_type().is_some(), "a modern virtio device should report a device type (id {:#06x})", d.device_id);
	}
}

#[cfg(test)]
#[test_case]
fn device_table_exposes_virtio_mmio() {
	use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};
	// device::init() populated the table at boot from the PCI scan. A driver-like
	// thread queries it the way DeviceManager will: count the devices, read the
	// first one's DeviceInfo, acquire its DeviceMemory capability, and map the MMIO.
	static COUNT: AtomicI64 = AtomicI64::new(-1);
	static VTYPE: AtomicU64 = AtomicU64::new(0);
	static BAR_LEN: AtomicU64 = AtomicU64::new(0);
	static MAPPED: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		let mut info = abi::DeviceInfo::default();
		let size = core::mem::size_of::<abi::DeviceInfo>() as u64;
		unsafe {
			COUNT.store(arch::syscall::invoke(syscall::SYS_DEVICE_COUNT, 0, 0, 0, 0) as i64, Ordering::SeqCst);
			if arch::syscall::invoke(syscall::SYS_DEVICE_INFO, 0, &mut info as *mut _ as u64, size, 0) as i64 == 0 {
				VTYPE.store(info.virtio_type as u64, Ordering::SeqCst);
				BAR_LEN.store(info.bar_len, Ordering::SeqCst);
			}
			let handle = arch::syscall::invoke(syscall::SYS_DEVICE_ACQUIRE, 0, 0, 0, 0);
			if !syscall::sys_is_err(handle) {
				MAPPED.store(arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, handle, 0, 0, 0), Ordering::SeqCst);
			}
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(COUNT.load(Ordering::SeqCst) >= 3, "expected at least the 3 QEMU virtio devices");
	assert!((1..=4).contains(&VTYPE.load(Ordering::SeqCst)), "device 0 should report a virtio type");
	assert!(BAR_LEN.load(Ordering::SeqCst) > 0, "the MMIO BAR should have a non-zero length");
	let mapped = MAPPED.load(Ordering::SeqCst);
	assert!(mapped != 0 && !syscall::sys_is_err(mapped), "the device MMIO should map to a valid address");
}

#[cfg(test)]
#[test_case]
fn dma_buffer_maps_and_reports_phys() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	// A driver allocates a DMA buffer for its virtqueue, maps it, and programs its
	// physical base into the device. Here a thread writes a marker through the
	// mapping and reads it back at the reported physical address (via the HHDM),
	// proving the mapping and the phys base name the same memory - what makes device
	// DMA work. The check runs inside the thread, while the buffer is still alive
	// (it is freed when the thread's process is reaped).
	const MARK: u64 = 0xc0ffee_d00d_u64;
	static PHYS: AtomicU64 = AtomicU64::new(0);
	static READBACK: AtomicU64 = AtomicU64::new(0);
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let handle = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			if syscall::sys_is_err(handle) {
				return;
			}
			let virt = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_MAP, handle, 0, 0, 0);
			let phys = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_PHYS, handle, 0, 0, 0);
			if syscall::sys_is_err(virt) {
				return;
			}
			(virt as *mut u64).write_volatile(MARK);
			let via_hhdm = ((mem::hhdm_offset() + phys) as *const u64).read_volatile();
			PHYS.store(phys, Ordering::SeqCst);
			READBACK.store(via_hhdm, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the DMA buffer thread did not complete");
	assert!(PHYS.load(Ordering::SeqCst) != 0, "the DMA buffer should report a non-zero physical base");
	assert_eq!(READBACK.load(Ordering::SeqCst), MARK, "the bytes written through the mapping must be visible at the physical base");
}

#[cfg(test)]
#[test_case]
fn log_record_roundtrip_and_renders() {
	use abi::log::{LogRecord, Severity, encode, render_cbor, render_json, render_text};
	// A LogRecord is the canonical structured object; text/JSON/CBOR are derived
	// representations. Encode one, parse it back (the fields survive), then render
	// the SAME record three ways and check each representation byte-for-byte.
	let fields: [(&[u8], &[u8]); 2] = [(b"event", b"online"), (b"files", b"2")];
	let mut wire: [u8; 128] = [0u8; 128];
	let n: usize = encode(42, Severity::Info, b"storage_service", &fields, &mut wire).expect("encode fits");
	let rec: LogRecord<'_> = LogRecord::parse(&wire[..n]).expect("parse round-trips");
	assert_eq!(rec.ts(), 42);
	assert_eq!(rec.severity(), Severity::Info);
	assert_eq!(rec.source(), b"storage_service");
	assert_eq!(rec.field_count(), 2);
	let mut it = rec.fields();
	assert_eq!(it.next(), Some((&b"event"[..], &b"online"[..])));
	assert_eq!(it.next(), Some((&b"files"[..], &b"2"[..])));
	assert_eq!(it.next(), None);
	// human text
	let mut tbuf: [u8; 128] = [0u8; 128];
	let tn: usize = render_text(&rec, &mut tbuf).expect("text fits");
	assert_eq!(&tbuf[..tn], b"[42] INFO storage_service: event=online files=2");
	// JSON
	let mut jbuf: [u8; 256] = [0u8; 256];
	let jn: usize = render_json(&rec, &mut jbuf).expect("json fits");
	assert_eq!(&jbuf[..jn], br#"{"ts":42,"severity":"INFO","source":"storage_service","fields":{"event":"online","files":"2"}}"#);
	// CBOR: map(4); spot-check the head and that the source text is embedded
	let mut cbuf: [u8; 128] = [0u8; 128];
	let cn: usize = render_cbor(&rec, &mut cbuf).expect("cbor fits");
	assert_eq!(cbuf[0], 0xA4, "CBOR record is a 4-entry map");
	assert!(cbuf[..cn].windows(b"storage_service".len()).any(|w: &[u8]| w == b"storage_service"), "source string present in CBOR");
}

#[cfg(test)]
#[test_case]
fn log_service_ingests_queries_and_renders() {
	use abi::log::{self, Severity};
	use alloc::sync::Arc;
	use object::KernelObject;
	use object::channel::{Channel, Message};
	use object::handle::Capability;
	use object::rights::Rights;

	// Drive the real userspace LogService as a client: spawn it from the init
	// package, hand it a serve channel, EMIT two structured records, then QUERY them
	// back in each representation. The queries and a quit sentinel are pre-queued so
	// the cooperative service drains them in one pass and exits, after which we read
	// its replies (the M16/M17 kernel-as-client pattern).
	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = package.lookup(b"log_service").expect("log_service in the init package");

	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn LogService");

	// hand the service the channel its clients reach it on
	let server_cap = Capability::new(service_server as Arc<dyn KernelObject>, Rights::ALL, 0);
	boot_kernel.send(Message::new(b"SERVE".to_vec(), alloc::vec![server_cap], 0)).expect("serve bootstrap");

	// helper: build and send an EMIT for one record
	let emit = |ts: u64, severity: Severity, source: &[u8], fields: &[(&[u8], &[u8])]| {
		let mut wire = [0u8; 128];
		let n = log::encode(ts, severity, source, fields, &mut wire).expect("encode record");
		let mut msg = alloc::vec::Vec::with_capacity(1 + n);
		msg.push(log::OP_EMIT);
		msg.extend_from_slice(&wire[..n]);
		service_client.send(Message::new(msg, alloc::vec::Vec::new(), 0)).expect("emit");
	};
	emit(10, Severity::Info, b"storage_service", &[(b"event" as &[u8], b"online" as &[u8])]);
	emit(11, Severity::Error, b"device_manager", &[(b"code" as &[u8], b"5" as &[u8])]);

	// queries: all severities in each representation, then a filtered text query
	let query = |format: u8, min: Severity| {
		service_client.send(Message::new(alloc::vec![log::OP_QUERY, format, min as u8], alloc::vec::Vec::new(), 0)).expect("query");
	};
	query(log::FORMAT_TEXT, Severity::Trace);
	query(log::FORMAT_JSON, Severity::Trace);
	query(log::FORMAT_CBOR, Severity::Trace);
	query(log::FORMAT_TEXT, Severity::Error);
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// text: both records, one per line
	let text = service_client.recv().expect("text reply");
	assert_eq!(&text.bytes[..], b"[10] INFO storage_service: event=online\n[11] ERROR device_manager: code=5\n");
	// JSON: an array of two objects
	let json = service_client.recv().expect("json reply");
	assert_eq!(&json.bytes[..], br#"[{"ts":10,"severity":"INFO","source":"storage_service","fields":{"event":"online"}},{"ts":11,"severity":"ERROR","source":"device_manager","fields":{"code":"5"}}]"#);
	// CBOR: an array of two records
	let cbor = service_client.recv().expect("cbor reply");
	assert_eq!(cbor.bytes[0], 0x82, "CBOR reply is an array of two records");
	// filtered: only Error and above -> just the device_manager record
	let filtered = service_client.recv().expect("filtered reply");
	assert_eq!(&filtered.bytes[..], b"[11] ERROR device_manager: code=5\n");
}

#[cfg(test)]
#[test_case]
fn init_package_starts_system_manager() {
	// The boot chain, end to end: SystemManager starts from the init package, spawns
	// ServiceManager and delegates the package and the ramdisk to it, and
	// ServiceManager brings up the core services in dependency order - LogService
	// first (DeviceManager, StorageService, and the shell all depend on it, though
	// they are listed before it in the manifest, so the order is driven by declared
	// dependencies). StorageService is handed the ramdisk and a service channel
	// before it reports in; the shell is handed the StorageService client channel
	// and proves the round-trip by reading a file with `cat` before it reports in.
	// Every report is relayed up, so the kernel observes the services come up in
	// dependency order, then DeviceManager stopped (ServiceManager exercises the
	// stop path on that leaf service - nothing depends on it), followed by the two
	// managers.
	let (kernel_ep, _koid) = spawn_system_manager().expect("SystemManager should start from the init package");
	sched::run_until_idle();
	let reports: [&[u8]; 7] = [b"LogService: online", b"DeviceManager: online", b"StorageService: online", b"Shell: online", b"DeviceManager: stopped", b"ServiceManager: online", b"SystemManager: online"];
	for expected in reports {
		let message = kernel_ep.recv().expect("a boot-chain report should arrive");
		assert_eq!(&message.bytes[..], expected, "boot-chain reports must arrive in dependency order");
	}
}

#[cfg(test)]
#[test_case]
fn system_manager_recovery_escalates_after_repeated_crashes() {
	use object::KernelObject;
	// The kernel supervises SystemManager: if it faults, the kernel starts a
	// recovery SystemManager, up to a limit, then escalates (it reboots in
	// production). Here the "SystemManager" faults on every attempt (a ring-3 page
	// fault), so supervision detects each crash via the crash-notify channel,
	// exhausts its restarts, and reports failure - the trigger for escalation.
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let up = supervise(&crash_rx, 3, || {
		let thread = sched::spawn(user_fault_thread_body, 0);
		thread.process().header().koid()
	});
	fault::clear_crash_notify();
	assert!(!up, "a SystemManager that faults on every attempt must exhaust recovery and escalate");
}

#[cfg(test)]
#[test_case]
fn system_manager_recovery_survives_a_clean_start() {
	use object::KernelObject;
	// A SystemManager that does not fault must survive on the first attempt, so
	// supervision returns "up" without starting a recovery SystemManager.
	extern "C" fn clean_body(_arg: u64) {}
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let up = supervise(&crash_rx, 3, || {
		let thread = sched::spawn(clean_body, 0);
		thread.process().header().koid()
	});
	fault::clear_crash_notify();
	assert!(up, "a SystemManager that does not fault should survive without recovery");
}

#[cfg(test)]
#[test_case]
fn userspace_spawn_syscalls_start_a_second_process() {
	use core::sync::atomic::{AtomicU64, Ordering};
	// A kernel thread drives the userspace spawn syscalls exactly as a ring-3
	// spawner would: process_create -> process_load -> thread_create -> thread_start.
	// The image is the embedded LogService ELF, a leaf service that reports in over
	// its bootstrap channel and exits. The spawner hands the child the channel
	// endpoint it received as its own bootstrap (transferred through thread_create).
	static ELF_PTR: AtomicU64 = AtomicU64::new(0);
	static ELF_LEN: AtomicU64 = AtomicU64::new(0);
	extern "C" fn spawner(bootstrap: u64) {
		unsafe {
			let child = arch::syscall::invoke(syscall::SYS_PROCESS_CREATE, 0, 0, 0, 0);
			assert!((child as i64) > 0, "process_create");
			let entry = arch::syscall::invoke(syscall::SYS_PROCESS_LOAD, child, ELF_PTR.load(Ordering::SeqCst), ELF_LEN.load(Ordering::SeqCst), 0);
			assert!((entry as i64) > 0, "process_load");
			let thread = arch::syscall::invoke(syscall::SYS_THREAD_CREATE, child, entry, memlayout::USER_STACK_TOP, bootstrap);
			assert!((thread as i64) > 0, "thread_create");
			let started = arch::syscall::invoke(syscall::SYS_THREAD_START, thread, 0, 0, 0);
			assert_eq!(started as i64, 0, "thread_start");
		}
	}
	let bytes = init_package_bytes().expect("init package present");
	let package = pkg::Package::parse(bytes).expect("init package parses");
	let elf = package.lookup(b"log_service").expect("log_service image");
	ELF_PTR.store(elf.as_ptr() as u64, Ordering::SeqCst);
	ELF_LEN.store(elf.len() as u64, Ordering::SeqCst);
	let (kernel_ep, user_ep) = object::channel::Channel::create();
	sched::spawn_with_object(spawner, user_ep, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = kernel_ep.recv().expect("the spawned process should report in over IPC");
	assert_eq!(&message.bytes[..], b"LogService: online");
}

#[cfg(test)]
#[test_case]
fn storage_serves_volume_file_to_client() {
	// The StorageService (a ring-3 process) maps a ramdisk volume, and a client
	// process opens vol://system/hello.txt through it, receives a shared-buffer
	// capability to the file's bytes, maps it, and reports the contents back. The
	// bytes the client read must equal the file straight from the volume archive -
	// an end-to-end, capability-brokered, zero-copy read across two userspace
	// processes coordinated only by IPC.
	let (expected, actual) = run_storage_scenario().expect("the storage scenario should run");
	assert!(!expected.is_empty(), "the volume file should not be empty");
	assert_eq!(actual, expected);
}

#[cfg(test)]
#[test_case]
fn object_info_get_reports_object() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// object_info_get introspects a handle in the caller's table, so it runs inside
	// a spawned kernel thread (which has one). It reports the object's identity,
	// type, and the rights the handle confers, and rejects an unknown handle.
	extern "C" fn body(_arg: u64) {
		use object::ObjectType;
		use object::rights::Rights;
		unsafe {
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			let mut info = syscall::ObjectInfo { koid: 0, object_type: 0, rights: 0, generation: 0 };
			let info_ptr = &mut info as *mut syscall::ObjectInfo as u64;
			let size = core::mem::size_of::<syscall::ObjectInfo>() as u64;
			let got = arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, handle, info_ptr, size, 0);
			assert_eq!(got, 1);
			assert!(info.koid >= 1);
			assert_eq!(info.object_type, ObjectType::MemoryObject.code());
			assert_eq!(info.rights, Rights::ALL.bits());
			assert!(info.generation >= 1);
			// an unknown handle is rejected with the bad-handle error
			let bad = arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, 0xdead_beef, info_ptr, size, 0);
			assert_eq!(bad as i64, syscall::ERR_BAD_HANDLE);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn system_graph_reflects_live_state() {
	use object::address_space::AddressSpace;
	use object::channel::Channel;
	use object::domain::Domain;
	use object::process::Process;
	use object::rights::Rights;
	use object::{KernelObject, ObjectType};
	// A standalone Domain with one process holding two handles. Collecting the
	// graph from that Domain must reflect the live structure exactly: one process
	// with two handles, one of them the channel we installed - with its koid, type,
	// rights, and badge intact. Dropping the process removes it from the graph.
	let domain = Domain::new(1 << 20, 16, 8);
	let process = Process::new(AddressSpace::kernel(), domain.clone());
	let (endpoint, _peer) = Channel::create();
	let channel_koid = endpoint.header().koid();
	process.install(endpoint, Rights::READ | Rights::WRITE, 42);
	process.install(object::event::Event::create(), Rights::ALL, 0);

	let node = graph::collect_from(&domain);
	assert_eq!(node.koid, domain.header().koid());
	assert_eq!(node.processes.len(), 1, "the Domain has one live process");
	let proc_node = &node.processes[0];
	assert_eq!(proc_node.koid, process.header().koid());
	assert_eq!(proc_node.handles.len(), 2, "the process holds two handles");
	let channel_handle = proc_node.handles.iter().find(|h| h.koid == channel_koid).expect("the channel handle should appear in the graph");
	assert_eq!(channel_handle.object_type, ObjectType::Channel);
	assert_eq!(channel_handle.rights, Rights::READ | Rights::WRITE);
	assert_eq!(channel_handle.badge, 42);

	// Dropping the process removes it from the live graph.
	drop(process);
	let after = graph::collect_from(&domain);
	assert_eq!(after.processes.len(), 0, "the process is gone after it drops");
}

#[cfg(test)]
#[test_case]
fn cli_reads_file_through_storage_service() {
	// The CLI's `cat` path: the kernel drives the StorageService as its own client,
	// sending one open request and a quit sentinel, then reads the returned shared
	// buffer. The bytes must equal the file straight from the volume archive - a
	// command round-tripping to a real userspace service.
	let expected = pkg::Package::parse(volume_package_bytes().expect("the volume package should be present")).and_then(|p| p.lookup(b"hello.txt").map(|b| b.to_vec())).expect("hello.txt should be in the volume");
	let actual = storage_read(b"vol://system/hello.txt").expect("the storage read should succeed");
	assert!(!expected.is_empty(), "the volume file should not be empty");
	assert_eq!(actual, expected);
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

// Kernel-thread body that drops to ring 3 running the embedded cooperative-yield
// program. Each instance takes a distinct slot so two can be alive on the same
// core at once (their user pages share the kernel address space at non-overlapping
// virtual addresses). The program yields several times before reporting in, so two
// instances interleave through the scheduler.
#[cfg(test)]
extern "C" fn user_yield_thread_body(handle: u64) {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame::{self, PAGE_SIZE};
	static SLOT: AtomicU64 = AtomicU64::new(0);
	let slot = SLOT.fetch_add(1, Ordering::Relaxed);
	let code_va = 0x0000_0000_5000_0000 + slot * 0x0010_0000;
	let stack_va = code_va + 0x0001_0000;
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(code_va, code, flags);
	arch::paging::map_page(stack_va, stack, flags);
	let program = arch::usermode::program_yield_bytes();
	unsafe {
		core::ptr::copy_nonoverlapping(program.as_ptr(), code_va as *mut u8, program.len());
		arch::usermode::enter(code_va, stack_va + PAGE_SIZE, handle);
	}
	arch::paging::unmap_page(code_va);
	arch::paging::unmap_page(stack_va);
	frame::deallocate(code);
	frame::deallocate(stack);
}

#[cfg(test)]
#[test_case]
fn userspace_yields_cooperatively() {
	use object::channel::Channel;
	// Two ring-3 threads share one core and each call SYS_YIELD several times
	// before sending "OK". The yields interleave them through the scheduler, which
	// only works if every syscall saves its user return state (rip/rsp/rflags) and
	// its kernel syscall stack per thread - a single per-CPU slot would be clobbered
	// by the sibling and one thread would return to the wrong context. Both messages
	// arriving proves the save path is per-thread.
	let (k0, u0) = Channel::create();
	let (k1, u1) = Channel::create();
	sched::spawn_with_object(user_yield_thread_body, u0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(user_yield_thread_body, u1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert_eq!(&k0.recv().expect("first ring-3 thread sent a message").bytes[..], b"OK");
	assert_eq!(&k1.recv().expect("second ring-3 thread sent a message").bytes[..], b"OK");
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
fn driver_crash_is_cleaned_up_and_notified() {
	use object::KernelObject;
	use object::domain::Domain;
	// A "driver" process binds an IRQ and creates a DMA buffer, then faults. The
	// kernel must detach the IRQ, refund the DMA, remove the caps, and deliver a
	// crash record naming the process - all without cooperation from the driver.
	let (notify_tx, notify_rx) = object::channel::Channel::create();
	fault::set_crash_notify(notify_tx);
	let domain = Domain::new(1 << 20, 8, 4);
	let koid = {
		let driver = sched::spawn_in(domain.clone(), driver_crash_thread_body, 0).expect("spawn driver");
		// Capture the process identity, then drop the Arc so reaping the thread can
		// tear the process down and run the crash cleanup.
		driver.process().header().koid()
	};
	sched::run_until_idle();
	// The IRQ binding is gone, and the DMA and handle quotas are back to zero: the
	// crashed driver's resources were reclaimed by the kernel.
	assert!(!arch::interrupts::is_bound(DRIVER_IRQ_VECTOR as u8), "the driver's IRQ should be detached");
	assert_eq!(domain.account().dma().used(), 0, "the driver's DMA should be refunded");
	assert_eq!(domain.account().handles().used(), 0, "the driver's handles should be removed");
	// A crash record naming the driver process was delivered to the supervisor.
	let record = notify_rx.recv().expect("a crash notification should be delivered");
	assert_eq!(record.bytes.len(), 16, "crash record is koid + kind");
	let got_koid = u64::from_le_bytes(record.bytes[0..8].try_into().unwrap());
	let got_kind = u64::from_le_bytes(record.bytes[8..16].try_into().unwrap());
	assert_eq!(got_koid, koid, "crash record names the crashed process");
	assert_eq!(got_kind, fault::FAULT_PAGE, "crash record carries the fault kind");
	fault::clear_crash_notify();
}

#[cfg(test)]
#[test_case]
fn device_manager_reacts_to_a_driver_crash() {
	use object::KernelObject;
	use object::domain::Domain;
	// DeviceManager's reaction to a driver crash: the kernel reports the crash on the
	// crash-notify channel (M20h), and the supervisor finds the device that driver
	// was bound to and marks it offline. Here device 0 is driven by a process that
	// then crashes; consuming the crash event, the supervisor marks it offline.
	#[derive(PartialEq, Debug)]
	enum DeviceState {
		Online,
		Offline,
	}
	let (notify_tx, notify_rx) = object::channel::Channel::create();
	fault::set_crash_notify(notify_tx);
	let mut device0 = DeviceState::Online;
	let domain = Domain::new(1 << 20, 8, 4);
	let driver_koid = {
		let driver = sched::spawn_in(domain.clone(), driver_crash_thread_body, 0).expect("spawn driver");
		driver.process().header().koid()
	};
	sched::run_until_idle();
	// react: the crash event names the crashed process; if it is our device's driver,
	// mark the device offline.
	let record = notify_rx.recv().expect("a crash event should be delivered");
	let crashed_koid = u64::from_le_bytes(record.bytes[0..8].try_into().unwrap());
	if crashed_koid == driver_koid {
		device0 = DeviceState::Offline;
	}
	fault::clear_crash_notify();
	assert_eq!(device0, DeviceState::Offline, "DeviceManager should mark a crashed driver's device offline");
}

#[cfg(test)]
#[test_case]
fn driver_survives_crash_and_restart() {
	use object::KernelObject;
	// The driver crash/restart cycle: a driver that faults is respawned by its
	// supervisor, and the restarted driver runs cleanly. The supervisor spawns the
	// driver, detects the fault on the crash-notify channel, and respawns it until an
	// attempt survives - the loop DeviceManager runs over a driver's bootstrap channel
	// (a crash there peer-closes it) and the kernel runs to recover SystemManager.
	extern "C" fn clean_driver(_arg: u64) {}
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let mut restarts: u32 = 0;
	let mut survived = false;
	for attempt in 0..4u32 {
		// the first start faults; each restart runs the clean driver.
		let body: extern "C" fn(u64) = if attempt == 0 { user_fault_thread_body } else { clean_driver };
		let koid = {
			let driver = sched::spawn(body, 0);
			driver.process().header().koid()
		};
		sched::run_until_idle();
		if crash_seen(&crash_rx, koid) {
			restarts += 1;
			continue;
		}
		survived = true;
		break;
	}
	fault::clear_crash_notify();
	assert!(survived, "the restarted driver should run without faulting");
	assert!(restarts >= 1, "the supervisor should have restarted the crashed driver");
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
fn dma_buffer_quota_enforced_cleanly() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread accounted to a Domain capped at two pages of pinned DMA. The
	// dma_buffer_create syscall charges the DMA quota at the create boundary, so a
	// third buffer must be refused cleanly (ERR_RESOURCE_EXHAUSTED, nothing
	// allocated) and closing the buffers must refund the quota.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let d0 = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(d0));
			let d1 = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(d1));
			let d2 = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert_eq!(d2 as i64, syscall::ERR_RESOURCE_EXHAUSTED);
			// Closing the buffers refunds both their DMA quota and their handles.
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, d0, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, d1, 0, 0, 0) as i64, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	domain.account().dma().set_limit(2 * 4096);
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "DMA quota test thread did not finish");
	// Every buffer was closed, so the pinned-DMA quota is back to zero.
	assert_eq!(domain.account().dma().used(), 0);
}

#[cfg(test)]
#[test_case]
fn ipc_queue_bytes_accounting_enforced() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread in a Domain capped at 250 bytes of in-transit IPC. Each 100-byte
	// send charges the sender's queue, so two fit and the third is refused
	// (WOULD_BLOCK); receiving one refunds the quota, so a send fits again.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut handles = [0u64; 2];
			let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, handles.as_mut_ptr() as u64, handles.as_mut_ptr().add(1) as u64, 0, 0);
			assert_eq!(created as i64, 0, "channel create failed");
			let (h0, h1) = (handles[0], handles[1]);
			let payload = [0u8; 100];
			let p = payload.as_ptr() as u64;
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, syscall::ERR_WOULD_BLOCK, "the third send should hit the queue cap");
			// Receiving one message refunds the sender's queue, so a send fits again.
			let mut buf = [0u8; 128];
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, h1, buf.as_mut_ptr() as u64, 128, 0);
			assert_eq!(n as i64, 100);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, 0, "a send fits after a recv refund");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	domain.account().ipc_queue().set_limit(250);
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "ipc queue test thread did not finish");
	// The thread and its channels are reaped, refunding every undelivered message.
	assert_eq!(domain.account().ipc_queue().used(), 0);
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
