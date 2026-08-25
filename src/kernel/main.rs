#![no_std]
#![no_main]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]
// `Arc::try_new`, for `heap::try_arc`. Almost every kernel object is an `Arc` and almost every one
// of them is built on a syscall path, so "an allocation ring 3 can trigger must be able to refuse"
// is not a rule this kernel can keep without it.
#![feature(allocator_api)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::tests::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

mod arch;
mod console;
mod console_input;
mod device;
mod dma_policy;
mod elf;
mod extable;
mod fault;
#[cfg(test)]
mod graph;
mod iommu;
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
    // ONE call, not two. Printing the line and then the newline separately hands another core a
    // gap between them, and a log where the newlines belong to the wrong lines is the same defect
    // as a garbled line. Nesting `format_args!` costs nothing: no intermediate string exists.
    ($($arg:tt)*) => {{
        $crate::_print(core::format_args!("{}\n", core::format_args!($($arg)*)));
    }};
}

// Serializes a whole log LINE between cores, which is what "the corrupted serial output on
// aarch64" turned out to be.
//
// `core::write!` calls `write_str` once per FORMATTING FRAGMENT, so `serial_println!("a={a} b={b}")`
// reaches the port as five separate writes. Nothing stood between them, so two cores printing at
// once produced a line belonging to neither - `caught_before=1837` where the counter was in single
// digits, and `child=<mixed>100000001`. Those were read as memory corruption for four measured
// cycles of this milestone; they are two cores' digits landing in one line, and nothing was wrong
// with the values at all.
//
// It is worst on the device-tree targets because their consoles are polled and take NO lock -
// aarch64's PL011 and riscv64's NS16550 mix at BYTE granularity - while x86_64 locks its TX ring
// per slice and could only ever mix at a fragment boundary. That difference is exactly the shape of
// the evidence: x86_64 never showed it, and both emulated targets did.
//
// Here rather than in each backend because the fragments are what has to be held together, and only
// this side can see them.
static PRINT: crate::sync::SpinLock<()> = crate::sync::SpinLock::new(());

// How long a core waits for the print lock before writing anyway.
//
// A CONSOLE THAT CAN DEADLOCK IS WORSE THAN ONE THAT CAN INTERLEAVE, and this kernel has the
// failure to prove it: a core that halts, panics or faults while holding this lock would silence
// every other core forever, turning a diagnosable panic into the silent hang that has cost this
// milestone the most. The same bound covers re-entry - a fault handler printing from inside a print
// on this very core cannot be granted a lock this core already holds, and must not wait for it.
// Past the bound the line goes out unserialized, which is the behaviour that existed before.
const PRINT_SPINS: u32 = 200_000;

// Take the print lock if it comes free within the bound, otherwise `None` and the caller prints
// anyway. `try_lock` in a loop rather than `lock`, because `lock` cannot give up.
fn print_lock() -> Option<crate::sync::SpinLockGuard<'static, ()>> {
	for _ in 0..PRINT_SPINS {
		// Test before test-and-set: `try_lock` masks and restores interrupts on every failed
		// attempt, which is far too much to pay two hundred thousand times while another core
		// finishes a line.
		if !PRINT.is_locked() {
			if let Some(guard) = PRINT.try_lock() {
				return Some(guard);
			}
		}
		core::hint::spin_loop();
	}
	None
}

// Write formatted output to the serial port (always) and mirror it to the
// framebuffer console (if one is initialized). Backs serial_print!/serial_println!
// so every log line reaches both sinks. Hidden from docs; call via the macros.
#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments<'_>) {
	use core::fmt::Write as _;
	let _guard = print_lock();
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
	// The same lock as `_print`, so a screenful pushed through SYS_DEBUG_WRITE does not land in the
	// middle of a kernel log line - the two sinks are the same wire.
	let _guard = print_lock();
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
//
// x86_64 ONLY, AND SAID SO IN THE TYPE-CHECKED TREE. This is the UEFI loader hand-off: the loader
// builds page tables and a `BootInfo` and jumps here. The other two ports enter their own prologue -
// `aarch64::boot::aarch64_main`, `riscv64::boot::riscv64_main` - and bring up their console, page
// tables, per-CPU register, interrupt controller, timer, syscall vector and secondary cores there.
// While this function compiled everywhere, every step it calls was a symbol the other backends had
// to define, and they defined them as `todo!()` bodies nothing could reach. A reader - and a static
// scan - could only read that as an unfinished port.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
unsafe extern "C" fn kmain(boot_info_ptr: *const BootInfo) -> ! {
	arch::serial::init();
	BOOT_INFO.store(boot_info_ptr as *mut BootInfo, Ordering::Release);
	let bi = boot_info();
	assert!(bi.magic == bootproto::MAGIC, "boot protocol magic mismatch: the loader and kernel disagree");
	assert!(bi.version == bootproto::VERSION, "boot protocol version mismatch: rebuild the loader and kernel together");
	serial_println!("{} kernel is starting ...", product::NAME);
	arch::init();
	// Named after `arch::init` rather than before it, because that is where a backend learns
	// where to ask. x86 reads the profile off a fixed I/O port and could answer at any moment,
	// but aarch64 and riscv64 read it over MMIO at an address the device tree names, and the
	// tree is parsed in `init` - asking earlier there is asking address zero.
	//
	// An ordinary boot prints nothing here, so a development instance is never mistaken for one
	// and no production boot carries the line.
	if let Some(profile) = arch::boot_profile() {
		serial_println!("{} boot profile: {}", product::NAME, profile);
	}
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
	// AFTER `device::init` RETURNS, not inside it. The bring-up reads the device table, and
	// `device::init` is holding that table's lock while it fills it - a spin lock taken twice on one
	// core is a boot that stops with no message at all, which is exactly what this did.
	dma_policy::init();

	#[cfg(test)]
	test_main();

	#[cfg(not(test))]
	boot_main();

	arch::halt_loop()
}

// Bring up physical frames, paging and the kernel heap from the loader's boot
// info. Runs before the test/boot split so `alloc` is available in tests.
// Reached from `kmain` alone: the other two prologues carve their own memory from the device tree
// before they have a `BootInfo` to read.
#[cfg(target_arch = "x86_64")]
fn init_memory() {
	let bi = boot_info();
	let regions = unsafe { core::slice::from_raw_parts(bi.memmap as *const bootproto::MemRegion, bi.memmap_len as usize) };
	mem::init(regions, bi.hhdm_offset);
}

// Bring up the framebuffer console from the loader's boot framebuffer, so the
// kernel log is mirrored to the screen alongside serial. A no-op (serial only) if
// the bootloader provided no framebuffer. Runs before the test/boot split so the
// console is up for both paths; it allocates its grid model (the shared `term`
// stack), so it must run after init_memory brings up the heap.
#[cfg(target_arch = "x86_64")]
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
//
// The x86 wake sequence, so x86_64 only: it enumerates local APICs from the ACPI MADT and drives
// INIT-SIPI-SIPI through a real-mode trampoline. aarch64 wakes its secondaries with PSCI `CPU_ON`
// and riscv64 with SBI HSM `hart_start`, each from its own prologue.
#[cfg(target_arch = "x86_64")]
fn init_smp() {
	smp::init(boot_info());
	#[cfg(all(test, target_arch = "x86_64"))]
	serial_println!("smp: {} of {} cores online", smp::online_count(), smp::cpu_count());
}

// x86_64's own boot tail: it calls the portable `boot_userspace` that the other two prologues call
// directly. See `arch/mod.rs` for which half of the boot is shared and which is each port's own.
#[cfg(all(not(test), target_arch = "x86_64"))]
fn boot_main() {
	serial_println!("arch: {}", arch::NAME);
	// Said at BOOT, once, where an operator will see it - not left for whoever eventually derives a
	// key from a number the clock could have told them.
	//
	// A machine with no hardware random source cannot answer `SYS_RANDOM_GET` at all: it refuses,
	// rather than handing out a formula under a name that promises a key. Everything still runs -
	// nothing in this system needs a secret yet - and the day something does, this line is what
	// says the machine cannot provide one.
	if !arch::random::secure_available() {
		serial_println!("random: WARNING: no hardware random source on this machine - SYS_RANDOM_GET will refuse, and nothing here can produce a key or a token");
	}
	serial_println!("smp: {} of {} cores online", smp::online_count(), smp::cpu_count());
	serial_println!("memory: {} physical frames free", mem::frame::free_count());
	// The pages this boot has handed back and been unable to record. Zero here on any healthy
	// machine, and the point is that it is PRINTED rather than only counted: the run table is
	// bounded, so under fragmentation a free can be dropped, and the machine then gets slowly
	// smaller with nothing adding it up. The symptom otherwise arrives weeks later as an
	// allocation failure with no cause attached. Printed unconditionally so a boot log always
	// carries the baseline, which is what makes a later non-zero one worth reading.
	// RETIRED AND LOST ARE DIFFERENT NEWS, so they are reported apart.
	//
	// A retired page is one this kernel deliberately took out of circulation - a shootdown that did
	// not complete, a span outside the buddy's extent - and marked as such: the machine knows it is
	// not using it. A lost page is that plus anything else that left circulation without a record.
	// A machine accumulating retirements is reporting a SHOOTDOWN problem, and losing the page is
	// only its receipt; one number for both said "memory problem" for either.
	serial_println!("memory: {} page(s) retired for good, {} page(s) lost in all, {} free(s) refused", mem::frame::retired_pages(), mem::frame::lost_pages(), mem::frame::refused_frees());
	// Perf-trace anchor: publish the calibrated TSC frequency so the host trace tool can
	// convert the ring-3 `\x1ePERF` cycle markers to wall-clock time.
	serial_println!("\x1ePERF tsc_hz {}", arch::tsc::hz());
	// WHAT IS ACTUALLY TRUE AT THIS POINT. The line said "entering the userspace shell" and was
	// followed by every driver binding, every service starting and the product banner before a
	// prompt appeared - so the one line a reader takes as "the boot finished" was printed in the
	// middle of it. The kernel IS up here, which is the fact worth a line; the shell says so itself
	// when it is.
	//
	// `boot OK` is kept verbatim at the front: `screenshot.sh` waits for it and `perf-trace.py`
	// anchors on it, and both are looking for exactly this moment.
	serial_println!("boot OK - kernel is up, starting userspace");
	// Serial input goes interrupt-driven HERE, in this port's prologue, because it is the machine
	// and not the policy: route the UART's legacy IRQ (COM1 = ISA IRQ 4) to the BSP and enable the
	// receive interrupt, so a typed byte reaches the shell at once rather than on the next
	// tick-quantized poll. The other two ports poll their UART and arrange nothing here.
	arch::interrupts::register(arch::interrupts::IRQ_BASE as u32 + 4, serial_rx_interrupt);
	arch::ioapic::route(4, arch::interrupts::IRQ_BASE + 4, smp::lapic_id(0));
	arch::serial::enable_rx_irq();
	// THREE HUNDRED ROUNDS, which is what `console_shell_loop` used to wait on this port before the
	// wait moved inside the supervised attempt.
	boot_userspace(300);
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
	// THE CONTROL PLANE HAS NO OWNER, SO THE MACHINE DOES NOT KEEP RUNNING AS THOUGH IT HAS ONE.
	//
	// Checked here because this hook runs on every idle pass whatever the shell is doing, and a
	// dead SystemManager is not something to discover the next time somebody types. There is no
	// in-place resurrection: bringing back a manager whose branch is still full of the processes
	// it owned would be a second manager beside an orphan, which is worse than a reboot.
	if resident_manager_lost() {
		serial_println!("recovery: SystemManager ended after the system was up - the control plane has no owner, rebooting");
		arch::reset();
	}
	static NUDGED: AtomicBool = AtomicBool::new(false);
	if !NUDGED.load(Ordering::Relaxed) && console_input::shell_listening() {
		NUDGED.store(true, Ordering::Relaxed);
		console_input::feed_serial(b'\n');
	}
	// Drain the whole serial RX FIFO each wake: the BSP now halts between idle passes
	// (~100 Hz timer wakes) instead of busy-spinning, so polling one byte per pass could
	// let a fast paste overrun the 16-byte UART FIFO. Reading until empty keeps serial
	// input lossless at the lower poll rate.
	while let Some(byte) = arch::serial::read_byte() {
		console_input::feed_serial(byte);
	}
}

// Drive the interactive userspace shell. The boot chain has already started it as
// its last component and the shell has registered a console channel; this pumps
// serial keystrokes to it a byte at a time, running the cooperative schedule after
// each so the shell (and any service it calls) makes progress. Returns when the
// shell exits (the user typed `exit`) or never attached.
#[cfg(not(test))]
pub(crate) fn console_shell_loop() {
	// NO WAIT HERE ANY MORE. This used to pump the schedule for three hundred rounds waiting for
	// ConsoleService to register its channel - after the recovery ladder had already reported the
	// boot a success, so a chain that never came up was a boot the kernel had called good and then
	// printed "no interactive shell attached" over. The wait belongs to the attempt, and
	// `supervise` does not return true until the shell is listening.
	// Nudge the shell to print its first prompt, then pump both input sources until
	// it exits. Each round forwards any waiting serial byte and runs the cooperative
	// schedule, so threads a device interrupt woke also make progress: the
	// virtio-input keyboard driver feeds console input from its own IRQ handler, so
	// the shell must be pumped whenever an interrupt arrives, not only when a serial
	// byte does. Polling serial (rather than blocking on it) keeps that interrupt
	// path live while no one is typing on the wire.
	// AND THE HINT, WHERE THE SHELL ACTUALLY IS. It used to ride on the kernel's `boot OK` line,
	// thirty lines and a whole userspace bring-up before anything could read a command.
	serial_println!("shell attached - type 'help', or 'exit' to halt");
	console_input::feed_serial(b'\n');
	while console_input::shell_listening() {
		if let Some(byte) = arch::serial::read_byte() {
			if !console_input::feed_serial(byte) {
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
			// A MODULE WITH NO BYTES IS NOT A MODULE. A named entry of length zero is a slot that
			// was filled in because the array had room, not something that was handed over - and
			// every caller here treats `Some` as "this arrived". The live-volume lookup below is
			// the one that showed it: an empty `system-volume.img` entry sent the storage bootstrap
			// down the LIVEVOL path with nothing to mount, and the boot chain stopped after seven
			// of its twenty-three services.
			if m.size == 0 {
				return None;
			}
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
// THE RESIDENT MANAGER, WATCHED AFTER THE SYSTEM IS UP.
//
// The recovery ladder below supervises SystemManager only until the system comes up: `supervise`
// returns on the first round that completes without a fault and stops looking. That was right while
// SystemManager relayed the boot reports and exited, because after that there was nothing to watch.
// It stays resident now and owns the control-plane branch, so its LATER death leaves that branch
// with no owner - and nothing would have noticed.
//
// The crash channel alone is not enough either: it reports FAULTS, and a manager that returns
// cleanly is just as gone. So the process itself is watched, and any ending - fault or clean exit -
// after the system is up reaches the same place a lost control plane has to reach.
static RESIDENT_MANAGER: crate::sync::SpinLock<Option<alloc::sync::Arc<object::process::Process>>> = crate::sync::SpinLock::new(None);

// Set once the pre-online ladder has finished and the system is up. BEFORE this point a
// SystemManager that ends is the ladder's business and a restart is the right answer; after it,
// the same ending means the control plane is ownerless and the only honest answer is a reboot. One
// flag separates the two, and nothing clears it.
#[cfg(not(test))]
static SYSTEM_IS_UP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Whether the control plane has lost its owner.
//
// TAKEN AS ARGUMENTS RATHER THAN READ FROM STATICS, so the decision can be tested with a process
// that really has ended instead of by reasoning about it. The three cases it separates are the
// whole of the rule:
//
//   before the system is up   the pre-online ladder owns the outcome, and a manager that ends
//                             there is restarted rather than mourned
//   after, still alive        nothing to do
//   after, ended              the branch below has no owner and the machine reboots
//
// "Ended" and not "faulted": a manager that returns or calls `exit` cleanly is exactly as gone as
// one that faulted, and the crash channel only reports the second.
pub(crate) fn control_plane_lost(system_up: bool, manager: Option<&alloc::sync::Arc<object::process::Process>>) -> bool {
	system_up && manager.is_some_and(|process| process.is_terminated())
}

#[cfg(not(test))]
fn resident_manager_lost() -> bool {
	let manager = RESIDENT_MANAGER.lock();
	control_plane_lost(SYSTEM_IS_UP.load(core::sync::atomic::Ordering::Relaxed), manager.as_ref())
}

fn spawn_system_manager() -> Result<(alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::process::Process>), &'static str> {
	use alloc::sync::Arc;
	use object::KernelObject;
	use object::channel::Message;
	use object::handle::Capability;
	use object::memory_object::MemoryObject;
	use object::privilege::{Privilege, PrivilegeKind};
	use object::rights::Rights;

	let bytes = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(bytes).ok_or("init package is malformed")?;
	let elf_image = package.lookup(b"system_manager.lsexe").ok_or("system_manager.lsexe missing from init package")?;
	let (kernel_ep, user_ep) = object::channel::Channel::create();
	let process = loader::spawn_elf_process(sched::root_domain(), elf_image, user_ep, Rights::ALL).map_err(|_| "failed to load SystemManager")?;
	// The one process nothing else can name. The loader takes a name from a staged image's
	// identity note, and the static init-package programs carry none; every other one of them
	// is launched through ProcessService, which labels it from the package entry it was looked
	// up by. This one the kernel launches itself, so the kernel labels it.
	process.header().set_name("system_manager");
	// Kept so the ending can be seen. Replaced on each attempt of the pre-online ladder, so what is
	// watched afterwards is the instance that actually came up.
	*RESIDENT_MANAGER.lock() = Some(process.clone());

	// Hand SystemManager the init package as a read-only shared buffer: the kernel
	// copies the package bytes into a MemoryObject and sends "PACKAGE" + length
	// with that capability, so SystemManager can find and spawn ServiceManager and
	// then delegate the package onward to it (TRANSFER) to start the rest. DUPLICATE
	// lets ServiceManager share it further (with DeviceManager, which spawns drivers
	// from it) without giving up its own handle.
	let package_obj = MemoryObject::create(bytes.len()).ok_or("no memory for the init package")?;
	copy_into_object(&package_obj, bytes);
	// ALLOC-OK: boot, handing SystemManager its bootstrap; nothing else is running
	let mut msg = alloc::vec::Vec::with_capacity(7 + 8);
	// ALLOC-OK: the boot chain's own hand-off message, built before userspace exists.
	msg.extend_from_slice(b"PACKAGE");
	// ALLOC-OK: the boot chain's own hand-off message, built before userspace exists.
	msg.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
	let cap = Capability::new(package_obj as Arc<dyn KernelObject>, Rights::READ | Rights::MAP | Rights::TRANSFER | Rights::DUPLICATE);
	// ALLOC-OK: boot, as above
	kernel_ep.send(Message::new(msg, alloc::vec![cap])).map_err(|_| "failed to hand SystemManager the init package")?;

	// Hand SystemManager the ramdisk volume the same way, so it can be delegated
	// down to the StorageService the boot chain brings up. "RAMDISK" + length with a
	// read-only buffer capability the StorageService will map and serve files from.
	// A LIVE medium carries a whole filesystem here instead of an archive: the loader read
	// `system-volume.img` off the boot medium and handed it over as a module, and the running
	// system copies it into memory because the medium it booted from cannot be written. The tag
	// says which of the two arrived, so the storage service knows whether to unpack an archive or
	// mount a volume.
	let (volume, tag): (&[u8], &[u8]) = match module_bytes(crate::product::SYSTEM_VOLUME) {
		Some(image) => (image, b"LIVEVOL"),
		// An INSTALLED system has neither: its volume is a partition the storage service mounts
		// off the disk, so there is no archive to seed from and no image to copy. The message is
		// still sent, empty, because this bootstrap carries its length implicitly at both ends and
		// every message after it is positional - leaving it out stops the chain at SystemManager's
		// next read. ServiceManager drops this capability on the disk path in any case.
		//
		// Refusing here instead is what broke the disk image: it boots its kernel and its
		// bootstrap set from the volume, which is exactly what this milestone set out to do, and
		// then died for the absence of the artifact that work removed.
		None => (volume_package_bytes().unwrap_or(&[]), b"RAMDISK"),
	};
	let ramdisk = MemoryObject::create(volume.len().max(1)).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);
	// ALLOC-OK: boot, as above
	let mut rdmsg = alloc::vec::Vec::with_capacity(7 + 8);
	// ALLOC-OK: the boot chain's own hand-off message, built before userspace exists.
	rdmsg.extend_from_slice(tag);
	// ALLOC-OK: the boot chain's own hand-off message, built before userspace exists.
	rdmsg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	let rdcap = Capability::new(ramdisk as Arc<dyn KernelObject>, Rights::READ | Rights::MAP | Rights::TRANSFER);
	// ALLOC-OK: boot, as above
	kernel_ep.send(Message::new(rdmsg, alloc::vec![rdcap])).map_err(|_| "failed to hand SystemManager the ramdisk")?;

	// Hand SystemManager the power capability: a handle to the root Domain carrying MANAGE,
	// which is what `SYS_SYSTEM_POWER` checks. Stopping the machine used to need no capability
	// at all, so every process in the system had it; it now travels the boot chain like any
	// other authority - and travels exactly one hop, to the one process that keeps it.
	//
	// MANAGE ALONE, AND THAT IS THE WHOLE GRANT. It carried TRANSFER and DUPLICATE while it was
	// delegated onward - ServiceManager to DeviceManager to two keyboard drivers - and since M11
	// it is not delegated at all: SystemManager stays resident and holds it, and what goes down
	// the chain in its place is a client of the narrow SystemPower service, which can ask for a
	// reboot and can do nothing else. Rights nobody exercises are rights that only widen what a
	// mistake can reach.
	//
	// Sent AFTER the ramdisk because that is the order SystemManager reads its handoffs in,
	// and a bootstrap read consumes whatever arrived: out of order, its RAMDISK read takes
	// this message instead and the whole boot chain stops before the first service starts.
	let power_cap = Capability::new(sched::root_domain() as Arc<dyn KernelObject>, Rights::MANAGE);
	// ALLOC-OK: boot, as above
	// ALLOC-OK: boot, before userspace exists - a four-byte tag on the handover path.
	kernel_ep.send(Message::new(b"POWER".to_vec(), alloc::vec![power_cap])).map_err(|_| "failed to hand SystemManager the power capability")?;

	// Tell the boot chain which kind of boot this is: "MODE" + one byte, 1 in a test
	// build and 0 in a production one. ServiceManager runs its bring-up self-tests
	// (the stop-path exercise and the canary crash / hang drills) only in a test boot,
	// so a production system never deliberately faults a process or stops a service.
	let mode: u8 = if cfg!(test) { 1 } else { 0 };
	// ALLOC-OK: boot, as above
	kernel_ep.send(Message::new(alloc::vec![b'M', b'O', b'D', b'E', mode], alloc::vec::Vec::new())).map_err(|_| "failed to hand SystemManager the boot mode")?;

	// Hand SystemManager the three console/display capabilities, in ONE message carrying three
	// capabilities rather than three messages - the bootstrap is a strictly ordered sequence and
	// every hop has to read it in the same order, so each message added is a place the chain can
	// be got wrong. The order inside the message is the order every hop unpacks them:
	// DisplayController, ConsoleInputSource, ConsoleSink, DeviceManager.
	//
	// Like the power capability above, this process holds them only to pass them on. They are
	// minted exactly here and nowhere else - no syscall creates one - so the three that exist
	// after this line are the four that will ever exist.
	// ALLOC-OK: boot, minting the four privilege capabilities before userspace exists.
	let privileges: alloc::vec::Vec<Capability> = [PrivilegeKind::DisplayController, PrivilegeKind::ConsoleInputSource, PrivilegeKind::ConsoleSink, PrivilegeKind::DeviceManager].into_iter().map(|kind| Capability::new(Privilege::create(kind).expect("the four privilege capabilities, minted at boot before any userspace allocation") as Arc<dyn KernelObject>, Rights::TRANSFER | Rights::DUPLICATE)).collect();
	// ALLOC-OK: boot, before userspace exists - a tag on the same handover path.
	kernel_ep.send(Message::new(b"CONSOLECAPS".to_vec(), privileges)).map_err(|_| "failed to hand SystemManager the console capabilities")?;
	Ok((kernel_ep, process))
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

// How many child Domains the root Domain has.
//
// THE POINT WHERE THE RETRY LADDER STOPS APPLYING is a CHANGE in this number. SystemManager creates
// one child Domain and spawns ServiceManager into it, early - so a manager that faults after that
// leaves a branch full of running processes with no owner. Starting a replacement beside it would
// be two managers over one tree, which is worse than starting again, and the supervisor contract says so in
// as many words: the pre-online ladder applies only BEFORE the control-plane Domain exists.
//
// Counted rather than tested for emptiness, and compared against what was there when the ladder
// began, so the rule says "the last attempt built a branch" and not "somebody, at some point, made
// a Domain". Read-only on purpose: the kernel decides to escalate; it does not go tidying userspace
// up, and the branch's owner is the one that tears it down.
fn root_child_domains() -> usize {
	let mut total: usize = 0;
	let mut at: usize = 0;
	loop {
		let mut batch: [Option<alloc::sync::Arc<object::domain::Domain>>; 8] = [const { None }; _];
		let (written, next) = sched::root_domain().children_from(at, &mut batch);
		total += written;
		if written == 0 && next == at {
			return total;
		}
		at = next;
	}
}

// Supervise a critical process (SystemManager) through the recovery ladder: each round, `spawn` it
// (returning its report endpoint and its process, or None if it could not be spawned), then drive
// the system until the interactive shell is listening. Returns true as soon as a round gets that
// far, or false once every attempt - the original plus `max_restarts` recovery restarts - has
// failed, at which point the caller escalates (reboot as the last resort). This is the kernel's one
// minimal rescue mechanism, the single exception to "the kernel is pure mechanism".
//
// A ROUND ENDS WHEN THE SHELL IS LISTENING, not when one pass of the scheduler left the manager
// standing. All three ports used to wait for readiness AFTER the ladder had already returned
// success - x86_64 inside `console_shell_loop`, the other two in a loop of their own - so a boot
// whose chain never came up was a boot the ladder had called good, and the two hand-rolled tails
// printed "settled" over it. Inside the attempt the three outcomes are distinct and each is honest:
// the shell attaches and the round succeeded; the manager faults or ends while the wait runs and
// the ladder retries; the budget runs out and the round FAILED, which is not a boot that settled.
//
// `settle_rounds` is a caller's number because the machines differ by an order of magnitude:
// riscv64 under TCG needs thousands of passes where x86_64 needs hundreds. A number that differs by
// target is a parameter, not a reason for three functions.
fn supervise(crash_rx: &object::channel::Channel, max_restarts: u32, settle_rounds: u32, mut spawn: impl FnMut() -> Option<(alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::process::Process>)>) -> bool {
	use object::KernelObject;
	let branches_before: usize = root_child_domains();
	for attempt in 0..=max_restarts {
		// A RETRY IS ONLY HONEST WHILE THERE IS NOTHING TO RETRY BESIDE. See above: once the last
		// attempt built the control-plane branch, the ladder is over and the caller reboots.
		if attempt > 0 && root_child_domains() > branches_before {
			serial_println!("recovery: a control-plane branch is already running - not starting a second SystemManager beside it");
			return false;
		}
		// NO PROCESS IS A SPAWN THAT DID NOT HAPPEN, not a process that behaved.
		//
		// This read `koid == 0 || !crash_seen(..)` and returned true for both, so the one gate the
		// recovery ladder has reported SUCCESS when nothing had been started - and the boot then
		// carried on as though userspace were up. A failed spawn is a failed attempt: retry it like
		// a crash, and let the ladder run out if it keeps failing.
		let Some((reports, process)) = spawn() else {
			serial_println!("recovery: SystemManager did not start (attempt {} of {})", attempt + 1, max_restarts + 1);
			continue;
		};
		let koid = process.header().koid();
		sched::run_until_idle();
		if crash_seen(crash_rx, koid) {
			serial_println!("recovery: SystemManager (koid {}) faulted - starting a recovery SystemManager (attempt {} of {})", koid, attempt + 1, max_restarts + 1);
			continue;
		}
		// ENDED IS AS GONE AS FAULTED, and the crash channel only reports the second.
		//
		// Since M11 SystemManager is RESIDENT: it owns the control-plane branch for the life of the
		// system and is not supposed to end at all. A round that leaves it terminated is therefore
		// a failed round, not a quiet success - and reported as success it would put the machine
		// into exactly the state the resident-manager watch exists to catch, one step earlier and
		// with nothing watching yet.
		if process.is_terminated() {
			serial_println!("recovery: SystemManager (koid {}) ended before the system was up (attempt {} of {})", koid, attempt + 1, max_restarts + 1);
			continue;
		}
		// DRIVE THE CHAIN UNTIL THE SHELL IS LISTENING. The reports are drained as they arrive,
		// because the endpoint is bounded and a full one would block the manager that is writing to
		// it - so this is the boot log and the backpressure relief in one loop.
		let mut listening = false;
		let mut gone = false;
		for _ in 0..settle_rounds {
			sched::run_until_idle();
			while let Ok(message) = reports.recv() {
				serial_println!("userspace: {}", core::str::from_utf8(&message.bytes).unwrap_or("<bad>"));
			}
			if console_input::shell_listening() {
				listening = true;
				break;
			}
			// THE MANAGER IS CHECKED ACROSS THE WAIT, not only before it. Bringing the chain up is
			// where it does its work, so it is also where it faults - and a fault noticed only at
			// the end of the budget is a retry that waited for nothing.
			if crash_seen(crash_rx, koid) || process.is_terminated() {
				gone = true;
				break;
			}
			arch::idle_halt();
		}
		if listening {
			return true;
		}
		if gone {
			serial_println!("recovery: SystemManager (koid {}) is gone while the chain was coming up (attempt {} of {})", koid, attempt + 1, max_restarts + 1);
		} else {
			serial_println!("recovery: no interactive shell after {} rounds (attempt {} of {})", settle_rounds, attempt + 1, max_restarts + 1);
		}
	}
	false
}

// Serial receive interrupt: drain the UART FIFO into the console input the moment
// bytes arrive, so typed input wakes the shell immediately instead of waiting for
// the next 100 Hz idle-hook poll (the poll stays as a fallback and for the first-
// prompt nudge). Runs on the BSP (the UART's legacy IRQ is routed there); the
// channel send inside feed() wakes the shell's waiter on this same core.
#[cfg(not(test))]
#[cfg(target_arch = "x86_64")]
fn serial_rx_interrupt(_vector: u32) {
	while let Some(byte) = arch::serial::read_byte() {
		console_input::feed_serial(byte);
	}
}

// THE ONE BOOT TAIL, called by all three entries once their machine is up.
//
// Bring the userspace system up under SystemManager-crash recovery and hand control to the
// interactive shell. The kernel registers a crash-notify channel and supervises SystemManager: if
// it faults, ends, or never brings the chain as far as a listening shell, the kernel starts a
// recovery SystemManager, up to a few times, and reboots as the last resort.
//
// WHAT EACH PORT KEEPS FOR ITSELF is the machine: firmware hand-off, memory, per-CPU, the interrupt
// controller, the timer, device discovery, and its own console arrangement - interrupt-driven on
// x86_64, polled on the other two. What is here is the part that carries POLICY, and it used to
// exist three times: x86_64 called this, and aarch64 and riscv64 hand-rolled a settle loop and then
// called `console_shell_loop` directly. Those two therefore had no recovery ladder, no crash-notify
// channel, no ended-is-as-gone-as-faulted check, no rule that the ladder stops once a control-plane
// branch exists, and no idle hook - so a SystemManager lost after boot left an ownerless control
// plane on two targets of three and nothing noticed. None of that machinery needed porting: it
// carries no `cfg` and compiled on every target already. Two entries simply did not call it.
//
// `settle_rounds` is how long the readiness wait may take on this machine (see `supervise`).
//
// THE ARCHITECTURE IS NOT NEWS ON ITS OWN LOG. Every forwarded userspace line carried the port's
// name - `x86_64: userspace: Shell: online` - on a machine that has exactly one architecture, in a
// serial log that belongs to one boot of it. The prefix that says which of the three produced a log
// is the log's own name, not a repetition on every line inside it.
// WHAT THIS MACHINE IS, printed once by every port rather than by one of them.
//
// Both lines are baselines: "every bus-mastering device is untranslated" and "one node, no locality"
// are facts about the machine rather than absences of news, and a report that is always there is
// what makes a later change worth reading. Placed in the shared tail because a report only x86
// printed would be a report that says nothing about the ports most likely to differ.
#[cfg(not(test))]
fn report_machine() {
	// The cores are bound to their nodes only now, after bring-up has established which of them
	// answered. Firmware describes processors it believes exist; this binds the ones that are here.
	let bound = smp::numa::bind_online();
	if bound > 0 {
		serial_println!("numa: {bound} of {} core(s) bound to a node", smp::cpu_count());
	}
	mem::numa::report();
}

#[cfg(not(test))]
pub(crate) fn boot_userspace(settle_rounds: u32) {
	report_machine();
	const MAX_RESTARTS: u32 = 3;
	// Pump the serial console from the scheduler's idle spin: virtio-gpu polls its
	// display size on a short repeating timer, so run_until_idle never returns and the
	// BSP would never reach console_shell_loop to poll the UART. The idle hook keeps
	// serial input live regardless (the keyboard is interrupt-driven and unaffected),
	// and it is what watches for a resident SystemManager going away later.
	sched::set_idle_hook(serial_console_pump);
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let up = supervise(&crash_rx, MAX_RESTARTS, settle_rounds, || match spawn_system_manager() {
		Ok((ep, process)) => Some((ep, process)),
		Err(reason) => {
			serial_println!("recovery: could not start SystemManager: {}", reason);
			None
		}
	});
	if up {
		// AND NOW THE DMA ISOLATION STATE, because now it is a fact. The devices bound while the
		// service set came up, so this is the first point at which "is anything reaching memory
		// untranslated" - and therefore what the controller is actually doing - has an answer.
		// See `dma_policy::report`.
		dma_policy::report();
		// SET HERE AND NOT BEFORE. From this point a SystemManager that ends is a control plane
		// with no owner, and the idle hook reboots rather than letting the machine run on as though
		// somebody were still supervising. Before the shell is listening there is nothing to own.
		SYSTEM_IS_UP.store(true, core::sync::atomic::Ordering::Relaxed);
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
