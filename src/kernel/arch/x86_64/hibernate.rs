// THE x86_64 HALF OF HIBERNATION: the snapshot's entry, the machine off with its image written, and the whole-memory
// replacement a restore ends with. What is copied, and the frames a restore keeps out of the way, are `sleep::disk`'s.
//
// THE SNAPSHOT is S3's entry up to the last step: from the boot core's idle context, every other core held, the I/O
// APIC's and every function's configuration saved, the loader's identity map reinstated in the kernel's page tables and
// those tables loaded, the callee-saved registers and the stack pointer saved (`sleep::s3_save_and_enter`) - and then,
// instead of SLP_EN, the copy of every page in use (`disk::copy_now`). The save RETURNS TWICE. Directly, 0: the machine
// that ran on, whose copy the image component now reads and writes out. Through `s3_resume_entry`, 1: the machine a
// restore replaced memory with, which continues exactly as S3's resume does - the same core state established again,
// every saved setting written back, the other cores restarted - for a machine whose devices were all reset.
//
// THE RESUME CONTEXT the image carries is what the restore jumps with: the kernel's page tables (their physical address,
// restored with the image), `s3_resume_entry` and the resume stack - the same addresses in every boot of the same
// kernel, which the system image's digest guarantees it is - and the direct map's offset and the top of RAM, which must
// match for the image's own tables to map what they mapped.
//
// THE REPLACEMENT runs from a TRAMPOLINE in a frame the image does not use, on page tables of its own in frames the
// image does not use either - an identity map of all RAM in 2 MiB pages - because the kernel running it, its tables
// included, is what is being overwritten. The kernel's own tables get the loader's identity map back so the jump to the
// trampoline's identity address holds; the trampoline loads its own tables, turns global pages off so no translation of
// the kernel it replaces survives, copies every page to its frame, then loads the image's tables - whose reinstated
// identity map still maps the trampoline - and jumps to the image's `s3_resume_entry` on the image's resume stack. Every
// other core is sent INIT first, so it waits for the startup IPI the image's kernel sends when it restarts them.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, Ordering};

use abi::{ERR_RESOURCE_EXHAUSTED, ERR_TIMED_OUT, ERR_UNSUPPORTED, SNAPSHOT_CONTEXT, SleepReport};

use super::port::{inw, outw};
use super::sleep::{PWRBTN_STS, RTC_STS, SLEEP_RESUME_STACK_TOP, SLPBTN_STS, WAK_STS};
use crate::arch::common::time::CLOCK;
use crate::sleep::disk;

// "LSHIBCTX", little-endian: the context's first word.
const CONTEXT_MAGIC: u64 = u64::from_le_bytes(*b"LSHIBCTX");

unsafe extern "C" {
	static __kernel_image_start: u8;
	static __kernel_image_end: u8;
	fn hibernate_trampoline_start();
	fn hibernate_trampoline_end();
}

// The kernel's code and read-only data as loaded: what the system image's digest reads of the kernel.
pub fn kernel_image() -> (u64, usize) {
	// Two linker symbols' addresses, never dereferenced here.
	let (start, end) = (&raw const __kernel_image_start as u64, &raw const __kernel_image_end as u64);
	(start, end.saturating_sub(start) as usize)
}

// THE DEVELOPMENT SWITCH FOR ANOTHER SYSTEM IMAGE: on a development profile only, the bytes of the fw_cfg file
// `opt/org.libersystem/system-variant` are a part of the system image's digest - what a boot of another build of the
// system changes. The hibernation gate cannot boot a second build of one tree, so it boots this one naming a variant; a
// shipping boot names no profile, and the file is never read there.
pub fn development_variant(out: &mut [u8; 64]) -> usize {
	if super::boot_profile().is_none() {
		return 0;
	}
	super::fwcfg::read_file(b"opt/org.libersystem/system-variant", out).unwrap_or(0)
}

// THE RESUME CONTEXT this kernel writes into a snapshot, and checks a restore's against.
pub fn context() -> [u8; SNAPSHOT_CONTEXT] {
	encode(words())
}

fn words() -> [u64; 8] {
	[
		CONTEXT_MAGIC,
		crate::sched::kernel_cr3(),
		super::sleep::s3_resume_entry as *const () as u64,
		SLEEP_RESUME_STACK_TOP(),
		crate::mem::hhdm_offset(),
		disk::ram_top(),
		kernel_image().0,
		0,
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

// A CONTEXT THIS KERNEL WROTE: its magic, the same entry and resume stack, the same direct map and the same top of RAM.
// The page tables' address is the image's own and is checked only for being a page.
pub fn context_is_ours(context: &[u8; SNAPSHOT_CONTEXT]) -> bool {
	let [magic, cr3, entry, stack, hhdm, top, image, _] = decode(context);
	let ours = words();
	magic == CONTEXT_MAGIC && cr3 != 0 && cr3 % 4096 == 0 && entry == ours[2] && stack == ours[3] && hhdm == ours[4] && top == ours[5] && image == ours[6]
}

// What the copy answered, read after the save returns directly.
static FITS: AtomicBool = AtomicBool::new(false);

extern "C" fn snapshot_now() -> u64 {
	FITS.store(disk::copy_now(), Ordering::Release);
	0
}

// ------------------------------------------------------------------ the snapshot

// THE SNAPSHOT, on the boot core's idle context - see the head of this file. `WAKE_SNAPSHOT` in the machine that ran on,
// `WAKE_RESTORED` in the machine restored from the image.
pub(super) fn run_snapshot() -> Result<SleepReport, i64> {
	let Some(ports) = super::sleep::ports() else { return Err(ERR_UNSUPPORTED) };
	let tramp_phys = crate::boot_info().smp_trampoline;
	let tramp = (crate::mem::hhdm_offset() + tramp_phys) as *mut u8;
	let kernel_cr3 = crate::sched::kernel_cr3();
	if tramp_phys == 0 || tramp_phys >= 0x10_0000 || !super::apboot::cr3_is_reachable(kernel_cr3) {
		return Err(ERR_UNSUPPORTED);
	}
	// THE COPY'S FRAMES FIRST, while everything still runs: the count, the copies, the lists and the bitmap.
	if let Err(error) = disk::prepare(context()) {
		crate::serial_println!("sleep: hibernation's snapshot not taken - free memory cannot hold a copy of the pages in use ({error})");
		return Err(error);
	}
	let rtc = super::rtc_present();
	let suspended_at = match crate::sleep::prologue("hibernation's snapshot") {
		Ok(at) => at,
		Err(error) => {
			disk::release();
			return Err(error);
		}
	};
	let wall_before = if rtc { super::rtc::read_unix() } else { 0 };
	super::sleep::mask_device_lines();
	crate::idle::begin_hold();
	let mut waited = 0u32;
	while !crate::idle::all_others_held() {
		if waited > 1_000_000 {
			crate::idle::end_hold();
			super::sleep::unmask_device_lines();
			disk::release();
			return Err(super::sleep::abandon(suspended_at, false, "every other core was not parked within its bound", ERR_TIMED_OUT));
		}
		waited += 1;
		core::hint::spin_loop();
	}
	super::disable_interrupts();
	super::ioapic::save_all();
	if !super::pci::save_config_all() || !super::paging::reinstate_bootstrap_identity(kernel_cr3) {
		super::enable_interrupts();
		crate::idle::end_hold();
		super::sleep::unmask_device_lines();
		disk::release();
		return Err(super::sleep::abandon(suspended_at, false, "the functions' configuration or the trampoline's identity map could not be kept", ERR_UNSUPPORTED));
	}
	// THE KERNEL'S OWN PAGE TABLES, as S3 loads them: these are the tables the image's context names.
	unsafe { asm!("mov cr3, {}", in(reg) kernel_cr3, options(nostack, preserves_flags)) };
	// SAFETY: the page the trampoline was installed into at boot; nothing else uses it while every other core is held.
	unsafe { super::apboot::set_root(tramp, kernel_cr3) };
	// SAFETY: the saved frame is this function's own, on the boot core's idle stack, which nothing else runs on.
	let restored = unsafe { super::sleep::s3_save_and_enter(snapshot_now) } == 1;
	if !restored {
		// ------------------------------------------------------------------ the first return: the machine ran on
		super::paging::remove_reinstated_identity(kernel_cr3);
		super::enable_interrupts();
		crate::idle::end_hold();
		super::sleep::unmask_device_lines();
		if !FITS.load(Ordering::Acquire) {
			disk::release();
			return Err(super::sleep::abandon(suspended_at, false, "the pages in use outgrew the copy prepared for them", ERR_RESOURCE_EXHAUSTED));
		}
		let pages = disk::taken();
		CLOCK.rebase(suspended_at, super::tsc::now());
		crate::serial_println!("sleep: hibernation's snapshot taken - {pages} page(s) copied, for the image to be written");
		super::serial::sleep_end();
		return Ok(SleepReport { wake: abi::WAKE_SNAPSHOT, ..SleepReport::default() });
	}
	// ------------------------------------------------------------------ the second return: restored from the image
	let resumed_at = super::tsc::now();
	super::serial::sleep_wake(true);
	CLOCK.rebase(suspended_at, resumed_at);
	for (status, _) in ports.status.iter().flatten() {
		unsafe { outw(*status, inw(*status) & (WAK_STS | RTC_STS | PWRBTN_STS | SLPBTN_STS)) };
	}
	super::ioapic::restore_all();
	super::pci::restore_config_all();
	super::interrupts::restore_msix_entries();
	crate::declared::replay_claim_writes();
	#[cfg(not(test))]
	super::sci::rearm_after_reset();
	let translating = crate::iommu::resume_after_reset();
	let slept_ns = if rtc {
		super::rtc::read_unix().saturating_sub(wall_before).saturating_mul(1_000_000_000)
	} else {
		crate::sleep::slept_unknown();
		0
	};
	crate::sleep::add_slept(slept_ns);
	// THE RANDOM POOL MOVED ON: its state is the image's, from before the copy, and what the machine that ran on drew
	// after the copy - the image's key among it - must not be drawn again. Fresh bytes stirred in before anything draws.
	stir_entropy(resumed_at);
	crate::idle::end_hold();
	crate::smp::restart_after_resume(tramp, (tramp_phys >> 12) as u8);
	super::paging::remove_reinstated_identity(kernel_cr3);
	super::apic::timer_periodic();
	super::enable_interrupts();
	super::sleep::unmask_device_lines();
	if !translating {
		crate::serial_println!("sleep: the IOMMU did not come back - the drivers that require it will be refused at their resume");
	}
	// THE SNAPSHOT'S COPIES, held in the image's own allocator state and nobody's now.
	disk::release();
	let report = SleepReport { wake: abi::WAKE_RESTORED, slept_ns, ..SleepReport::default() };
	crate::sleep::epilogue(&report, true);
	Ok(report)
}

fn stir_entropy(counter: u64) {
	let mut fresh = [0u8; 32];
	if super::random::secure(&mut fresh) {
		crate::entropy::absorb(&fresh, entropy::Source::Hardware);
	}
	let mut stirred = [0u8; 24];
	stirred[..8].copy_from_slice(&counter.to_le_bytes());
	stirred[8..16].copy_from_slice(&super::tsc::now().to_le_bytes());
	stirred[16..].copy_from_slice(&crate::sleep::rtc_unix().to_le_bytes());
	crate::entropy::stir(&stirred);
}

// ------------------------------------------------------------------ the machine off, its image written

// `\_S4` WHERE THE FIRMWARE OFFERS IT, soft-off otherwise. The path is named among the last words, as power-off names
// its own.
pub fn enter_disk() -> ! {
	let ports = super::sleep::ports();
	match (crate::sleep::sleep_type(4), ports) {
		(Some((typ_a, typ_b)), Some(ports)) => {
			crate::serial_println!("hibernate: the image is written - the registered \\_S4 ({typ_a}, {typ_b}) into PM1 control");
			super::serial::flush_sync();
			super::disable_interrupts();
			unsafe {
				asm!("wbinvd", options(nostack, preserves_flags));
				let value_a = (inw(ports.control_a) & !(0x7 << 10 | 1 << 13)) | u16::from(typ_a) << 10;
				outw(ports.control_a, value_a);
				if ports.control_b != 0 {
					let value_b = (inw(ports.control_b) & !(0x7 << 10 | 1 << 13)) | u16::from(typ_b) << 10;
					outw(ports.control_b, value_b);
					outw(ports.control_a, value_a | 1 << 13);
					outw(ports.control_b, value_b | 1 << 13);
				} else {
					outw(ports.control_a, value_a | 1 << 13);
				}
			}
			for _ in 0..50_000_000u64 {
				core::hint::spin_loop();
			}
			crate::serial_println!("hibernate: the machine did not enter S4 - it is powered off instead");
			super::poweroff()
		}
		_ => {
			crate::serial_println!("hibernate: the image is written - no \\_S4 registered, so the machine is powered off");
			super::poweroff()
		}
	}
}

// ------------------------------------------------------------------ the replacement

// The trampoline page's layout: the code from the start, the parameters at `PARAMS`, the directory pages' addresses
// after them.
const PARAMS: u64 = 0x800;
const DIRS: u64 = PARAMS + 64;
const MAX_DIRS: u64 = (4096 - DIRS) / 8;
// List pages one directory page names.
const PER_DIR: u64 = 512;

global_asm!(
	r#"
.section .text.hibernate, "ax"
.globl hibernate_trampoline_start
.globl hibernate_trampoline_end
// FROM ITS IDENTITY ADDRESS, interrupts off, rdi the parameters' identity address: position-independent, no stack.
hibernate_trampoline_start:
	mov rax, [rdi]
	mov cr3, rax
	mov rax, cr4
	and rax, -129
	mov cr4, rax
	mov r8, [rdi + 8]
	xor r9, r9
2:
	test r8, r8
	jz 5f
	mov rax, r9
	shr rax, 9
	mov r10, [rdi + 64 + rax * 8]
	mov rax, r9
	and rax, 511
	mov r10, [r10 + rax * 8]
	mov r11, 256
	cmp r8, r11
	cmovb r11, r8
	sub r8, r11
3:
	test r11, r11
	jz 4f
	mov rdx, rdi
	mov rcx, 512
	mov rsi, [r10 + 8]
	mov rdi, [r10]
	cld
	rep movsq
	mov rdi, rdx
	add r10, 16
	dec r11
	jmp 3b
4:
	inc r9
	jmp 2b
5:
	mov rax, [rdi + 24]
	mov rsp, [rdi + 32]
	mov rbx, [rdi + 40]
	mov cr3, rax
	jmp rbx
hibernate_trampoline_end:
"#
);

// THE WHOLE-MEMORY REPLACEMENT - see the head of this file. Answers only when it cannot happen, with nothing changed.
pub fn replace_memory(restore: &mut disk::Restore) -> i64 {
	let [_, image_cr3, entry, stack, _, top, _, _] = decode(&restore.context);
	let gib = top.div_ceil(1 << 30);
	let (lists, pages) = restore.pairs();
	let Some(list_pages) = crate::mem::heap::try_to_vec(lists) else { return ERR_RESOURCE_EXHAUSTED };
	let dir_count = (list_pages.len() as u64).div_ceil(PER_DIR);
	if gib == 0 || gib > 512 || dir_count > MAX_DIRS {
		return ERR_UNSUPPORTED;
	}
	// EVERY FRAME THE TRAMPOLINE TOUCHES, outside the image: its page, its tables and the directory pages.
	let mut take = || restore.frame().ok_or(ERR_RESOURCE_EXHAUSTED);
	let frames = (|| -> Result<(u64, u64, u64, alloc::vec::Vec<u64>, alloc::vec::Vec<u64>), i64> {
		let tramp = take()?;
		let pml4 = take()?;
		let pdpt = take()?;
		let mut pds = alloc::vec::Vec::new();
		pds.try_reserve_exact(gib as usize).map_err(|_| ERR_RESOURCE_EXHAUSTED)?;
		for _ in 0..gib {
			pds.push(take()?);
		}
		let mut dirs = alloc::vec::Vec::new();
		dirs.try_reserve_exact(dir_count as usize).map_err(|_| ERR_RESOURCE_EXHAUSTED)?;
		for _ in 0..dir_count {
			dirs.push(take()?);
		}
		Ok((tramp, pml4, pdpt, pds, dirs))
	})();
	let (tramp, pml4, pdpt, pds, dirs) = match frames {
		Ok(frames) => frames,
		Err(error) => return error,
	};
	let hhdm = crate::mem::hhdm_offset();
	let at = |phys: u64| (hhdm + phys) as *mut u64;
	const PRESENT_WRITE: u64 = 0b11;
	const LARGE: u64 = 1 << 7;
	// SAFETY: every frame written below was just taken for this and is reached through the direct map.
	unsafe {
		for phys in [pml4, pdpt].iter().chain(pds.iter()).chain(dirs.iter()).chain(core::iter::once(&tramp)) {
			core::ptr::write_bytes(at(*phys) as *mut u8, 0, 4096);
		}
		// AN IDENTITY MAP OF ALL RAM, 2 MiB pages, supervisor, writable and executable.
		for (gigabyte, &pd) in pds.iter().enumerate() {
			for entry in 0..512u64 {
				at(pd).add(entry as usize).write((gigabyte as u64) << 30 | entry << 21 | PRESENT_WRITE | LARGE);
			}
			at(pdpt).add(gigabyte).write(pd | PRESENT_WRITE);
		}
		at(pml4).write(pdpt | PRESENT_WRITE);
		for (index, &page) in list_pages.iter().enumerate() {
			at(dirs[index / PER_DIR as usize]).add(index % PER_DIR as usize).write(page);
		}
		let start = hibernate_trampoline_start as *const () as *const u8;
		let length = hibernate_trampoline_end as *const () as usize - start as usize;
		if length as u64 > PARAMS {
			return ERR_UNSUPPORTED;
		}
		core::ptr::copy_nonoverlapping(start, at(tramp) as *mut u8, length);
		let params = at(tramp + PARAMS);
		params.write(pml4);
		params.add(1).write(pages);
		params.add(2).write(dir_count);
		params.add(3).write(image_cr3);
		params.add(4).write(stack);
		params.add(5).write(entry);
		for (index, &dir) in dirs.iter().enumerate() {
			at(tramp + DIRS).add(index).write(dir);
		}
	}
	let kernel_cr3 = crate::sched::kernel_cr3();
	if !super::paging::reinstate_bootstrap_identity(kernel_cr3) {
		crate::serial_println!("hibernate: the loader's identity map is not kept - the replacement cannot jump");
		return ERR_UNSUPPORTED;
	}
	crate::serial_println!("hibernate: replacing memory with the image - {pages} page(s); the image's kernel continues");
	super::serial::flush_sync();
	// EVERY OTHER CORE TO INIT: waiting for the startup IPI the image's kernel sends when it restarts them. This runs on
	// the boot core, handed there as S3 is, so the core that stays is the one the image's resume path is.
	super::disable_interrupts();
	for cpu in 1..crate::smp::cpu_count() {
		super::apic::send_init(crate::smp::lapic_id(cpu) as u32);
	}
	// SAFETY: the trampoline page is identity-mapped by the reinstated map in the tables loaded here, holds the
	// position-independent copy and its parameters, and never returns.
	unsafe {
		asm!("mov cr3, {}", in(reg) kernel_cr3, options(nostack, preserves_flags));
		asm!("jmp {entry}", entry = in(reg) tramp, in("rdi") tramp + PARAMS, options(noreturn));
	}
}
