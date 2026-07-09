#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::tests::test_runner))]
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
#[cfg(test)]
mod tests;

use core::sync::atomic::{AtomicPtr, Ordering};

use bootproto::BootInfo;

// The boot information the loader hands the kernel: the memory map, HHDM offset,
// framebuffer, loaded packages, and ACPI RSDP. Published once at kmain entry and
// read-only afterwards, so the boot-time init steps reach it without threading a
// pointer through every call.
static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());

// The published BootInfo. Only valid after kmain has stored the loader's pointer.
fn boot_info() -> &'static BootInfo {
	let ptr = BOOT_INFO.load(Ordering::Acquire);
	debug_assert!(!ptr.is_null(), "boot info read before it was published");
	unsafe { &*ptr }
}

// Publish a kernel-constructed BootInfo. aarch64 and riscv64 boot directly (no
// bootloader hand-off), so they build their own BootInfo from their boot state and the
// embedded packages and publish it here before driving the userspace boot chain.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn publish_boot_info(bi: &'static BootInfo) {
	BOOT_INFO.store(bi as *const BootInfo as *mut BootInfo, Ordering::Release);
}

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
// Returns how many bytes the serial transmit ring accepted, so a caller carrying a
// backlog knows where to resume instead of losing the tail.
#[doc(hidden)]
pub fn _print_bytes(bytes: &[u8]) -> usize {
	let n = arch::serial::write_bytes(bytes);
	console::write_bytes(&bytes[..n]);
	n
}

// Single-byte twin of _print_bytes, for the legacy single-byte SYS_DEBUG_WRITE form.
#[doc(hidden)]
pub fn _print_byte(byte: u8) {
	_print_bytes(&[byte]);
}

// kernel entry point (ELF entry, see ENTRY(kmain) in the linker script)
#[unsafe(no_mangle)]
unsafe extern "C" fn kmain(boot_info_ptr: *const BootInfo) -> ! {
	arch::serial::init();
	BOOT_INFO.store(boot_info_ptr as *mut BootInfo, Ordering::Release);
	let bi = boot_info();
	assert!(bi.magic == bootproto::MAGIC, "boot protocol magic mismatch: the loader and kernel disagree");
	assert!(bi.version == bootproto::VERSION, "boot protocol version mismatch: rebuild the loader and kernel together");
	serial_println!("{} kernel is starting ...", product::NAME);
	arch::init();
	init_memory();
	init_framebuffer();
	arch::init_interrupts();
	arch::init_tsc();
	arch::enable_interrupts();
	arch::init_syscalls();
	init_smp();
	// The application processors are up (their trampoline ran below 1 MiB on the
	// loader's identity map); drop that identity map now, before any kernel-context
	// user mapping, so a 2 MiB identity page cannot shadow a 4 KiB user page.
	arch::paging::remove_bootstrap_identity();
	sched::init();
	device::init();

	#[cfg(test)]
	test_main();

	#[cfg(not(test))]
	boot_main();

	arch::halt_loop()
}

// Bring up physical frames, paging and the kernel heap from the loader's boot
// info. Runs before the test/boot split so `alloc` is available in tests.
fn init_memory() {
	let bi = boot_info();
	let regions = unsafe { core::slice::from_raw_parts(bi.memmap as *const bootproto::MemRegion, bi.memmap_len as usize) };
	mem::init(regions, bi.hhdm_offset);
}

// Bring up the framebuffer console from the Limine framebuffer response, so the
// kernel log is mirrored to the screen alongside serial. A no-op (serial only) if
// the bootloader provided no framebuffer. Runs before the test/boot split so the
// console is up for both paths; it allocates its grid model (the shared `term`
// stack), so it must run after init_memory brings up the heap.
fn init_framebuffer() {
	let bi = boot_info();
	if bi.fb_present == 0 {
		return;
	}
	let fb = &bi.framebuffer;
	console::init(console::FbInfo { addr: fb.addr as *mut u8, width: fb.width as usize, height: fb.height as usize, pitch: fb.pitch as usize, bytes_per_pixel: fb.bpp as usize / 8, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size });
}

// The boot framebuffer's virtual base + geometry, for the framebuffer_map syscall to
// hand the display to a userspace ConsoleService. Reads the loader's boot info
// (it is 'static), or None if there is no framebuffer (headless / no video mode).
pub fn framebuffer_geometry() -> Option<(u64, abi::Framebuffer)> {
	let bi = boot_info();
	if bi.fb_present == 0 {
		return None;
	}
	let fb = &bi.framebuffer;
	let geom = abi::Framebuffer { width: fb.width, height: fb.height, pitch: fb.pitch, bytes_per_pixel: fb.bpp / 8, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size, _pad: [0; 2] };
	Some((fb.addr, geom))
}

// Wake the application processors and wait for every core to report in. Runs
// before the test/boot split so SMP is up for both paths.
fn init_smp() {
	smp::init(boot_info());
}

#[cfg(not(test))]
fn boot_main() {
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
pub(crate) fn console_shell_loop() {
	// The shell attaches asynchronously: ConsoleService registers its console channel
	// (SYS_CONSOLE_ATTACH) a few scheduler passes after it reports in, so the instant
	// the boot chain settles it may not be attached yet on a slower arch (riscv under
	// TCG emulation). Pump the schedule for a bounded window waiting for it before
	// concluding none is present; on x86/aarch64 it has already attached, so this falls
	// straight through with no wait.
	let mut waited = 0u32;
	while !console_input::shell_listening() {
		if waited >= 300 {
			serial_println!("shell: no interactive shell attached");
			return;
		}
		waited += 1;
		sched::run_until_idle();
		arch::serial::drain_tx();
		arch::idle_halt();
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
		// The system is settled: only no-deadline and periodic waits remain. HALT until
		// the next timer tick or device interrupt instead of spinning - a spinning BSP
		// floods KVM with the serial poll's port-I/O VM-exits (see run_until_idle) - and
		// re-enter, which wakes whatever housekeeping (a display poll, a blink tick)
		// came due in the meantime.
		arch::serial::drain_tx();
		arch::idle_halt();
	}
}

// A loaded package's bytes, located among the loader's modules by name. Returns
// None if the loader passed no module with the given name. The module memory is
// mapped in the HHDM and is 'static for the kernel.
fn module_bytes(name: &str) -> Option<&'static [u8]> {
	let bi = boot_info();
	let modules = unsafe { core::slice::from_raw_parts(bi.modules as *const bootproto::Module, bi.modules_len as usize) };
	for m in modules {
		let end = m.name.iter().position(|&b| b == 0).unwrap_or(m.name.len());
		if &m.name[..end] == name.as_bytes() {
			return Some(unsafe { core::slice::from_raw_parts(m.addr as *const u8, m.size as usize) });
		}
	}
	None
}

// The init package bytes (the first userspace programs the kernel ELF-loads).
fn init_package_bytes() -> Option<&'static [u8]> {
	module_bytes(product::INIT_PACKAGE)
}

// The ramdisk volume package bytes.
fn volume_package_bytes() -> Option<&'static [u8]> {
	module_bytes(product::VOLUME_PACKAGE)
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

	// Tell the boot chain which kind of boot this is: "MODE" + one byte, 1 in a test
	// build and 0 in a production one. ServiceManager runs its bring-up self-tests
	// (the stop-path exercise and the canary crash / hang drills) only in a test boot,
	// so a production system never deliberately faults a process or stops a service.
	let mode: u8 = if cfg!(test) { 1 } else { 0 };
	kernel_ep.send(Message::new(alloc::vec![b'M', b'O', b'D', b'E', mode], alloc::vec::Vec::new(), 0)).map_err(|_| "failed to hand SystemManager the boot mode")?;
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

// Serial receive interrupt: drain the UART FIFO into the console input the moment
// bytes arrive, so typed input wakes the shell immediately instead of waiting for
// the next 100 Hz idle-hook poll (the poll stays as a fallback and for the first-
// prompt nudge). Runs on the BSP (the UART's legacy IRQ is routed there); the
// channel send inside feed() wakes the shell's waiter on this same core.
#[cfg(not(test))]
fn serial_rx_interrupt(_vector: u8) {
	while let Some(byte) = arch::serial::read_byte() {
		console_input::feed(byte);
	}
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
	// Serial input goes interrupt-driven: route the UART's legacy IRQ (COM1 = ISA
	// IRQ 4) to the BSP and enable the receive interrupt, so a typed byte reaches
	// the shell at once rather than on the next tick-quantized poll.
	arch::interrupts::register(arch::interrupts::IRQ_BASE + 4, serial_rx_interrupt);
	arch::ioapic::route(4, arch::interrupts::IRQ_BASE + 4, smp::lapic_id(0));
	arch::serial::enable_rx_irq();
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
