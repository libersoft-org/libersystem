#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

mod arch;
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

// Write a raw byte slice to the serial port (always) and mirror it to the framebuffer
// console (if one is initialized), without the per-char format_args _print does. Backs
// the bulk SYS_DEBUG_WRITE path so the console service flushes a screenful of
// serial-mirror output in one syscall instead of one (formatted) syscall per byte.
#[doc(hidden)]
pub fn _print_bytes(bytes: &[u8]) {
	arch::serial::write_bytes(bytes);
	console::write_bytes(bytes);
}

// Single-byte twin of _print_bytes, for the legacy single-byte SYS_DEBUG_WRITE form.
#[doc(hidden)]
pub fn _print_byte(byte: u8) {
	_print_bytes(&[byte]);
}

// kernel entry point (ELF entry, see ENTRY(kmain) in the linker script)
#[unsafe(no_mangle)]
unsafe extern "C" fn kmain() -> ! {
	arch::serial::init();
	serial_println!("{} kernel is starting ...", product::NAME);
	arch::init();
	init_memory();
	init_framebuffer();
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
// console is up for both paths; it allocates its grid model (the shared `term`
// stack), so it must run after init_memory brings up the heap.
fn init_framebuffer() {
	let Some(response) = FRAMEBUFFER_REQUEST.get_response() else {
		return;
	};
	let Some(fb) = response.framebuffers().next() else {
		return;
	};
	console::init(console::FbInfo { addr: fb.addr(), width: fb.width() as usize, height: fb.height() as usize, pitch: fb.pitch() as usize, bytes_per_pixel: fb.bpp() as usize / 8, red_shift: fb.red_mask_shift(), red_size: fb.red_mask_size(), green_shift: fb.green_mask_shift(), green_size: fb.green_mask_size(), blue_shift: fb.blue_mask_shift(), blue_size: fb.blue_mask_size() });
}

// The boot framebuffer's virtual base + geometry, for the framebuffer_map syscall to
// hand the display to a userspace ConsoleService. Re-queries the Limine response
// (it is 'static), or None if there is no framebuffer (headless / no video mode).
pub fn framebuffer_geometry() -> Option<(u64, abi::Framebuffer)> {
	let fb = FRAMEBUFFER_REQUEST.get_response()?.framebuffers().next()?;
	let geom = abi::Framebuffer { width: fb.width() as u32, height: fb.height() as u32, pitch: fb.pitch() as u32, bytes_per_pixel: (fb.bpp() / 8) as u32, red_shift: fb.red_mask_shift(), red_size: fb.red_mask_size(), green_shift: fb.green_mask_shift(), green_size: fb.green_mask_size(), blue_shift: fb.blue_mask_shift(), blue_size: fb.blue_mask_size(), _pad: [0; 2] };
	Some((fb.addr() as u64, geom))
}

// Wake the application processors and wait for every core to report in. Runs
// before the test/boot split so SMP is up for both paths.
fn init_smp() {
	let mp = MP_REQUEST.get_response().expect("Limine: no MP response");
	smp::init(mp);
}

#[cfg(not(test))]
fn boot_main() {
	if !BASE_REVISION.is_supported() {
		serial_println!("ERROR: Limine base revision not supported");
		return;
	}
	serial_println!("arch: {}", arch::NAME);
	serial_println!("smp: {} of {} cores online", smp::online_count(), smp::cpu_count());
	serial_println!("memory: {} physical frames free", mem::frame::free_count());
	// Perf-trace anchor: publish the calibrated TSC frequency so the host trace tool can
	// convert the ring-3 `\x1ePERF` cycle markers to wall-clock time.
	serial_println!("\x1ePERF tsc_hz {}", arch::tsc::hz());
	serial_println!("boot OK - entering the userspace shell (type 'help', or 'exit' to halt)");
	boot_userspace_with_recovery();
	serial_println!("halting");
}

// Pump the serial UART into the console input and nudge the shell's first prompt.
// Registered as the scheduler's idle hook (sched::set_idle_hook) so it runs on the
// BSP's idle spin: a polling driver (virtio-gpu's display-resize timer) keeps the BSP
// in run_until_idle so it never reaches console_shell_loop's own pump, yet serial
// input must stay live. The one-shot newline nudges the shell's first prompt once it
// has attached (the keyboard path nudges the same way on its first key).
#[cfg(not(test))]
fn serial_console_pump() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static NUDGED: AtomicBool = AtomicBool::new(false);
	if !NUDGED.load(Ordering::Relaxed) && console_input::shell_listening() {
		NUDGED.store(true, Ordering::Relaxed);
		console_input::feed(b'\n');
	}
	// Drain the whole serial RX FIFO each wake: the BSP now halts between idle passes
	// (~100 Hz timer wakes) instead of busy-spinning, so polling one byte per pass could
	// let a fast paste overrun the 16-byte UART FIFO. Reading until empty keeps serial
	// input lossless at the lower poll rate.
	while let Some(byte) = arch::serial::read_byte() {
		console_input::feed(byte);
	}
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
	// Nudge the shell to print its first prompt, then pump both input sources until
	// it exits. Each round forwards any waiting serial byte and runs the cooperative
	// schedule, so threads a device interrupt woke also make progress: the
	// virtio-input keyboard driver feeds console input from its own IRQ handler, so
	// the shell must be pumped whenever an interrupt arrives, not only when a serial
	// byte does. Polling serial (rather than blocking on it) keeps that interrupt
	// path live while no one is typing on the wire.
	console_input::feed(b'\n');
	while console_input::shell_listening() {
		if let Some(byte) = arch::serial::read_byte() {
			if !console_input::feed(byte) {
				break;
			}
		}
		sched::run_until_idle();
		core::hint::spin_loop();
	}
}

// Userspace (ring 3) page layout for the test: one USER page for the program,
// one for its stack, mapped into the low half of the shared address space
// (per-process page tables / CR3 isolation are a later milestone).
#[cfg(test)]
use crate::memlayout::{USER_CODE_VA, USER_STACK_VA};

// Kernel-thread body that runs a ring-3 program. It maps a USER code and stack
// page, copies the embedded position-independent program in, and drops to ring 3
// with its bootstrap Channel handle. The program makes a capability-gated channel
// send and a debug-write, then exits back here, where we tear the mapping down.
#[cfg(test)]
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
	// Pump the serial console from the scheduler's idle spin: virtio-gpu polls its
	// display size on a short repeating timer, so run_until_idle never returns and the
	// BSP would never reach console_shell_loop to poll the UART. The idle hook keeps
	// serial input live regardless (the keyboard is interrupt-driven and unaffected).
	sched::set_idle_hook(serial_console_pump);
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

// Load the volume archive bytes and the parsed init package - the 'static modules
// every userspace scenario starts from.
#[cfg(test)]
fn scenario_packages() -> Result<(&'static [u8], pkg::Package<'static>), &'static str> {
	let volume = volume_package_bytes().ok_or("volume package module not found")?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(init).ok_or("init package is malformed")?;
	Ok((volume, package))
}

// Look up `name` in the volume archive and return a copy of its bytes - the file a
// scenario expects the component/client to read back.
#[cfg(test)]
fn volume_file(volume: &[u8], name: &[u8]) -> Result<alloc::vec::Vec<u8>, &'static str> {
	pkg::Package::parse(volume).and_then(|p| p.lookup(name).map(|b| b.to_vec())).ok_or("file missing from the volume package")
}

// Send a tagged capability over a bootstrap channel: wrap `object` in a Capability
// carrying `rights` and send it with `payload` as the message bytes. The shared
// "hand a process one of its initial capabilities" step the scenarios repeat.
#[cfg(test)]
fn send_cap(channel: &object::channel::Channel, payload: &[u8], object: alloc::sync::Arc<dyn object::KernelObject>, rights: object::rights::Rights) -> Result<(), &'static str> {
	let cap = object::handle::Capability::new(object, rights, 0);
	channel.send(object::channel::Message::new(payload.to_vec(), alloc::vec![cap], 0)).map_err(|_| "bootstrap capability send failed")
}

// Create a ramdisk MemoryObject from `volume`, fill it, and hand it to a service's
// bootstrap channel as "RAMDISK" + the volume's byte length, with a read+map cap.
#[cfg(test)]
fn send_ramdisk(channel: &object::channel::Channel, volume: &[u8]) -> Result<(), &'static str> {
	use object::rights::Rights;
	let ramdisk = object::memory_object::MemoryObject::create(volume.len()).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);
	let mut msg = alloc::vec::Vec::with_capacity(7 + 8);
	msg.extend_from_slice(b"RAMDISK");
	msg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	send_cap(channel, &msg, ramdisk, Rights::READ | Rights::MAP)
}

// Create a MemoryObject from `archive`, fill it, and hand it to a process's bootstrap
// channel as "PACKAGE" + the archive's byte length, with a read+map+transfer cap - the
// rt recv_package handshake. A launcher (e.g. PermissionManager) maps it and spawns the
// programs it governs from it.
#[cfg(test)]
fn send_package(channel: &object::channel::Channel, archive: &[u8]) -> Result<(), &'static str> {
	use object::rights::Rights;
	let object = object::memory_object::MemoryObject::create(archive.len()).ok_or("no memory for the package")?;
	copy_into_object(&object, archive);
	let mut msg = alloc::vec::Vec::with_capacity(7 + 8);
	msg.extend_from_slice(b"PACKAGE");
	msg.extend_from_slice(&(archive.len() as u64).to_le_bytes());
	send_cap(channel, &msg, object, Rights::READ | Rights::MAP | Rights::TRANSFER)
}

// Build the M16 storage topology and run it to completion. A MemoryObject holds
// the ramdisk volume; the StorageService process maps it and serves files over a
// service channel; a client process opens vol://system/hello.txt through the
// service, receives a shared-buffer capability to the file's bytes, maps it, and
// reports the contents back over its bootstrap channel. The kernel only brokers
// the initial capabilities - the open, the resolve, and the zero-copy read all
// happen in userspace. Returns (expected, actual): the file straight from the
// volume archive, and the bytes the client read through the service.
#[cfg(test)]
fn run_storage_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	// the volume archive backing the ramdisk, the file we expect served, and the
	// userspace programs from the init package
	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"hello.txt")?;
	let service_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let client_elf = package.lookup(b"storage_client").ok_or("storage_client missing from the init package")?;

	// channels: a bootstrap per process, plus the service<->client request channel
	let (service_boot_kernel, service_boot_user) = Channel::create();
	let (client_boot_kernel, client_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	// spawn the two processes with their bootstrap endpoints
	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), service_elf, service_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain, client_elf, client_boot_user, Rights::ALL, 0).map_err(|_| "failed to load the storage client")?;

	// hand the service its ramdisk (with the volume length) and its service
	// endpoint, then hand the client the other end of that service channel.
	send_ramdisk(&service_boot_kernel, volume)?;
	send_cap(&service_boot_kernel, b"SERVE", service_server, Rights::ALL)?;
	send_cap(&client_boot_kernel, b"CONNECT", service_client, Rights::ALL)?;

	// run the cooperative schedule until everyone is done, then read the result
	sched::run_until_idle();
	let result = client_boot_kernel.recv().map_err(|_| "the client reported no result")?;
	Ok((expected, result.bytes))
}

// Build the M28 WASI topology and run it to completion. A StorageService serves the
// ramdisk volume; the wasi_host process loads the embedded Wasm component and runs
// it, and the component's only import (`liber.read`) is wired by the host to read
// the granted file vol://system/hello.txt through StorageService into the
// component's linear memory. The component has no other capability - no ambient
// authority. The host reports the bytes the component read back over its bootstrap
// channel. The kernel only brokers the initial capabilities. Returns (expected,
// actual): the file straight from the volume, and the bytes the component read.
#[cfg(test)]
fn run_wasi_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"hello.txt")?;
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let host_elf = package.lookup(b"wasi_host").ok_or("wasi_host missing from the init package")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain, host_elf, host_boot_user, Rights::ALL, 0).map_err(|_| "failed to load wasi_host")?;

	// storage bootstrap: the ramdisk volume and its service channel; the host gets
	// only the StorageService client - the one capability it is granted.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", service_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"STORAGE", service_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	Ok((expected, result.bytes))
}

// Build the M29 powerbox topology and run it to completion. A StorageService serves
// the ramdisk volume; a file_picker holds the trusted storage client and serves the
// Picker contract; the wasi_host is given ONLY a picker client - no filesystem
// access of its own - and runs the same Wasm component. The component's read import
// now goes through the picker: `pick` (standing in for the user's choice) opens the
// chosen file (motd.txt) over StorageService and hands back that one file as a
// handle<file> capability, which the host reads into the component's memory. So a
// component with no filesystem capability reaches exactly the user-picked file and
// nothing else. The kernel only brokers the initial capabilities. Returns
// (expected, actual): the picked file straight from the volume, and what the
// component read.
#[cfg(test)]
fn run_powerbox_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"motd.txt")?;
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let picker_elf = package.lookup(b"file_picker").ok_or("file_picker missing from the init package")?;
	let host_elf = package.lookup(b"wasi_host").ok_or("wasi_host missing from the init package")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (picker_boot_kernel, picker_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (picker_server, picker_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), picker_elf, picker_boot_user, Rights::ALL, 0).map_err(|_| "failed to load file_picker")?;
	loader::spawn_elf_process(domain, host_elf, host_boot_user, Rights::ALL, 0).map_err(|_| "failed to load wasi_host")?;

	// StorageService: the ramdisk volume and its service channel. file_picker: the
	// trusted StorageService client and its own service channel. wasi_host: only the
	// picker client - no filesystem access of its own.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;
	send_cap(&picker_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&picker_boot_kernel, b"SERVE", picker_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"PICKER", picker_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	Ok((expected, result.bytes))
}

// Build the M38 permission topology and run it to completion. A StorageService serves
// the ramdisk volume; a ProcessService is the loading mechanism; a TimeService serves the
// wall clock; the permission_manager (PermissionManager) is given the clients it may grant
// onward - a duplicable StorageService client, a duplicable (but dead-peer) LogService
// client, and a TimeService client - plus a NetworkService client it holds but is NOT to
// grant, a ProcessService client it drives to load components, and the channel its clients
// reach it on. PermissionManager governs four components through ProcessService, each under
// a typed permission manifest. Two are report-back probes: sandbox_probe (granted storage and
// log but not network - it transfers exactly those two clients and withholds the network one)
// and request_probe (granted only log, which then asks for an undeclared capability - storage
// - at runtime), recording every decision. The other two it launches on demand through its
// `run` op (the launcher / granter path), each printing to a captured stdout: `date` (granted
// only time) renders the wall clock, and `cat` (granted only storage) prints a file. Each
// sandboxed component reaches only its granted capabilities: sandbox_probe reads its one
// granted file vol://system/hello.txt through the storage grant and reports the bytes back;
// `date` reads the wall clock through the time grant and prints the rendered instant to its
// captured stdout; request_probe's runtime request is refused by the headless policy default
// (least privilege - an undeclared capability is never granted) and recorded as a dynamic
// denial; and `cat` prints that file through its storage grant to the forwarded stdout. The
// kernel only brokers the initial capabilities. Returns (expected,
// probe_read, probe_summary, date_read, date_summary, request_read, request_summary,
// cat_read): the file straight from the volume, then each component's proof and decisions
// summary, then the bytes `cat` printed through the run launcher.
#[cfg(test)]
fn run_permission_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let expected = volume_file(volume, b"hello.txt")?;
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let process_elf = package.lookup(b"process_service").ok_or("process_service missing from the init package")?;
	let time_elf = package.lookup(b"time_service").ok_or("time_service missing from the init package")?;
	let pm_elf = package.lookup(b"permission_manager").ok_or("permission_manager missing from the init package")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (time_boot_kernel, time_boot_user) = Channel::create();
	let (pm_boot_kernel, pm_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();
	let (time_server, time_client) = Channel::create();
	let (perm_server, _perm_client) = Channel::create();
	// The manager's log grant: a real, duplicable client whose service peer is dropped, so
	// the sandboxed probe's best-effort log emit fails fast instead of blocking (no
	// LogService runs in this scenario). The capability is still granted and audited.
	let (log_server, log_client) = Channel::create();
	core::mem::drop(log_server);
	// The manager's network capability: held, but never granted to the probe.
	let (_net_server, net_client) = Channel::create();
	// TimeService's own network client: a real, dead-peer client whose service peer is
	// dropped, so its best-effort SNTP discipline fails fast (PeerClosed) instead of
	// blocking on a reply that never comes (no NetworkService runs in this scenario). It
	// still serves the RTC-seeded wall clock to the governed `date` command.
	let (time_net_server, time_net_client) = Channel::create();
	core::mem::drop(time_net_server);
	// The manager's config / device / audio / resource capabilities: real, dead-peer clients (no
	// such services run in this scenario), held but never granted to the governed components here.
	let (config_server, config_client) = Channel::create();
	core::mem::drop(config_server);
	let (device_server, device_client) = Channel::create();
	core::mem::drop(device_server);
	let (audio_server, audio_client) = Channel::create();
	core::mem::drop(audio_server);
	let (resource_server, resource_client) = Channel::create();
	core::mem::drop(resource_server);
	// The manager's grantable process capability: a real, dead-peer ProcessService connection
	// (distinct from the live ProcessService it drives as the launch mechanism below), held but
	// never granted to the governed components here.
	let (process_grant_server, process_grant_client) = Channel::create();
	core::mem::drop(process_grant_server);
	// The manager's grantable supervisor capability: a real, dead-peer ServiceManager admin
	// channel, held but never granted to the governed components here (the `stop` command,
	// which would receive a narrowed copy, is not among them).
	let (supervisor_server, supervisor_client) = Channel::create();
	core::mem::drop(supervisor_server);

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), process_elf, process_boot_user, Rights::ALL, 0).map_err(|_| "failed to load ProcessService")?;
	loader::spawn_elf_process(domain.clone(), time_elf, time_boot_user, Rights::ALL, 0).map_err(|_| "failed to load TimeService")?;
	loader::spawn_elf_process(domain, pm_elf, pm_boot_user, Rights::ALL, 0).map_err(|_| "failed to load PermissionManager")?;

	// StorageService: the ramdisk volume and its service channel.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;

	// ProcessService: the init package (to load the probe from) and its service channel.
	// PermissionManager drives it to load the component it governs - the loading mechanism,
	// kept separate from the granting policy.
	send_package(&process_boot_kernel, init)?;
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL)?;

	// TimeService: its (dead-peer) network client and its service channel. It seeds its
	// wall clock from the RTC and serves it; the governed `date` command reads it through
	// the grant PermissionManager hands on.
	send_cap(&time_boot_kernel, b"NET", time_net_client, Rights::ALL)?;
	send_cap(&time_boot_kernel, b"SERVE", time_server, Rights::ALL)?;

	// PermissionManager: the grantable clients (storage + log, both duplicable, and time, plus
	// dead-peer config / device / audio / resource / process-grant / supervisor it holds but does
	// not grant here), a network client it withholds, the ProcessService client it drives to load
	// the components, and the channel its clients reach it on. The order matches PermissionManager's
	// receive order. (The grantable permission capability is not sent: the manager mints that
	// self-connection itself.)
	send_cap(&pm_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"LOG", log_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"NETWORK", net_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"TIME", time_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"CONFIG", config_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"DEVICE", device_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"AUDIO", audio_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"RESOURCE", resource_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS_GRANT", process_grant_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SUPERVISOR", supervisor_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS", process_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SERVE", perm_server, Rights::ALL)?;

	sched::run_until_idle();

	// PermissionManager reports its "online" line, then each governed component's proof and
	// decisions summary: the bytes sandbox_probe read through its one granted storage
	// capability and its summary, the instant `date` printed through its one granted time
	// capability and its summary, then request_probe's verdict on its runtime request for an
	// undeclared capability and its summary (which marks that refused request as dynamic) -
	// exactly which capabilities each component was and was not given - and finally the bytes
	// the on-demand `cat` tool printed through its storage grant to the forwarded stdout.
	let _online = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported nothing")?;
	let probe_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no sandbox read-back")?;
	let probe_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no sandbox decisions summary")?;
	let date_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no date read-back")?;
	let date_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no date decisions summary")?;
	let request_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no dynamic-request verdict")?;
	let request_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no dynamic-request decisions summary")?;
	let cat_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no cat output")?;
	Ok((expected, probe_read.bytes, probe_summary.bytes, date_read.bytes, date_summary.bytes, request_read.bytes, request_summary.bytes, cat_read.bytes))
}

// Build the M41 component topology and run it to completion. A StorageService serves
// the ramdisk volume and a LogService holds the journal; the component_host is given
// exactly two capabilities - a StorageService client and a LogService client - and
// nothing else. It loads a real Wasm component (built by the Rust SDK, served from
// storage as vol://system/app.wasm rather than embedded in the kernel image) and runs
// it: the component's three imports are wired by name to the two services - `read` /
// `write` to StorageService, `log` to LogService - with no ambient authority. The
// component reads its one granted file, upper-cases it, logs the result through
// LogService, writes it back, and returns the count; the host also calls the
// component's float `score` export. The kernel only brokers the initial capabilities.
// Returns (expected, content, logged, score): the upper-cased granted file, the bytes
// the component produced, whether the log grant was reached, and the float result.
#[cfg(test)]
fn run_component_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, bool, i32), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let raw = volume_file(volume, b"hello.txt")?;
	let expected: alloc::vec::Vec<u8> = raw.iter().map(|b: &u8| b.to_ascii_uppercase()).collect();
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let log_elf = package.lookup(b"log_service").ok_or("log_service missing from the init package")?;
	let host_elf = package.lookup(b"component_host").ok_or("component_host missing from the init package")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (log_boot_kernel, log_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (log_server, log_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), log_elf, log_boot_user, Rights::ALL, 0).map_err(|_| "failed to load LogService")?;
	loader::spawn_elf_process(domain, host_elf, host_boot_user, Rights::ALL, 0).map_err(|_| "failed to load component_host")?;

	// StorageService: the ramdisk volume and its service channel. LogService: its
	// service channel. component_host: the StorageService client, then the LogService
	// client - exactly the two capabilities its world is wired to, and nothing else.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;
	send_cap(&log_boot_kernel, b"SERVE", log_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"LOG", log_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	let bytes: alloc::vec::Vec<u8> = result.bytes;
	if bytes.len() < 5 {
		return Err("the host report was too short");
	}
	let logged: bool = bytes[0] != 0;
	let score: i32 = i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
	let content: alloc::vec::Vec<u8> = bytes[5..].to_vec();
	Ok((expected, content, logged, score))
}

// Build the M39 resource topology and run it to completion. The resource_manager
// (ResourceManager) is given the init package (to launch the component it governs from) and
// the channel its clients reach it on - nothing more, since it governs through the kernel's
// resource syscalls, not by brokering service connections. ResourceManager creates a
// bounded sub-Domain, launches its one governed component (resource_probe) into that Domain,
// caps the Domain's memory, drives the probe to fill the budget and be refused once (the
// over-budget allocation is contained to that Domain with RESOURCE_EXHAUSTED rather than
// crashing the probe or the system), then raises the cap at runtime and drives the probe
// into the new headroom. The kernel only charges and enforces the per-Domain budget.
// Returns the manager's budget summary: the pages granted under the cap, the contained
// refusal, and the pages regranted after the runtime raise.
#[cfg(test)]
fn run_resource_scenario() -> Result<alloc::vec::Vec<u8>, &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (_volume, package) = scenario_packages()?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let rm_elf = package.lookup(b"resource_manager").ok_or("resource_manager missing from the init package")?;

	let (rm_boot_kernel, rm_boot_user) = Channel::create();
	let (resource_server, _resource_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain, rm_elf, rm_boot_user, Rights::ALL, 0).map_err(|_| "failed to load ResourceManager")?;

	// ResourceManager: the init package (to launch the probe from) and the channel its
	// clients reach it on. The order matches ResourceManager's receive order: PACKAGE, SERVE.
	send_package(&rm_boot_kernel, init)?;
	send_cap(&rm_boot_kernel, b"SERVE", resource_server, Rights::ALL)?;

	sched::run_until_idle();

	// ResourceManager reports its "online" line, then the budget proof: the pages it granted
	// under the cap, the contained over-budget refusal, and the pages it regranted after
	// raising the budget at runtime.
	let _online = rm_boot_kernel.recv().map_err(|_| "ResourceManager reported nothing")?;
	let summary = rm_boot_kernel.recv().map_err(|_| "ResourceManager reported no budget summary")?;
	Ok(summary.bytes)
}

// Read a file from a vol:// volume by driving the StorageService as the kernel's
// own client (the kernel storage self-test). Spawns the service, hands
// it the ramdisk and a service channel, sends one open request plus an empty quit
// sentinel (so the service exits and the cooperative schedule drains), runs the
// schedule to completion, then receives the reply and reads the returned shared
// buffer through the HHDM. Returns the file's bytes, or an error string.
#[cfg(test)]
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

	// the generated volume.open request - [op u16][corr u32][open-opts] where
	// open-opts = [path: [len u16][utf8]][write u8][create u8] - then an empty quit
	// sentinel, which the service treats as end-of-session and exits on.
	let corr: u32 = 1;
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&1u16.to_le_bytes()); // OP_OPEN
	request.extend_from_slice(&corr.to_le_bytes());
	request.extend_from_slice(&(uri.len() as u16).to_le_bytes());
	request.extend_from_slice(uri);
	request.push(0); // write = false
	request.push(0); // create = false
	service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).map_err(|_| "open request failed")?;
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).map_err(|_| "quit sentinel failed")?;

	sched::run_until_idle();

	let reply = service_client.recv().map_err(|_| "the service sent no reply")?;
	// the generated reply - [corr u32][is_ok u8] then, on ok, the open-result record
	// [file placeholder u32][size u64] with the file capability transferred
	// out-of-band; the handle itself rides reply.caps, not the byte stream.
	if reply.bytes.len() < 5 {
		return Err("malformed reply");
	}
	if reply.bytes[4] != 1 {
		return Err("the service denied or could not find the file");
	}
	if reply.bytes.len() < 17 {
		return Err("malformed reply");
	}
	let size = u64::from_le_bytes([reply.bytes[9], reply.bytes[10], reply.bytes[11], reply.bytes[12], reply.bytes[13], reply.bytes[14], reply.bytes[15], reply.bytes[16]]) as usize;
	let cap = reply.caps.first().ok_or("the service granted no buffer")?;
	let object = cap.object();
	let memory = object.as_any().downcast_ref::<MemoryObject>().ok_or("the granted capability was not a buffer")?;
	Ok(read_from_object(memory, size))
}

// Read `len` bytes out of a MemoryObject's frames through the HHDM (the reverse of
// copy_into_object). The object need not be mapped: its physical frames are read
// directly.
#[cfg(test)]
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

// Statics the fault-probe body records into; read back by the fault-isolation test.
#[cfg(test)]
static FAULT_GOT: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
#[cfg(test)]
static FAULT_KIND: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static FAULT_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Kernel-thread body that drops to ring 3 running the fault-probe program. Before
// entering it opens a MemoryObject - charging its Domain's memory and a handle -
// and deliberately leaves it open, so that tearing the process down (when this
// thread is reaped) is what refunds it. The ring-3 program writes to an unmapped
// address and faults; the kernel records the fault, terminates the process, and
// longjmps back here, where we read the recorded fault and free the user mapping.
#[cfg(test)]
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

// A kernel thread that holds a resource and parks until its Domain is killed. It
// opens a MemoryObject (charged to its Domain) and then yields forever; once its
// Domain is killed, it observes the kill at the next yield and exits, releasing
// the object. Used by the domain-kill test.
#[cfg(test)]
extern "C" fn domain_parker(_arg: u64) {
	let _mo = unsafe { arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, mem::frame::PAGE_SIZE, 0, 0, 0) };
	loop {
		sched::yield_now();
	}
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
fn wait_any_wakes_on_the_ready_handle() {
	use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
	static HB: AtomicU64 = AtomicU64::new(0);
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	static OK: AtomicBool = AtomicBool::new(false);
	// The server blocks in SYS_WAIT_ANY on TWO channels at once. Only the second
	// (hb) ever receives a message, so wait_any must wake, return index 1, and the
	// server then recv's that message - exercising block_on_any and the multi-koid
	// waiter cleanup (the thread is parked under both koids, woken via one, and must
	// leave no stale entry under the other).
	extern "C" fn server(ha: u64) {
		unsafe {
			let hb = HB.load(Ordering::SeqCst);
			let handles = [ha, hb];
			let ret = arch::syscall::invoke(syscall::SYS_WAIT_ANY, handles.as_ptr() as u64, handles.len() as u64, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, hb, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			OK.store(!syscall::sys_is_err(n) && &buf[..n as usize] == b"pong", Ordering::SeqCst);
		}
	}
	extern "C" fn client(ch: u64) {
		unsafe {
			let payload = *b"pong";
			let _ = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, 0);
		}
	}
	let (a0, a1) = object::channel::Channel::create();
	let (b0, b1) = object::channel::Channel::create();
	// Spawn the server with the first channel (its arg is that handle), then install
	// the second channel as a second handle and record its value for the server.
	let server = sched::spawn_with_object(server, a0, object::rights::Rights::ALL, 0);
	let hb = server.handles().lock().insert(object::handle::Capability::new(b0, object::rights::Rights::ALL, 0)).raw();
	HB.store(hb, Ordering::SeqCst);
	sched::spawn_with_object(client, b1, object::rights::Rights::ALL, 0);
	// Hold the first channel's peer open so that handle stays silent (not closed),
	// otherwise its peer-close would make it ready and wait_any could return 0.
	let _keep_a1 = a1;
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 1);
	assert!(OK.load(Ordering::SeqCst));
}

#[cfg(test)]
#[test_case]
fn signal_terminate_wakes_a_blocked_thread() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::thread::ThreadState;
	static RAN: AtomicBool = AtomicBool::new(false);
	static PAST_WAIT: AtomicBool = AtomicBool::new(false);
	// The victim blocks forever in SYS_WAIT on a channel whose peer is held open, so
	// nothing wakes it on its own. Delivering the terminate disposition (mark the
	// process killed + wake its threads, exactly as sys_process_signal(SIG_INT)) must
	// wake the blocked thread, have it observe the kill at the wait's checkpoint, and
	// retire it - never running the code past the wait. This proves a signal reaches a
	// thread blocked on something that would otherwise never become ready.
	extern "C" fn victim(handle: u64) {
		unsafe {
			RAN.store(true, Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_WAIT, handle, 0, 0, 0);
			PAST_WAIT.store(true, Ordering::SeqCst);
		}
	}
	let (a, b) = object::channel::Channel::create();
	let _keep = b; // hold the peer so the channel never becomes ready by itself
	let victim_thread = sched::spawn_with_object(victim, a, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(RAN.load(Ordering::SeqCst), "the victim ran and blocked");
	assert!(!PAST_WAIT.load(Ordering::SeqCst), "the victim is blocked in the wait");
	// The terminate disposition, exactly as sys_process_signal(SIG_INT) applies it.
	let process = victim_thread.process().clone();
	process.terminate();
	for thread in process.live_threads() {
		sched::wake_thread(&thread);
	}
	sched::run_until_idle();
	assert!(!PAST_WAIT.load(Ordering::SeqCst), "the killed thread must retire at the wait, not resume past it");
	assert_eq!(victim_thread.state(), ThreadState::Exited, "the victim thread has exited");
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

// Spawn a userspace service from the init package and hand it the channel its
// clients reach it on ("SERVE"). Returns (boot_kernel, service_client): the report
// channel the kernel reads the service's "online" report on, and the client end the
// kernel-as-client drives the generated bindings over. The shared setup of the
// service integration tests.
#[cfg(test)]
fn spawn_service(name: &[u8]) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>) {
	use object::channel::Channel;
	use object::rights::Rights;
	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = package.lookup(name).expect("service in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn service");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	(boot_kernel, service_client)
}

// Like `spawn_service`, but also hands the service a read-only copy of the init
// package ("PACKAGE" + length) before the serve channel - the bootstrap a service
// that launches programs (ProcessService) needs.
#[cfg(test)]
fn spawn_service_with_package(name: &[u8]) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>) {
	use object::channel::Channel;
	use object::rights::Rights;
	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = package.lookup(name).expect("service in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn service");
	let pkg_obj = object::memory_object::MemoryObject::create(init.len()).expect("memory for the package");
	copy_into_object(&pkg_obj, init);
	let mut pkg_msg = alloc::vec::Vec::new();
	pkg_msg.extend_from_slice(b"PACKAGE");
	pkg_msg.extend_from_slice(&(init.len() as u64).to_le_bytes());
	send_cap(&boot_kernel, &pkg_msg, pkg_obj, Rights::READ | Rights::MAP | Rights::TRANSFER).expect("package bootstrap");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	(boot_kernel, service_client)
}

// Little-endian field readers for decoding the proto reply bytes in the tests.
#[cfg(test)]
fn le_u16(b: &[u8], off: usize) -> u16 {
	u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}
#[cfg(test)]
fn le_u32(b: &[u8], off: usize) -> u32 {
	u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
#[cfg(test)]
fn le_u64(b: &[u8], off: usize) -> u64 {
	u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

#[cfg(test)]
#[test_case]
fn log_service_speaks_generated_bindings() {
	use abi::log::{self, Severity};
	use object::channel::Message;

	// Drive the real userspace LogService as a client over its generated Log
	// bindings: spawn it from the init package, hand it a serve channel, EMIT two
	// records and QUERY them back. The wire is the proto framing - request
	// [op u16][corr u32][args], reply [corr u32][result] - and the proto Entry
	// encoding is byte-for-byte the abi::log record, so we build entries with
	// log::encode and frame them by hand. Everything is pre-queued so the
	// cooperative service drains it in one pass and exits, after which we read its
	// replies (the kernel-as-client pattern).
	let (_boot_kernel, service_client) = spawn_service(b"log_service");

	// EMIT one record: [op = 1 (emit) u16][corr u32][entry bytes].
	let emit = |corr: u32, ts: u64, severity: Severity, source: &[u8], fields: &[(&[u8], &[u8])]| {
		let mut wire = [0u8; 128];
		let n = log::encode(ts, severity, source, fields, &mut wire).expect("encode entry");
		let mut msg = alloc::vec::Vec::new();
		msg.extend_from_slice(&1u16.to_le_bytes());
		msg.extend_from_slice(&corr.to_le_bytes());
		msg.extend_from_slice(&wire[..n]);
		service_client.send(Message::new(msg, alloc::vec::Vec::new(), 0)).expect("emit");
	};
	emit(1, 10, Severity::Info, b"storage_service", &[(b"event" as &[u8], b"online" as &[u8])]);
	emit(2, 11, Severity::Error, b"device_manager", &[(b"code" as &[u8], b"5" as &[u8])]);

	// QUERY all severities: [op = 2 (query) u16][corr u32][query bytes]. The query
	// record is since:option<u64> min-severity:option<severity> source:option<string>
	// limit:u32; all-absent with limit 0 is seven zero bytes.
	let mut q = alloc::vec::Vec::new();
	q.extend_from_slice(&2u16.to_le_bytes());
	q.extend_from_slice(&7u32.to_le_bytes());
	q.extend_from_slice(&[0u8; 7]);
	service_client.send(Message::new(q, alloc::vec::Vec::new(), 0)).expect("query");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// Each emit is a round-trip replying result<unit, error> = [corr u32][ok u8 = 1].
	for corr in [1u32, 2] {
		let reply = service_client.recv().expect("emit reply");
		assert_eq!(reply.bytes.len(), 5, "emit reply is corr + ok");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "emit reply echoes the correlation id");
		assert_eq!(reply.bytes[4], 1, "emit succeeded");
	}

	// The query reply is [corr u32 = 7][ok u8 = 1][count u16 = 2][entry][entry].
	let reply = service_client.recv().expect("query reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 7, "query reply echoes the correlation id");
	assert_eq!(b[4], 1, "query succeeded");
	assert_eq!(le_u16(b, 5), 2, "both records came back");
	// spot-check both entries are present in the structured reply
	assert!(b.windows(b"storage_service".len()).any(|w: &[u8]| w == b"storage_service"), "first entry present");
	assert!(b.windows(b"device_manager".len()).any(|w: &[u8]| w == b"device_manager"), "second entry present");
}

#[cfg(test)]
#[test_case]
fn device_service_lists_devices() {
	use object::channel::Message;

	// Drive the real userspace DeviceService as a client over its generated Device
	// bindings: spawn it, hand it a serve channel, and LIST the devices the kernel
	// discovered on the bus. The wire is the proto framing - request [op u16][corr
	// u32][args], reply [corr u32][result]; `list` takes no args and replies
	// result<list<device-entry>, error>. Everything is pre-queued so the cooperative
	// service drains it in one pass and exits (the kernel-as-client pattern).
	let (boot_kernel, service_client) = spawn_service(b"device_service");

	// LIST: [op = 1 (list) u16][corr u32], no args. Then an empty quit sentinel.
	let corr: u32 = 9;
	let mut req = alloc::vec::Vec::new();
	req.extend_from_slice(&1u16.to_le_bytes());
	req.extend_from_slice(&corr.to_le_bytes());
	service_client.send(Message::new(req, alloc::vec::Vec::new(), 0)).expect("list request");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("DeviceService online report");
	assert_eq!(&online.bytes[..], b"DeviceService: online", "DeviceService reports in");

	// The list reply is [corr u32][ok u8 = 1][count u16][device-entry...], each entry
	// [index u32][kind u8][mmio-len u64]. QEMU exposes the virtio devices the kernel
	// found on the bus, so the count is non-zero and the first entry is index 0.
	let reply = service_client.recv().expect("list reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), corr, "list reply echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	let count = le_u16(b, 5);
	assert!(count >= 1, "at least one device was enumerated");
	assert_eq!(le_u32(b, 7), 0, "the first device is index 0");
}

#[cfg(test)]
#[test_case]
fn input_service_streams_pointer_events() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Drive the real userspace InputService end to end over its generated Input
	// bindings: spawn it from the init package, hand it a SERVE channel, an INPUT
	// raw channel (the one the virtio_input pointer driver would feed), and a FORWARD
	// channel (ConsoleService's pointer sink, which it mirrors raw events to), inject a
	// couple of normalized [x u16][y u16][buttons u8] pointer events the way the
	// driver does, then SUBSCRIBE and read the mapped text-cell events back off the
	// stream. The pointer device is interactive-only, so here the test plays the
	// driver itself by sending raw events on the producer end it keeps.
	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = package.lookup(b"input_service").expect("input_service in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	let (raw_producer, raw_consumer) = Channel::create();
	// ConsoleService's pointer sink: the test keeps the consumer end alive so the forward
	// channel stays open (InputService mirrors each raw event to it), but does not assert
	// on it here - the forwarding path is exercised by the live console.
	let (_forward_drain, forward_input) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn InputService");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	send_cap(&boot_kernel, b"INPUT", raw_consumer, Rights::ALL).expect("input raw bootstrap");
	send_cap(&boot_kernel, b"FORWARD", forward_input, Rights::ALL).expect("forward raw bootstrap");

	// Inject two normalized pointer events as the driver would. The grid is COLS = 80
	// x ROWS = 50 over the 0..0x10000 normalized span, so col = (x * 80) / 0x10000 and
	// row = (y * 50) / 0x10000. x = y = 0x8000 (half span) lands on col 40 / row 25
	// with the left button held; the second event is the top-left corner, no buttons.
	let raw_event = |x: u16, y: u16, buttons: u8| -> Message {
		let mut bytes = alloc::vec::Vec::new();
		bytes.extend_from_slice(&x.to_le_bytes());
		bytes.extend_from_slice(&y.to_le_bytes());
		bytes.push(buttons);
		Message::new(bytes, alloc::vec::Vec::new(), 0)
	};
	raw_producer.send(raw_event(0x8000, 0x8000, 1)).expect("first pointer event");
	raw_producer.send(raw_event(0, 0, 0)).expect("second pointer event");

	// SUBSCRIBE: [op = 1 (subscribe) u16][corr u32], no args.
	let corr: u32 = 7;
	let mut req = alloc::vec::Vec::new();
	req.extend_from_slice(&1u16.to_le_bytes());
	req.extend_from_slice(&corr.to_le_bytes());
	service_client.send(Message::new(req, alloc::vec::Vec::new(), 0)).expect("subscribe request");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("InputService online report");
	assert_eq!(&online.bytes[..], b"InputService: online", "InputService reports in");

	// the subscribe reply is [corr u32] with the stream consumer transferred out of band
	let reply = service_client.recv().expect("subscribe reply");
	assert_eq!(le_u32(&reply.bytes, 0), corr, "subscribe reply echoes the correlation id");
	let cap = reply.caps.first().expect("the stream consumer is transferred");
	let consumer = cap.object().into_any_arc().downcast::<Channel>().expect("the consumer is a channel");

	// each event rides its own framed message [seq u32][col u16][row u16][buttons u8];
	// closing the producer ends the stream, so recv drains to a clean close.
	let mut events = alloc::vec::Vec::new();
	while let Ok(frame) = consumer.recv() {
		let f = &frame.bytes;
		events.push((le_u16(f, 4), le_u16(f, 6), f[8]));
	}
	assert_eq!(events.len(), 2, "both injected pointer events stream back");
	assert_eq!(events[0], (40, 25, 1), "the half-span event maps to the middle cell with the left button");
	assert_eq!(events[1], (0, 0, 0), "the corner event maps to column 0, row 0, no buttons");
}

#[cfg(test)]
#[test_case]
fn process_service_starts_a_program() {
	use object::channel::Message;

	// Drive the real userspace ProcessService over its generated Process bindings:
	// spawn it, hand it the init package (to launch from) and a serve channel, then
	// START a program and LIST it back. The wire is the proto framing - request [op
	// u16][corr u32][args], reply [corr u32][result]; `start` takes a string name and
	// replies result<process-info, error> = [koid u64][name string]. Everything is
	// pre-queued so the cooperative service drains it in one pass and exits.
	let (boot_kernel, service_client) = spawn_service_with_package(b"process_service");

	// START storage_client: [op = 1 u16][corr u32][name: [len u16][utf8]].
	let name: &[u8] = b"storage_client";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&1u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	service_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");

	// LIST: [op = 2 u16][corr u32]. Then an empty quit sentinel.
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&2u16.to_le_bytes());
	list.extend_from_slice(&2u32.to_le_bytes());
	service_client.send(Message::new(list, alloc::vec::Vec::new(), 0)).expect("list request");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("ProcessService online report");
	assert_eq!(&online.bytes[..], b"ProcessService: online", "ProcessService reports in");

	// The start reply is [corr u32 = 1][ok u8 = 1][koid u64][name: [len u16][utf8]].
	let reply = service_client.recv().expect("start reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 1, "start reply echoes the correlation id");
	assert_eq!(b[4], 1, "start succeeded");
	let koid = le_u64(b, 5);
	assert!(koid >= 1, "the started process has a koid");
	let name_len = le_u16(b, 13) as usize;
	assert_eq!(&b[15..15 + name_len], name, "the reply echoes the launched program name");

	// The list reply is [corr u32 = 2][ok u8 = 1][count u16 = 1][process-info].
	let reply = service_client.recv().expect("list reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 2, "list reply echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	assert_eq!(le_u16(b, 5), 1, "the started process is listed");
}

#[cfg(test)]
#[test_case]
fn config_service_serves_the_tree() {
	use object::channel::Message;

	// Drive the real userspace ConfigService over its generated Config bindings:
	// spawn it, hand it a serve channel, GET a seeded node, LIST the tree, SET a new
	// node, and GET it back. The wire is the proto framing - request [op u16][corr
	// u32][args], reply [corr u32][result]; strings are [len u16][utf8].
	let (boot_kernel, service_client) = spawn_service(b"config_service");

	// frame a GET: [op = 1 u16][corr u32][key: [len u16][utf8]].
	let get = |corr: u32, key: &[u8]| -> alloc::vec::Vec<u8> {
		let mut m = alloc::vec::Vec::new();
		m.extend_from_slice(&1u16.to_le_bytes());
		m.extend_from_slice(&corr.to_le_bytes());
		m.extend_from_slice(&(key.len() as u16).to_le_bytes());
		m.extend_from_slice(key);
		m
	};
	service_client.send(Message::new(get(1, b"system.name"), alloc::vec::Vec::new(), 0)).expect("get");

	// LIST: [op = 2 u16][corr u32].
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&2u16.to_le_bytes());
	list.extend_from_slice(&2u32.to_le_bytes());
	service_client.send(Message::new(list, alloc::vec::Vec::new(), 0)).expect("list");

	// SET demo.key = hi: [op = 3 u16][corr u32][config-entry: key string + value string].
	let (k, v): (&[u8], &[u8]) = (b"demo.key", b"hi");
	let mut set = alloc::vec::Vec::new();
	set.extend_from_slice(&3u16.to_le_bytes());
	set.extend_from_slice(&3u32.to_le_bytes());
	set.extend_from_slice(&(k.len() as u16).to_le_bytes());
	set.extend_from_slice(k);
	set.extend_from_slice(&(v.len() as u16).to_le_bytes());
	set.extend_from_slice(v);
	service_client.send(Message::new(set, alloc::vec::Vec::new(), 0)).expect("set");
	service_client.send(Message::new(get(4, b"demo.key"), alloc::vec::Vec::new(), 0)).expect("get-back");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	let online = boot_kernel.recv().expect("ConfigService online report");
	assert_eq!(&online.bytes[..], b"ConfigService: online", "ConfigService reports in");

	// GET reply: [corr u32 = 1][ok u8 = 1][value: [len u16][utf8]].
	let r = service_client.recv().expect("get reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 1, "get echoes the correlation id");
	assert_eq!(b[4], 1, "get succeeded");
	let vlen = le_u16(b, 5) as usize;
	assert_eq!(&b[7..7 + vlen], b"LiberSystem", "system.name is the seeded value");

	// LIST reply: [corr u32 = 2][ok u8 = 1][count u16][entries...].
	let r = service_client.recv().expect("list reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 2, "list echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	assert!(le_u16(b, 5) >= 4, "the seeded tree has nodes");

	// SET reply: [corr u32 = 3][ok u8 = 1].
	let r = service_client.recv().expect("set reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 3, "set echoes the correlation id");
	assert_eq!(b[4], 1, "set succeeded");

	// GET demo.key reply: the value we just set reads back.
	let r = service_client.recv().expect("get-back reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 4, "get-back echoes the correlation id");
	assert_eq!(b[4], 1, "get-back succeeded");
	let vlen = le_u16(b, 5) as usize;
	assert_eq!(&b[7..7 + vlen], b"hi", "the value just set reads back");
}

#[cfg(test)]
#[test_case]
fn init_package_starts_system_manager() {
	// The boot chain, end to end: SystemManager starts from the init package, spawns
	// ServiceManager and delegates the package and the ramdisk to it, and
	// ServiceManager brings up the core services in dependency order - LogService
	// first, then DeviceService, ProcessService, and ConfigService (they depend only
	// on LogService, so they come up right after), then ResourceManager (which also
	// depends only on LogService, so it comes up among them, and in turn launches the
	// component it governs and caps its Domain before reporting in), then DeviceManager,
	// StorageService (handed the disk block channel DeviceManager routes up), the
	// media StorageService (handed the second disk's block channel, mounting it as the
	// writable FAT / exFAT vol://media), the iso StorageService (handed the third disk's
	// block channel, mounting it as the read-only ISO9660 vol://iso), and the udf
	// StorageService (handed the fourth disk's block channel, mounting it as the read-only
	// UDF vol://udf - so four StorageService reports arrive),
	// NetworkService (handed the net driver's frame channel the same way), then
	// PermissionManager (which needs storage and network to grant onward, so it comes up
	// once they are running, and in turn launches its sandboxed component before reporting
	// in), and finally - after every component it observes - SystemGraphService, then the
	// shell (which proves the StorageService round-trip by reading a file with `cat`
	// before it reports in). Every report is relayed up, so the kernel observes the
	// services come up in dependency order, then DeviceManager stopped (ServiceManager
	// exercises the stop path on that service), then the watchdog canary brought up,
	// restarted after a commanded crash and recovered after a missed heartbeat
	// (ServiceManager exercises the restart policy and watchdog), followed by the two
	// managers.
	let (kernel_ep, _koid) = spawn_system_manager().expect("SystemManager should start from the init package");
	sched::run_until_idle();
	let reports: [&[u8]; 24] = [b"LogService: online", b"DeviceService: online", b"ProcessService: online", b"ConfigService: online", b"ResourceManager: online", b"DeviceManager: online", b"StorageService: online", b"StorageService: online", b"StorageService: online", b"StorageService: online", b"NetworkService: online", b"TimeService: online", b"AudioService: online", b"InputService: online", b"PermissionManager: online", b"ConsoleService: online", b"SystemGraphService: online", b"Shell: online", b"DeviceManager: stopped", b"WatchdogProbe: online", b"WatchdogProbe: restarted", b"WatchdogProbe: recovered", b"ServiceManager: online", b"SystemManager: online"];
	for expected in reports {
		let message = kernel_ep.recv().expect("a boot-chain report should arrive");
		assert_eq!(&message.bytes[..], expected, "boot-chain reports must arrive in dependency order");
	}
}

#[cfg(test)]
#[test_case]
fn pty_hosts_a_program() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// The M35i PTY abstraction: a program hosts a terminal it is not the hardware console
	// for. ConsoleService opens a pseudo-terminal on request, spawns a slave program on it,
	// and hands back the master channel; the host drives the slave through the line
	// discipline over that master, exactly as the `script` tool (and a future ssh) does.
	// Here we stand in for the host (and for VT 1's idle shell) and drive a `ptyecho` slave:
	// a line written to the master is cooked by the line discipline, delivered to the slave,
	// echoed back prefixed with "pty:", and forwarded out the master to us.
	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let console_elf = package.lookup(b"console_service").expect("console_service in the init package");

	// ConsoleService's bootstrap channel and the channels its __user_main expects: VT 1's
	// data (CLIENT) + control (CONTROL), a factory per service (FSTORAGE..FNET, unused here
	// since the ptyecho slave needs no services), then GPU (none), POINTER (none) and PACKAGE.
	let (boot_kernel, boot_user) = Channel::create();
	let (vt1_console_a, _vt1_console_b) = Channel::create();
	let (ctl_console, ctl_shell) = Channel::create();
	let (dummy_a, _dummy_b) = Channel::create();

	loader::spawn_elf_process(sched::root_domain(), console_elf, boot_user, Rights::ALL, 0).expect("spawn ConsoleService");

	send_cap(&boot_kernel, b"CLIENT", vt1_console_a, Rights::ALL).expect("CLIENT bootstrap");
	send_cap(&boot_kernel, b"CONTROL", ctl_console, Rights::ALL).expect("CONTROL bootstrap");
	for tag in [&b"FSTORAGE"[..], &b"FLOG"[..], &b"FDEVICE"[..], &b"FPROCESS"[..], &b"FCONFIG"[..], &b"FTIME"[..], &b"FAUDIO"[..], &b"FNET"[..]] {
		send_cap(&boot_kernel, tag, dummy_a.clone(), Rights::ALL).expect("factory bootstrap");
	}
	boot_kernel.send(Message::new(b"GPU".to_vec(), alloc::vec::Vec::new(), 0)).expect("GPU bootstrap");
	boot_kernel.send(Message::new(b"POINTER".to_vec(), alloc::vec::Vec::new(), 0)).expect("POINTER bootstrap");
	let pkg_obj = object::memory_object::MemoryObject::create(init.len()).expect("memory for the package");
	copy_into_object(&pkg_obj, init);
	let mut pkg_msg = alloc::vec::Vec::new();
	pkg_msg.extend_from_slice(b"PACKAGE");
	pkg_msg.extend_from_slice(&(init.len() as u64).to_le_bytes());
	send_cap(&boot_kernel, &pkg_msg, pkg_obj, Rights::READ | Rights::MAP | Rights::TRANSFER).expect("PACKAGE bootstrap");

	// stand in for the shell's PTY_OPEN request: ask the console to host a `ptyecho` slave
	// on a new pty.
	ctl_shell.send(Message::new(b"PTY_OPENptyecho".to_vec(), alloc::vec::Vec::new(), 0)).expect("PTY_OPEN request");

	sched::run_until_idle();

	// the console replies "PTY" + the master channel (the host side of the pty).
	let reply = ctl_shell.recv().expect("a PTY reply should arrive");
	assert_eq!(&reply.bytes[..3], b"PTY", "the console opens the pty");
	let cap = reply.caps.first().expect("the master channel is transferred");
	let master = cap.object().into_any_arc().downcast::<Channel>().expect("the master is a channel");

	// drive the slave: a line through the master is cooked and delivered, the slave echoes
	// it back prefixed, and the prefixed line is forwarded out the master back to us.
	master.send(Message::new(b"hello\n".to_vec(), alloc::vec::Vec::new(), 0)).expect("write to the pty master");
	sched::run_until_idle();

	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = master.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	assert!(captured.windows(b"pty:hello".len()).any(|w| w == b"pty:hello"), "the slave's reply is forwarded back out the master");
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
fn wasi_host_runs_a_component() {
	// The wasi_host (a ring-3 process) loads an embedded Wasm component and runs it
	// on the `wasm` runtime. The component's only import, `liber.read`, is wired by
	// the host to read the one granted file (vol://system/hello.txt) through
	// StorageService into the component's linear memory - a WASI-style world: the
	// component has no other capability and can reach nothing it was not given. The
	// bytes the component read must equal the file straight from the volume, proving
	// a Wasm component performed a capability-gated operation via a host import
	// mapped to a native service.
	let (expected, actual) = run_wasi_scenario().expect("the wasi scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(actual, expected, "the component read the granted file's bytes through the host import");
}

#[cfg(test)]
#[test_case]
fn powerbox_grants_a_picked_file_to_a_component() {
	// A Wasm component with NO filesystem access of its own runs under wasi_host,
	// which holds only a FilePicker client. The component's read import goes through
	// the picker, which (standing in for the user's choice) opens the chosen file
	// over StorageService and hands back exactly that file as a handle<file>
	// capability; the host reads it into the component's memory. The bytes must equal
	// the picked file straight from the volume - the component gained access to
	// exactly one user-picked file, and to nothing else (the powerbox pattern).
	let (expected, actual) = run_powerbox_scenario().expect("the powerbox scenario should run");
	assert!(!expected.is_empty(), "the picked file should not be empty");
	assert_eq!(actual, expected, "the component read the user-picked file through the picker");
}

#[cfg(test)]
#[test_case]
fn permission_manager_sandboxes_a_component() {
	// The PermissionManager governs four components under typed permission manifests. Two are
	// report-back probes. sandbox_probe is granted storage and log but not network: it starts
	// with only its manifest's capabilities - the manager transfers exactly the storage and
	// log clients to it and withholds the network one it holds, recording every decision - and
	// reads its one granted file through the storage capability, reporting the bytes back.
	// request_probe is granted only log and then asks for an undeclared capability (storage)
	// at runtime: the headless policy default refuses it (least privilege) and the manager
	// records that refusal as a dynamic decision. The other two are real system tools the
	// manager launches on demand through its `run` op, each printing to a captured stdout:
	// `date` (granted only time) reaches the wall clock through that one capability and prints
	// the rendered instant, and `cat` (granted only storage) prints its one file argument. The
	// probe's bytes must equal the file straight from the volume (the storage grant is live
	// and reaches exactly that file) and its summary must show storage and log granted and
	// every other capability denied; `date`'s output must be a well-formed ISO-8601 UTC instant
	// (the time grant is live) and its summary must show only time granted and every other
	// capability denied; request_probe's runtime request must be denied and its summary must
	// mark that refusal as dynamic - each component was given exactly its manifest and nothing
	// more. Finally `cat`'s output must equal that file (the storage grant reaches it through
	// the on-demand launcher).
	let (expected, probe_read, probe_summary, date_read, date_summary, request_read, request_summary, cat_read) = run_permission_scenario().expect("the permission scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(probe_read, expected, "the sandboxed component read its one granted file through the storage grant");
	assert_eq!(probe_summary.as_slice(), b"storage=grant log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny", "sandbox_probe was granted exactly its manifest - storage and log - and denied every other capability in the vocabulary");
	// `date` reached its one granted capability: its output is a well-formed ISO-8601 UTC
	// instant "YYYY-MM-DDTHH:MM:SSZ" (the exact moment varies, so check the shape, not the
	// value - its presence proves the time grant is live).
	assert_eq!(date_read.len(), 20, "the date command rendered a 20-byte ISO-8601 UTC instant through its time grant");
	assert_eq!(date_read[4], b'-', "the date instant has a date separator after the year");
	assert_eq!(date_read[7], b'-', "the date instant has a date separator after the month");
	assert_eq!(date_read[10], b'T', "the date instant separates date and time with 'T'");
	assert_eq!(date_read[13], b':', "the date instant has a time separator after the hour");
	assert_eq!(date_read[16], b':', "the date instant has a time separator after the minute");
	assert_eq!(date_read[19], b'Z', "the date instant is UTC, terminated by 'Z'");
	assert_eq!(date_summary.as_slice(), b"storage=deny log=deny network=deny device=deny config=deny time=grant audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny", "date was granted exactly its manifest - time - and denied every other capability in the vocabulary");
	// request_probe asked for storage at runtime - a capability outside its manifest. The
	// headless policy default refused it, so the request comes back denied and its summary
	// carries the static grants followed by the refused runtime request marked `(dynamic)`.
	assert_eq!(request_read.as_slice(), b"storage denied", "request_probe's runtime request for an undeclared capability was refused by the headless policy default");
	assert_eq!(request_summary.as_slice(), b"storage=deny log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny storage=deny(dynamic)", "request_probe was granted exactly its manifest - log - and its runtime storage request was refused and recorded as a dynamic denial");
	// The on-demand `cat` tool, launched through PermissionManager's `run` op under a manifest
	// granting only storage, printed the file it was given through that grant to the stdout the
	// manager forwarded it: the bytes it rendered must equal the file straight from the volume.
	assert_eq!(cat_read, expected, "the cat tool printed its file argument through the storage grant the run launcher gave it, forwarded to the captured stdout");
}

#[cfg(test)]
#[test_case]
fn component_host_runs_an_sdk_component() {
	// component_host (a ring-3 process) loads a real Wasm component - built by the Rust
	// SDK and served from storage as vol://system/app.wasm, not embedded in the kernel
	// image - and runs it. Its three imports are resolved by name and wired to two
	// typed services with no ambient authority: `read` / `write` to StorageService,
	// `log` to LogService. The component reads its one granted file, upper-cases it,
	// logs the result through LogService, writes it back, and returns the count; the
	// host also calls the component's float `score` export. The bytes the component
	// produced must equal the upper-cased granted file (a real SDK component performed a
	// capability-gated filesystem read and transformed it on the interpreter), the log
	// grant must have been reached (the second typed service was wired - no ambient
	// authority), and score(10) must be 17 (the float path on genuine toolchain output).
	let (expected, content, logged, score) = run_component_scenario().expect("the component scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(content, expected, "the component read, transformed, and returned its granted file's bytes through the host imports");
	assert!(logged, "the component reached its LogService grant - the second typed service was wired with no ambient authority");
	assert_eq!(score, 17, "the component's float `score` export computed floor(10 * 1.5 + 2.0) on real toolchain output");
}

#[cfg(test)]
#[test_case]
fn resource_manager_contains_a_domain() {
	// The ResourceManager creates a bounded sub-Domain, launches resource_probe into it, and
	// caps the Domain's memory at four one-page objects above the probe's baseline. It drives
	// the probe to fill the budget (four objects fit) and be refused the fifth - that
	// over-budget allocation fails with RESOURCE_EXHAUSTED, contained to the offending Domain
	// rather than crashing the probe (which survives and answers) or the system. The manager
	// then raises the cap by another four pages at runtime and drives the probe into the new
	// headroom (four more fit). The budget summary must show exactly that: four pages granted
	// under the cap, one contained refusal survived, and four pages regranted after the
	// runtime raise - the kernel enforced the per-Domain budget and the policy adjusted it
	// live.
	let summary = run_resource_scenario().expect("the resource scenario should run");
	assert_eq!(summary.as_slice(), b"granted=4 denied=1 regranted=4", "the kernel enforced the Domain's memory budget, contained the over-budget refusal, and honored the runtime raise");
}

#[cfg(test)]
#[test_case]
fn capability_grants_no_operation_beyond_rights() {
	// Property: a handle grants no operation beyond the rights it carries. Across many random
	// granted-rights sets and random probe rights, a rights-checked lookup succeeds exactly
	// when the probe is a subset of the granted set - never a superset. (Fixed-seed xorshift,
	// so the run is deterministic.)
	use object::handle::HandleTable;
	use object::rights::Rights;
	let mut seed: u64 = 0x5eed_1238_d38a_77c1;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	let mut table = HandleTable::new();
	for _ in 0..512 {
		let granted = Rights::from_bits(next() as u32);
		let probe = Rights::from_bits(next() as u32);
		let h = table.insert_object(TestObject::new(1), granted, 0);
		assert_eq!(table.lookup(h, probe).is_ok(), granted.contains(probe), "a lookup must succeed iff the probe rights are a subset of the granted rights");
		table.close(h).expect("close");
	}
}

#[cfg(test)]
#[test_case]
fn capability_attenuation_only_narrows() {
	// Property: duplicating a capability can only narrow it, never widen it. Across many
	// random grants (carrying the DUPLICATE right) and random requests, duplication succeeds
	// exactly when the request is a subset of the grant, and the derived handle carries
	// exactly the requested rights - no right the original lacked, and none outside the
	// request. There is no path by which a derived capability gains authority.
	use object::handle::HandleTable;
	use object::rights::Rights;
	let mut seed: u64 = 0xabcd_0042_1357_9bdf;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	let mut table = HandleTable::new();
	for _ in 0..512 {
		let granted = Rights::from_bits(next() as u32) | Rights::DUPLICATE;
		let requested = Rights::from_bits(next() as u32);
		let h = table.insert_object(TestObject::new(2), granted, 0);
		match table.duplicate(h, requested) {
			Ok(dup) => {
				// Duplication is allowed only when the request is within the grant...
				assert!(granted.contains(requested), "duplication widened the rights beyond the original");
				// ...and the derived handle carries exactly the requested rights, never more.
				let probe = Rights::from_bits(next() as u32);
				assert_eq!(table.lookup(dup, probe).is_ok(), requested.contains(probe), "the derived capability carries exactly the requested rights");
				table.close(dup).expect("close dup");
			}
			Err(_) => {
				// The grant carries DUPLICATE, so the only reason to refuse is that the request
				// asked for a right outside the grant - widening, which is forbidden.
				assert!(!granted.contains(requested), "duplication refused a request that was within the grant");
			}
		}
		table.close(h).expect("close");
	}
}

#[cfg(test)]
#[test_case]
fn no_ambient_authority_fresh_table_empty() {
	// A newly created handle table holds nothing: a process begins with no ambient authority
	// and can reach only capabilities explicitly handed to it. The table is empty, and every
	// lookup into it - across a wide range of handle values - is rejected as a bad handle.
	use object::handle::{Handle, HandleError, HandleTable};
	use object::rights::Rights;
	let table = HandleTable::new();
	assert_eq!(table.len(), 0, "a fresh handle table must be empty");
	let mut seed: u64 = 0x0f0f_1234_dead_c0de;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	for _ in 0..256 {
		let handle = Handle::from_raw(next());
		assert!(matches!(table.lookup(handle, Rights::NONE), Err(HandleError::BadHandle)), "an empty table must resolve no handle");
	}
}

#[cfg(test)]
#[test_case]
fn syscall_fuzz_rejects_invalid_calls() {
	// Syscall fuzzing: from a ring-0 thread (with its own, empty handle table), drive the
	// syscall boundary with random unknown syscall numbers and random arguments, then known
	// handle syscalls with random (bogus) handle arguments. Every call must be rejected with
	// an error rather than crash the kernel - the boundary validates its inputs, and a caller
	// cannot reach authority it was never handed. The thread completing at all is itself the
	// survival check. (Fixed-seed xorshift, so the run is deterministic.)
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		let mut seed: u64 = 0x1357_2468_face_b00c;
		let mut next = || -> u64 {
			seed ^= seed << 13;
			seed ^= seed >> 7;
			seed ^= seed << 17;
			seed
		};
		unsafe {
			// Unknown syscall numbers (well above the defined range) must be rejected.
			for _ in 0..1024 {
				let num = 10_000 + (next() % 0x00ff_ffff);
				let r = arch::syscall::invoke(num, next(), next(), next(), next());
				assert!(syscall::sys_is_err(r), "an unknown syscall number must return an error");
			}
			// Known handle syscalls with random handle arguments. The fuzz thread's handle
			// table is empty, so every random handle resolves to nothing and is rejected
			// before any user buffer is touched - a bogus capability grants no authority.
			let ops = [syscall::SYS_HANDLE_CLOSE, syscall::SYS_HANDLE_DUPLICATE, syscall::SYS_MEMORY_MAP, syscall::SYS_MEMORY_UNMAP];
			for _ in 0..1024 {
				let op = ops[(next() as usize) % ops.len()];
				let r = arch::syscall::invoke(op, next(), next(), next(), next());
				assert!(syscall::sys_is_err(r), "a syscall on a bogus handle must return an error");
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the syscall fuzz thread did not finish - the kernel did not survive");
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
fn process_counters_track_ipc_and_resources() {
	use object::address_space::AddressSpace;
	use object::domain::Domain;
	use object::process::Process;
	use object::rights::Rights;
	// The per-process observability counters SYS_PROCESS_STATS_GET reads back: a fresh
	// process has done no IPC and holds nothing, recording sends and receives bumps the
	// IPC volume independently, installing handles grows the handle count, and a kill is
	// observable as the FAILED liveness the stats syscall derives.
	let domain = Domain::new(1 << 20, 16, 8);
	let process = Process::new(AddressSpace::kernel(), domain.clone());
	assert_eq!(process.messages_sent(), 0);
	assert_eq!(process.messages_received(), 0);
	assert_eq!(process.handle_count(), 0);
	assert_eq!(process.memory_bytes(), 0, "a kernel process owns no user frames");

	process.record_send();
	process.record_send();
	process.record_recv();
	assert_eq!(process.messages_sent(), 2, "two sends counted");
	assert_eq!(process.messages_received(), 1, "one recv counted");

	process.install(object::event::Event::create(), Rights::ALL, 0);
	process.install(object::event::Event::create(), Rights::ALL, 0);
	assert_eq!(process.handle_count(), 2, "two installed handles");

	// Liveness the stats syscall reports: not killed here, killed after terminate().
	assert!(!process.is_killed(), "a live process is not failed");
	process.terminate();
	assert!(process.is_killed(), "a terminated process reports as failed");
}

#[cfg(test)]
#[test_case]
fn kernel_reads_file_through_storage_service() {
	// The kernel drives the StorageService as its own client, sending one open request
	// and a quit sentinel, then reads the returned shared buffer. The bytes must equal
	// the file straight from the volume archive - a round-trip to a real userspace
	// service.
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
