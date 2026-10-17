// driver.nvdimm - persistent memory the NFIT describes, served as the block wire.
//
// WHAT IT BINDS. One persistent-memory row, `table:NFIT#n`: the range as the row's one window, and the namespace
// record the kernel derived from the table - the modules' state flags and the platform's capabilities - as its
// property block. The kernel published the row only after the range passed the table's own checks and the memory
// map's, and a control region is never a row; so everything this driver maps is memory, and only its own.
//
// WHAT IT SERVES. The whole range as sectors, on the same wire every storage driver serves: a read copies out of the
// range into a buffer the client is handed, a write copies the client's buffer in and is answered only after the
// bytes are durable, as `drivers::pmem` orders it. The range is never mapped into a client - direct access would put
// the persistence ordering and a poisoned line's machine check out of this driver's reach. A namespace a module will
// not save, or one on a platform that names no persistence domain, is served read-only, its writes refused.

#![no_std]
#![no_main]

use driver_protocol::block;
use drivers::pmem::{self, Durability, ReadOnly};
use drivers::{blk, common};
use rt::*;

// THE PROCESSOR'S HALF OF THE ORDERING: a line written back to memory, and a fence every earlier store and write-back
// completes before.
struct Processor;

impl pmem::Persist for Processor {
	fn flush(&mut self, address: usize) {
		// SAFETY: a write-back of a line of this process's own mapping; it changes no memory.
		#[cfg(target_arch = "x86_64")]
		unsafe {
			core::arch::asm!("clflush [{}]", in(reg) address, options(nostack, preserves_flags))
		};
		#[cfg(target_arch = "aarch64")]
		unsafe {
			core::arch::asm!("dc civac, {}", in(reg) address, options(nostack, preserves_flags))
		};
		// A RISC-V hart has no write-back instruction in its base set; the fence is the whole of what it can do.
		#[cfg(target_arch = "riscv64")]
		let _ = address;
	}

	fn fence(&mut self) {
		// SAFETY: a fence.
		#[cfg(target_arch = "x86_64")]
		unsafe {
			core::arch::asm!("mfence", options(nostack, preserves_flags))
		};
		#[cfg(target_arch = "aarch64")]
		unsafe {
			core::arch::asm!("dsb sy", options(nostack, preserves_flags))
		};
		#[cfg(target_arch = "riscv64")]
		unsafe {
			core::arch::asm!("fence rw, rw", options(nostack, preserves_flags))
		};
	}
}

// The line the processor writes back, as it says - its CLFLUSH line size - where it says one.
fn line() -> usize {
	#[cfg(target_arch = "x86_64")]
	{
		let said = ((core::arch::x86_64::__cpuid(1).ebx >> 8) & 0xff) as usize * 8;
		if said.is_power_of_two() {
			return said;
		}
	}
	pmem::LINE
}

// The namespace this driver serves.
struct Namespace {
	base: *mut u8,
	len: usize,
	plan: Result<Durability, ReadOnly>,
	line: usize,
	device: u64,
}

fn refuse(bootstrap: u64, bind: &common::Bind, what: &[u8]) -> ! {
	print(b"driver.nvdimm: ");
	print(what);
	print(b"\n");
	common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let Some(range) = bind.info.platform.mmio().first().copied() else { refuse(bootstrap, &bind, b"its row names no range") };
	if resources.device == 0 {
		refuse(bootstrap, &bind, b"it was not handed the range");
	}
	let mut block_bytes = [0u8; 64];
	let read = device_properties(resources.device, &mut block_bytes);
	let Some(record) = (read > 0).then(|| pmem::record(&block_bytes[..(read as usize).min(block_bytes.len())])).flatten() else { refuse(bootstrap, &bind, b"its row carries no namespace record") };
	if record.base != range.base || record.len != range.len {
		refuse(bootstrap, &bind, b"its record names another range than its row");
	}
	let Ok(len) = usize::try_from(range.len) else { refuse(bootstrap, &bind, b"the range does not fit this process") };
	let mapped: u64 = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) };
	if sys_is_err(mapped) {
		refuse(bootstrap, &bind, b"the range could not be mapped");
	}
	let namespace = Namespace { base: mapped as *mut u8, len, plan: pmem::plan(&record), line: line(), device: resources.device };
	let Some((server, client)) = channel() else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
	// ONE LINE: the size, the range, the modules, how a write is made durable or why there are none, and every state
	// flag a module reported.
	let mut report: common::Bounded<256> = common::Bounded::new();
	report.push(b"driver.nvdimm: online - ");
	report.decimal(range.len >> 20);
	report.push(b" MiB of persistent memory, range ");
	report.decimal(u64::from(record.index));
	report.push(b", ");
	report.decimal(u64::from(record.modules));
	report.push(b" module(s), ");
	report.push(match namespace.plan {
		Ok(Durability::Visible) => b"writes durable once visible",
		Ok(Durability::Flushed) => b"writes flushed to the memory controller",
		Err(ReadOnly::NotArmed) => b"read-only: a module is not armed",
		Err(ReadOnly::NoDomain) => b"read-only: the platform names no persistence domain",
	});
	for (flag, words) in pmem::STATE_WORDS {
		if record.state & flag != 0 {
			report.push(b", ");
			report.push(words);
		}
	}
	if !common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::BLOCK, client)]) {
		exit();
	}
	serve(bootstrap, &bind, &namespace, server)
}

// THE SLEEP, between two requests: every write was durable before it was answered, so nothing is outstanding, and the
// range is memory the platform keeps - there is nothing to bring back after it.
struct Sleep<'a> {
	serving: &'a mut common::Serving,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

fn reply(endpoint: u64, status: u32, transferred: u64) {
	send_blocking(endpoint, &block::reply(status), transferred);
}

fn serve(bootstrap: u64, bind: &common::Bind, namespace: &Namespace, server: u64) -> ! {
	let mut request = [0u8; block::REQUEST_LEN];
	let mut serving = common::Serving::new(server, 0);
	let sectors = namespace.len as u64 / pmem::SECTOR;
	common::takes_sleep();
	loop {
		let ready = common::serve_any_or_sleep(bootstrap, bind, &mut serving);
		if let Some(None) = ready
			&& common::take_sleep_step(bootstrap, bind, &mut Sleep { serving: &mut serving })
		{
			continue;
		}
		let Some(Some(at)) = ready.filter(|_| !common::stop_requested()) else {
			// NOTHING IS OUTSTANDING when the stop is read: every write was durable before its answer, and the range
			// masters nothing.
			common::finish_stop(bootstrap, bind, namespace.device, true);
			exit();
		};
		let endpoint = serving.at(at);
		let Some((len, handle)) = common::recv_from_consumer(bootstrap, bind, &mut serving, at, &mut request) else { continue };
		let Some(block::Request { op, lba, count }) = block::Request::decode(&request[..len]) else {
			if handle != 0 {
				close(handle);
			}
			reply(endpoint, block::STATUS_INVALID, 0);
			continue;
		};
		if matches!(op, block::OP_READ | block::OP_WRITE) && blk::request_range(lba, count, sectors, pmem::MAX_SECTORS).is_err() {
			if handle != 0 {
				close(handle);
			}
			reply(endpoint, block::STATUS_INVALID, 0);
			continue;
		}
		match op {
			block::OP_READ => serve_read(namespace, endpoint, lba, count),
			block::OP_WRITE => serve_write(namespace, endpoint, lba, count, handle),
			block::OP_CAPACITY => {
				send_blocking(endpoint, &block::capacity_reply(namespace.len as u64, pmem::MAX_SECTORS, block::DeviceClass::PersistentMemory), 0);
			}
			// EVERY WRITE WAS FLUSHED AND FENCED BEFORE ITS ANSWER, so a flush has nothing left to make durable; the fence
			// is run anyway, so the answer never depends on that having been true.
			block::OP_FLUSH => {
				pmem::Persist::fence(&mut Processor);
				reply(endpoint, block::STATUS_OK, 0);
			}
			_ => {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_ERR, 0);
			}
		}
	}
}

fn serve_read(namespace: &Namespace, endpoint: u64, lba: u64, count: u32) {
	let Some((offset, bytes)) = pmem::span(lba, count, namespace.len as u64) else {
		reply(endpoint, block::STATUS_INVALID, 0);
		return;
	};
	let object: u64 = unsafe { syscall(SYS_MEMORY_OBJECT_CREATE, bytes as u64, 0, 0, 0) };
	if sys_is_err(object) {
		reply(endpoint, block::STATUS_ERR, 0);
		return;
	}
	let Some(mapped) = (unsafe { map_object(object) }) else {
		close(object);
		reply(endpoint, block::STATUS_ERR, 0);
		return;
	};
	// SAFETY: the object was created `bytes` long and is mapped here alone; the range is this driver's mapping.
	let moved = unsafe { pmem::read(namespace.base, namespace.len, offset, core::slice::from_raw_parts_mut(mapped as *mut u8, bytes)) };
	unmap_object(object);
	if !moved {
		close(object);
		reply(endpoint, block::STATUS_ERR, 0);
		return;
	}
	let granted = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
	close(object);
	if granted < 0 {
		reply(endpoint, block::STATUS_ERR, 0);
		return;
	}
	reply(endpoint, block::STATUS_OK, granted as u64);
}

fn serve_write(namespace: &Namespace, endpoint: u64, lba: u64, count: u32, handle: u64) {
	if handle == 0 {
		reply(endpoint, block::STATUS_INVALID, 0);
		return;
	}
	// A READ-ONLY NAMESPACE REFUSES THE WRITE AND KEEPS SERVING READS: its contents are there, and a write would be
	// answered as saved when it would not be.
	let Ok(durability) = namespace.plan else {
		close(handle);
		reply(endpoint, block::STATUS_INVALID, 0);
		return;
	};
	let Some((offset, bytes)) = pmem::span(lba, count, namespace.len as u64) else {
		close(handle);
		reply(endpoint, block::STATUS_INVALID, 0);
		return;
	};
	let Some(info) = object_info(handle) else {
		close(handle);
		reply(endpoint, block::STATUS_INVALID, 0);
		return;
	};
	if blk::write_source(&info, bytes as u64).is_err() {
		close(handle);
		reply(endpoint, block::STATUS_INVALID, 0);
		return;
	}
	let Some(mapped) = (unsafe { map_object(handle) }) else {
		close(handle);
		reply(endpoint, block::STATUS_ERR, 0);
		return;
	};
	// SAFETY: the object holds at least `bytes`, as checked; the range is this driver's mapping and this loop is its one
	// writer.
	let written = unsafe { pmem::write_durably(namespace.base, namespace.len, offset, core::slice::from_raw_parts(mapped as *const u8, bytes), durability, namespace.line, &mut Processor) };
	unmap_object(handle);
	close(handle);
	reply(endpoint, if written { block::STATUS_OK } else { block::STATUS_ERR }, 0);
}
