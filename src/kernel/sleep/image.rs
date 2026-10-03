// HIBERNATION ON THE DEVICE-TREE PORTS - aarch64 and riscv64: the snapshot's entry, the machine off with its image
// written, and the whole-memory replacement a restore ends with. What is copied, and the frames a restore keeps out of
// the way, are `sleep::disk`'s; the cores' contexts are the per-core resume path's (`arch::resume`); what is the
// machine's - its interrupt controller, its PCI functions, the MSI-X entries the kernel programmed - is the port's
// (`arch::sleep::save_machine` and `restore_machine`); and the trampoline that copies is the port's (`replace_jump`).
//
// THE SNAPSHOT runs from the boot core's idle context (`sleep::boot_core`), every other core HELD - and on these ports a
// held core saves its context first, through the per-core resume path, so the image holds every core's record. The
// machine's settings are saved, then the boot core's own context with them (`resume::save_and_leave`), and the copy of
// every page in use is the `leave`. The save RETURNS TWICE. Directly: the machine that ran on, whose copy the image
// component now reads and writes out. Through the resume entry, `RESUMED`: the machine a restore replaced memory with -
// every setting written back, then every other core turned on again AT ITS RECORD (`resume::start_saved`), so it comes
// back inside its hold, which has ended.
//
// THE RESUME CONTEXT the image carries is what the restore jumps with: the kernel's page tables, the resume entry's
// physical address, the boot core's record and the core it ran on - its hart id or its MPIDR - with the direct map's
// offset, the top of RAM and where the kernel was loaded, which must match for the image's own tables to map what they
// mapped.
//
// THE REPLACEMENT runs on THE CORE THE IMAGE'S BOOT CORE WAS - a firmware may start a boot on any of them, and an image's
// cores are its interrupt files' and its redistributors' - with every other core turned off (`idle::stop_held`), since
// the image's kernel turns each on again at its own record. That core, if it is not the boot core, takes it from its
// hold; the boot core, then, turns itself off. The trampoline (`replace_jump`) runs from its physical address with the
// MMU off, copies every page to its frame, and jumps to the image's resume entry with the image's boot record.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use abi::{ERR_RESOURCE_EXHAUSTED, ERR_TIMED_OUT, ERR_UNSUPPORTED, SNAPSHOT_CONTEXT, SleepReport};

use crate::arch;
use crate::arch::common::time::CLOCK;
use crate::sleep::disk;

// "LSHIBCTX", little-endian: the context's first word.
const CONTEXT_MAGIC: u64 = u64::from_le_bytes(*b"LSHIBCTX");

// THE RESUME CONTEXT this kernel writes into a snapshot, and checks a restore's against.
pub fn context() -> [u8; SNAPSHOT_CONTEXT] {
	encode(words())
}

fn words() -> [u64; 8] {
	[
		CONTEXT_MAGIC,
		crate::sched::kernel_cr3(),
		arch::resume::entry(),
		arch::resume::record_address(0),
		crate::mem::hhdm_offset(),
		disk::ram_top(),
		arch::sleep::kernel_image().0,
		crate::smp::lapic_id(0),
	]
}

fn encode(words: [u64; 8]) -> [u8; SNAPSHOT_CONTEXT] {
	let mut out = [0u8; SNAPSHOT_CONTEXT];
	for (at, word) in words.iter().enumerate() {
		out[at * 8..at * 8 + 8].copy_from_slice(&word.to_le_bytes());
	}
	out
}

fn decode(context: &[u8; SNAPSHOT_CONTEXT]) -> [u64; 8] {
	let mut out = [0u64; 8];
	for (at, word) in out.iter_mut().enumerate() {
		let mut bytes = [0u8; 8];
		bytes.copy_from_slice(&context[at * 8..at * 8 + 8]);
		*word = u64::from_le_bytes(bytes);
	}
	out
}

// A CONTEXT THIS KERNEL WROTE: its magic, the same entry and boot record, the same direct map, the same top of RAM and
// the same load address. The page tables' address is the image's own and is checked only for being a page; the core
// it ran on is the one the replacement runs on.
pub fn context_is_ours(context: &[u8; SNAPSHOT_CONTEXT]) -> bool {
	let [magic, root, entry, record, hhdm, top, image, _] = decode(context);
	let ours = words();
	magic == CONTEXT_MAGIC && root != 0 && root % 4096 == 0 && entry == ours[2] && record == ours[3] && hhdm == ours[4] && top == ours[5] && image == ours[6]
}

// ------------------------------------------------------------------ the snapshot

// What the copy answered, read after the save returns directly.
static FITS: AtomicBool = AtomicBool::new(false);

extern "C" fn copy_leave(_record: *mut arch::resume::Record, _arg: u64) -> i64 {
	FITS.store(disk::copy_now(), Ordering::Release);
	0
}

// EVERY OTHER CORE HELD - its context saved - within a bound; false when one was not.
fn hold_every_other_core() -> bool {
	crate::idle::begin_hold();
	let mut waited = 0u32;
	while !crate::idle::all_others_held() {
		if waited > 1_000_000 {
			crate::idle::end_hold();
			return false;
		}
		waited += 1;
		core::hint::spin_loop();
	}
	true
}

// THE SNAPSHOT, on the boot core's idle context - see the head of this file. `WAKE_SNAPSHOT` in the machine that ran on,
// `WAKE_RESTORED` in the machine restored from the image.
pub fn run_snapshot() -> Result<SleepReport, i64> {
	if let Some(why) = arch::sleep::disk_refused() {
		crate::serial_println!("sleep: hibernation's snapshot not taken - {why}");
		return Err(ERR_UNSUPPORTED);
	}
	// THE COPY'S FRAMES FIRST, while everything still runs: the count, the copies, the lists and the bitmap.
	if let Err(error) = disk::prepare(context()) {
		crate::serial_println!("sleep: hibernation's snapshot not taken - free memory cannot hold a copy of the pages in use ({error})");
		return Err(error);
	}
	let suspended_at = match crate::sleep::prologue("hibernation's snapshot") {
		Ok(at) => at,
		Err(error) => {
			disk::release();
			return Err(error);
		}
	};
	let rtc = arch::rtc_present();
	let wall_before = if rtc { arch::rtc::read_unix() } else { 0 };
	arch::sleep::mask_device_lines();
	if !hold_every_other_core() {
		arch::sleep::unmask_device_lines();
		disk::release();
		return Err(abandon(suspended_at, "every other core was not held within its bound", ERR_TIMED_OUT));
	}
	arch::disable_interrupts();
	if !arch::sleep::save_machine() {
		arch::enable_interrupts();
		crate::idle::end_hold();
		arch::sleep::unmask_device_lines();
		disk::release();
		return Err(abandon(suspended_at, "the machine's settings could not be saved", ERR_UNSUPPORTED));
	}
	let restored = arch::resume::save_and_leave(copy_leave, 0) == arch::resume::RESUMED;
	if !restored {
		// ------------------------------------------------------------------ the first return: the machine ran on
		arch::enable_interrupts();
		crate::idle::end_hold();
		arch::sleep::unmask_device_lines();
		if !FITS.load(Ordering::Acquire) {
			disk::release();
			return Err(abandon(suspended_at, "the pages in use outgrew the copy prepared for them", ERR_RESOURCE_EXHAUSTED));
		}
		let pages = disk::taken();
		CLOCK.rebase(suspended_at, arch::tsc::now());
		crate::serial_println!("sleep: hibernation's snapshot taken - {pages} page(s) copied, for the image to be written");
		arch::serial::sleep_end();
		return Ok(SleepReport { wake: abi::WAKE_SNAPSHOT, ..SleepReport::default() });
	}
	// ------------------------------------------------------------------ the second return: restored from the image
	let resumed_at = arch::tsc::now();
	arch::serial::sleep_wake(true);
	CLOCK.rebase(suspended_at, resumed_at);
	arch::sleep::restore_machine();
	crate::declared::replay_claim_writes();
	// THE IOMMU BEFORE ANY DRIVER RUNS: the fresh boot's controller holds the fresh boot's domains, and the image's
	// attachments and mappings go to it again.
	let translating = crate::iommu::resume_after_reset();
	// THE SLEEP'S LENGTH FROM THE PLATFORM'S CLOCK, which ran through it - or, with none, from the base the clock's
	// driver hands again at its resume.
	let slept_ns = if rtc {
		arch::rtc::read_unix().saturating_sub(wall_before).saturating_mul(1_000_000_000)
	} else {
		crate::sleep::slept_unknown();
		0
	};
	crate::sleep::add_slept(slept_ns);
	// THE RANDOM POOL MOVED ON: its state is the image's, from before the copy, and what the machine that ran on drew
	// after it - the image's key among it - must not be drawn again.
	stir_entropy(resumed_at);
	// EVERY OTHER CORE BACK, AT ITS RECORD, inside the hold that has now ended.
	crate::idle::end_hold();
	let back = restart_other_cores();
	arch::apic::timer_periodic();
	arch::enable_interrupts();
	arch::sleep::unmask_device_lines();
	disk::release();
	if !translating {
		crate::serial_println!("sleep: the IOMMU did not come back - the drivers that require it will be refused at their resume");
	}
	let total = crate::smp::cpu_count();
	if back + 1 < total {
		crate::serial_println!("sleep: {} of {} other core(s) came back from the image", back, total - 1);
	}
	let report = SleepReport { wake: abi::WAKE_RESTORED, slept_ns, ..SleepReport::default() };
	crate::sleep::epilogue(&report, true);
	Ok(report)
}

// EVERY OTHER CORE TURNED ON AT ITS RECORD - each waited for until it has left its hold, within a bound. How many came
// back.
fn restart_other_cores() -> usize {
	let mut back = 0;
	for cpu in 1..crate::smp::cpu_count() {
		let status = arch::resume::start_saved(cpu);
		if status != 0 {
			crate::serial_println!("sleep: core {cpu} was not turned on again ({status})");
			continue;
		}
		let mut waited = 0u64;
		while crate::idle::held(cpu) && waited < 2_000_000_000 {
			waited += 1;
			core::hint::spin_loop();
		}
		if crate::idle::held(cpu) {
			crate::serial_println!("sleep: core {cpu} did not come back from the image");
		} else {
			back += 1;
		}
	}
	back
}

fn stir_entropy(counter: u64) {
	let mut stirred = [0u8; 24];
	stirred[..8].copy_from_slice(&counter.to_le_bytes());
	stirred[8..16].copy_from_slice(&arch::tsc::now().to_le_bytes());
	stirred[16..].copy_from_slice(&crate::sleep::rtc_unix().to_le_bytes());
	crate::entropy::stir(&stirred);
}

// A SNAPSHOT THAT DID NOT HAPPEN, unwound: the clock rebased on the counter that ran on, the reason said, and the console
// window closed.
fn abandon(suspended_at: u64, why: &str, error: i64) -> i64 {
	CLOCK.rebase(suspended_at, arch::tsc::now());
	crate::serial_println!("sleep: not entered - {why}");
	arch::serial::sleep_end();
	error
}

// ------------------------------------------------------------------ the machine off, its image written

// NO `\_S4` ON THESE PORTS: the machine powered off through the firmware - PSCI's SYSTEM_OFF, the SBI's System Reset - and
// the path named among the last words, as power-off names its own.
pub fn enter_disk() -> ! {
	crate::serial_println!("hibernate: the image is written - no S4 on this machine, so it is powered off");
	arch::poweroff()
}

// ------------------------------------------------------------------ the replacement

// The parameters page: the page count, the directory pages' count, the entry, the record and the core's id, then the
// directory pages' addresses from `DIRS`.
const DIRS: u64 = 8;
const MAX_DIRS: u64 = 512 - DIRS;
// List pages one directory page names.
const PER_DIR: u64 = 512;

// THE PARAMETERS, for a held core that takes the replacement from its hold.
static PARAMS: AtomicU64 = AtomicU64::new(0);

// THE WHOLE-MEMORY REPLACEMENT - see the head of this file. Answers only when it cannot happen, with nothing changed.
pub fn replace_memory(restore: &mut disk::Restore) -> i64 {
	let [_, _, entry, record, _, _, _, boot_id] = decode(&restore.context);
	let Some(jumper) = (0..crate::smp::cpu_count()).find(|&cpu| crate::smp::lapic_id(cpu) == boot_id) else {
		crate::serial_println!("hibernate: no core of this boot is the one the image's ran on ({boot_id:#x}) - the replacement cannot run");
		return ERR_UNSUPPORTED;
	};
	let (lists, pages) = restore.pairs();
	let Some(list_pages) = crate::mem::heap::try_to_vec(lists) else { return ERR_RESOURCE_EXHAUSTED };
	let dir_count = (list_pages.len() as u64).div_ceil(PER_DIR);
	if dir_count > MAX_DIRS {
		return ERR_UNSUPPORTED;
	}
	// EVERY FRAME THE TRAMPOLINE READS, outside the image: the parameters and the directory pages.
	let mut take = || restore.frame().ok_or(ERR_RESOURCE_EXHAUSTED);
	let params = match take() {
		Ok(frame) => frame,
		Err(error) => return error,
	};
	let mut dirs = alloc::vec::Vec::new();
	if dirs.try_reserve_exact(dir_count as usize).is_err() {
		return ERR_RESOURCE_EXHAUSTED;
	}
	for _ in 0..dir_count {
		match take() {
			Ok(frame) => dirs.push(frame),
			Err(error) => return error,
		}
	}
	let at = |phys: u64| (crate::mem::hhdm_offset() + phys) as *mut u64;
	// SAFETY: every frame written below was just taken for this and is reached through the direct map.
	unsafe {
		for &phys in core::iter::once(&params).chain(dirs.iter()) {
			core::ptr::write_bytes(at(phys) as *mut u8, 0, 4096);
		}
		for (index, &page) in list_pages.iter().enumerate() {
			at(dirs[index / PER_DIR as usize]).add(index % PER_DIR as usize).write(page);
		}
		let words = at(params);
		words.write(pages);
		words.add(1).write(dir_count);
		words.add(2).write(entry);
		words.add(3).write(record);
		words.add(4).write(boot_id);
		for (index, &dir) in dirs.iter().enumerate() {
			words.add((DIRS as usize) + index).write(dir);
		}
	}
	if !hold_every_other_core() {
		crate::serial_println!("hibernate: every other core was not held - the replacement cannot run");
		return ERR_TIMED_OUT;
	}
	crate::serial_println!("hibernate: replacing memory with the image - {pages} page(s) on core {jumper}; the image's kernel continues");
	arch::serial::flush_sync();
	PARAMS.store(params, Ordering::SeqCst);
	arch::disable_interrupts();
	// EVERY HELD CORE OFF - but the one the image's boot core was, which takes the replacement from its hold.
	crate::idle::stop_held(Some(jumper).filter(|&cpu| cpu != crate::sched::current_cpu_id()));
	if jumper == crate::sched::current_cpu_id() {
		jump()
	}
	// THIS CORE OFF TOO: the jumper copies only once every other core is.
	arch::resume::off_now()
}

// THE JUMP, on the core that takes it: every other core off, within a bound - a core still running would run in memory
// being replaced - and then the port's trampoline, which never returns.
pub fn jump() -> ! {
	// THE BOUND IS A TIME, NOT A COUNT: an emulated core spins at a pace no count describes.
	const OFF_BOUND_NS: u64 = 10_000_000_000;
	let this = crate::sched::current_cpu_id();
	let started = CLOCK.nanos(arch::tsc::now());
	for cpu in 0..crate::smp::cpu_count() {
		if cpu == this {
			continue;
		}
		while !arch::resume::stopped(cpu) {
			if CLOCK.nanos(arch::tsc::now()).saturating_sub(started) > OFF_BOUND_NS {
				crate::serial_println!("hibernate: core {cpu} did not turn off - the replacement does not run, and this core halts");
				arch::halt_loop();
			}
			core::hint::spin_loop();
		}
	}
	crate::serial_println!("hibernate: every other core is off - core {this} copies the image and jumps into its kernel");
	arch::serial::flush_sync();
	arch::sleep::replace_jump(PARAMS.load(Ordering::SeqCst))
}
