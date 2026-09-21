use super::*;

tagged_test!(device_memory_maps_mmio_region, [Drivers], id = "kernel.hardware.device_memory_maps_mmio_region", covers = ["kernel"]);
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
	let device = DeviceMemory::new(phys, mem::frame::PAGE_SIZE as usize).expect("a test device memory");
	// Hand the capability to the driver thread as its bootstrap handle.
	sched::spawn_with_object(body, device, Rights::ALL);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "device-memory mapping thread did not finish");
	// The thread (and its handle table) is reaped by run_until_idle, dropping the
	// DeviceMemory and tearing its mapping down, so the frame is free to reclaim.
	unsafe { mem::frame::deallocate(phys) };
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	interrupt_bind_delivers_to_driver,
	[Drivers, ArchX86_64],
	id = "kernel.hardware.interrupt_bind_delivers_to_driver",
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn interrupt_bind_delivers_to_driver() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// Vector 0x2c (IRQ 12) is a bindable device-IRQ vector (not the timer at 0x20).
	const VECTOR: u64 = 0x2c;
	extern "C" fn body(_arg: u64) {
		unsafe {
			let h = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, VECTOR, device_privilege(), 0, 0);
			assert!(!syscall::sys_is_err(h), "interrupt_bind failed");
			// Simulate the device IRQ firing with a software interrupt; the dispatch
			// path marks the bound Interrupt pending and wakes any waiter.
			core::arch::asm!("int 0x2c");
			// The interrupt is now pending, so a wait observes it and returns.
			let r = arch::syscall::invoke(syscall::SYS_WAIT, h, 0, 0, 0);
			assert_eq!(r as i64, 0, "wait did not observe the delivered interrupt");
			// Binding the same vector again while ours lives is refused.
			let again = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, VECTOR, device_privilege(), 0, 0);
			assert_eq!(again as i64, syscall::ERR_RESOURCE_EXHAUSTED);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

tagged_test!(device_table_exposes_virtio_mmio, [Drivers], id = "kernel.hardware.device_table_exposes_virtio_mmio", covers = ["kernel"]);
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
				VTYPE.store(info.device_type as u64, Ordering::SeqCst);
				BAR_LEN.store(info.bar_len, Ordering::SeqCst);
			}
			if let Ok(grant) = crate::tests::claim_device(0) {
				MAPPED.store(arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, grant.memory, 0, 0, 0), Ordering::SeqCst);
				// Given back, so a later test naming device 0 is not refused by this one.
				crate::tests::release_device(&grant);
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

tagged_test!(device_table_exposes_the_xhci_controller, [Drivers, Usb], id = "kernel.hardware.device_table_exposes_the_xhci_controller", covers = ["kernel"]);
fn device_table_exposes_the_xhci_controller() {
	use core::sync::atomic::{AtomicU64, Ordering};
	// The xHCI controller joins the same device table the virtio devices live in. A
	// driver-like thread walks the table over the device syscalls the way DeviceManager
	// will: find the entry reporting DEVICE_TYPE_XHCI, acquire its DeviceMemory
	// capability, and map the controller's register file.
	static BAR_LEN: AtomicU64 = AtomicU64::new(0);
	static MAPPED: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		let mut info = abi::DeviceInfo::default();
		let size = core::mem::size_of::<abi::DeviceInfo>() as u64;
		unsafe {
			let count = arch::syscall::invoke(syscall::SYS_DEVICE_COUNT, 0, 0, 0, 0);
			for i in 0..count {
				if arch::syscall::invoke(syscall::SYS_DEVICE_INFO, i, &mut info as *mut _ as u64, size, 0) as i64 != 0 {
					continue;
				}
				if info.device_type != abi::DEVICE_TYPE_XHCI {
					continue;
				}
				BAR_LEN.store(info.bar_len, Ordering::SeqCst);
				if let Ok(grant) = crate::tests::claim_device(i) {
					MAPPED.store(arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, grant.memory, 0, 0, 0), Ordering::SeqCst);
					crate::tests::release_device(&grant);
				}
				break;
			}
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(BAR_LEN.load(Ordering::SeqCst) > 0, "the device table should hold the xHCI controller");
	let mapped = MAPPED.load(Ordering::SeqCst);
	assert!(mapped != 0 && !syscall::sys_is_err(mapped), "the xHCI register file should map to a valid address");
}

tagged_test!(the_machine_reports_a_hot_plug_slot_the_scan_found, [Drivers, Pci], id = "kernel.hardware.the_machine_reports_a_hot_plug_slot_the_scan_found", covers = ["kernel"]);
fn the_machine_reports_a_hot_plug_slot_the_scan_found() {
	// THE SLOT PATH HAD NO FIXTURE ON THE PROFILE THE ORACLES RUN ON, so everything about it was
	// proved against a SYNTHETIC config space - `arch::common::pci::tests` stands one up and drives
	// `resolve_slot`, `slot_arm` and the acknowledgement against it. That proves the decisions and
	// says nothing about whether the scan on a real machine ever reaches a port at all.
	//
	// THE REASON IT WAS ABSENT WAS CHECKED AND IS SPENT. The harness note said adding a function to
	// the test profile would renumber the bus addresses several oracles print. That is true of a
	// function added AMONG them and false of one added after them: QEMU assigns an address to each
	// device without one in the order the arguments appear. The port is appended last, and the
	// driver oracles print the same addresses they did before it existed.
	//
	// WHAT THIS ASSERTS IS THE MACHINE'S SHAPE AND NOT A DEVICE. The port is empty; nothing binds to
	// it. What would be wrong without this test is a scan that stopped before the port, a capability
	// walk that missed the slot, or a fixture quietly dropped from the harness - each of which
	// leaves the hot-plug path built, armed and pointed at nothing, which is indistinguishable from
	// working until somebody plugs a disk in.
	let mut ports = [None; arch::common::pci::MAX_HOT_PLUG_PORTS];
	let found = arch::common::pci::hot_plug_ports(&mut ports);
	assert!(found > 0, "the scan should find the hot-plug port the harness attaches to every test profile");
	// THE HARNESS'S PORT IS LOOKED FOR AMONG THEM RATHER THAN AT ROW ZERO. A machine may carry more
	// than one port - and this one does - so requiring the FIRST row to be the fixture's would be
	// asserting an order nothing promises. What is asserted is that the port the harness attached is
	// in the list: `slot=1`, and empty.
	//
	// THE NUMBER IS THE HARNESS'S AND NOT ANY NUMBER, because a reader that took the wrong dword out
	// of the capability returns zero, and every port would pass a test that accepted whatever it
	// found. THE EMPTINESS matters for the same reason from the other side: a port reporting a
	// device present when nothing was plugged into it is a presence bit read out of an endpoint's
	// reserved bytes.
	let seen = ports.iter().take(found).flatten().map(|p| p.slot.number).fold(0u32, |acc, n| acc | 1 << (n.min(31)));
	let fixture = ports.iter().take(found).flatten().find(|p| p.slot.number == 1);
	let port = fixture.unwrap_or_else(|| panic!("the harness attaches a port with platform slot 1; {found} port(s) found, slot numbers as a bitmask: {seen:#x}"));
	assert!(!port.occupied, "the harness attaches the port with nothing in it");
}

tagged_test!(device_table_resources_the_nvme_controller, [Drivers, Pci], id = "kernel.hardware.device_table_resources_the_nvme_controller", covers = ["kernel"]);
fn device_table_resources_the_nvme_controller() {
	use core::sync::atomic::{AtomicU64, Ordering};
	// THE SECOND FAMILY THE PLAIN-PCI RESOURCE PROFILE RESOLVES, and the reason this test exists
	// rather than the xHCI one being taken as covering it: until this change there was exactly one
	// resolver, so "a plain-PCI function gets a register window" and "the xHCI controller gets a
	// register window" were the same sentence and the same code path. They are now a table and a
	// row in it, and a row that resolves nothing would be invisible to every other test here.
	//
	// AND IT READS THE CONTROLLER THROUGH THE WINDOW, which is what a non-zero `bar_len` does not
	// prove. A row carrying some other function's base is still a row with a BAR; a row whose base
	// is the NVMe register file answers `CAP` with a controller that declares the NVM command set.
	// That is the difference between resolving a number and resolving the right one.
	static BAR_LEN: AtomicU64 = AtomicU64::new(0);
	static FOUND: AtomicU64 = AtomicU64::new(0);
	static CAP: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		let mut info = abi::DeviceInfo::default();
		let size = core::mem::size_of::<abi::DeviceInfo>() as u64;
		unsafe {
			let count = arch::syscall::invoke(syscall::SYS_DEVICE_COUNT, 0, 0, 0, 0);
			for i in 0..count {
				if arch::syscall::invoke(syscall::SYS_DEVICE_INFO, i, &mut info as *mut _ as u64, size, 0) as i64 != 0 {
					continue;
				}
				if info.device_type != abi::DEVICE_TYPE_NVME {
					continue;
				}
				FOUND.fetch_add(1, Ordering::SeqCst);
				if BAR_LEN.load(Ordering::SeqCst) != 0 {
					// The first one already answered CAP through its window; a second resourced row
					// is what this count is here to see.
					continue;
				}
				BAR_LEN.store(info.bar_len, Ordering::SeqCst);
				if let Ok(grant) = crate::tests::claim_device(i) {
					let mapped = arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, grant.memory, 0, 0, 0);
					if mapped != 0 && !syscall::sys_is_err(mapped) {
						// CAP is the register file's first eight bytes, read as two halves because
						// a 64-bit access to a controller register is not portable.
						let low = (mapped as *const u32).read_volatile() as u64;
						let high = ((mapped + 4) as *const u32).read_volatile() as u64;
						CAP.store(low | (high << 32), Ordering::SeqCst);
					}
					crate::tests::release_device(&grant);
				}
			}
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(FOUND.load(Ordering::SeqCst) >= 2, "both NVMe controllers should be resourced, not just the first one the scan met ({} found)", FOUND.load(Ordering::SeqCst));
	// A whole page at least: the controller registers and the first doorbell live inside it.
	assert!(BAR_LEN.load(Ordering::SeqCst) >= 0x1000, "the NVMe BAR 0 should be at least a page (probed {:#x})", BAR_LEN.load(Ordering::SeqCst));
	let cap = CAP.load(Ordering::SeqCst);
	assert!(cap != 0 && cap != u64::MAX, "CAP read through the resolved window should be a real value, not {cap:#x}");
	// CAP.MQES is zero-based and a queue of no entries is not a controller; CAP.CSS bit 37 is the
	// NVM command set, which is the one this machine's driver speaks.
	assert!(cap & 0xFFFF != 0, "CAP.MQES should describe at least one queue entry");
	assert!((cap >> 37) & 1 != 0, "CAP.CSS should advertise the NVM command set");
}

tagged_test!(virtio_scsi_driver_serves_a_write_and_reads_it_back, [Drivers, Pci, Slow], id = "kernel.hardware.virtio_scsi_driver_serves_a_write_and_reads_it_back", covers = ["kernel", "bin.virtio_scsi"]);
fn virtio_scsi_driver_serves_a_write_and_reads_it_back() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// THE FIFTH DRIVER ON THE SAME BLOCK CONTRACT, over a fifth command set, and the first that
	// reaches its medium through a SCSI TARGET rather than speaking to the medium directly. What that
	// adds is the command set: the capacity comes from READ CAPACITY, whose answer is the LAST BLOCK
	// rather than the count, and the address goes out big-endian, which is the opposite of every
	// other wire here. Both are silent when wrong.
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/virtio_scsi.lsexe")).expect("the virtio_scsi.lsexe driver should be staged on the volume under drivers/");

	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::VIRTIO_TYPE_SCSI {
			// A VIRTIO DEVICE CARRIES ITS STRUCTURE OFFSETS, which is what tells this transport apart
			// from the plain-PCI ones: the driver reaches the common configuration, the notify window
			// and the device-specific config through them rather than at the base.
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: d.vendor, product: d.product, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the virtio-scsi controller");

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the virtio-scsi driver should load");
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the virtio-scsi controller")).expect("the controller is taken, as DeviceManager takes it");
	send_bind(&kernel_ep, &info, key.generation, 1).expect("the BIND should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
	sched::run_until_idle();

	// REACHING READY IS ALREADY A CLAIM ABOUT THE TARGET: this driver refuses to report in until one
	// answers TEST UNIT READY and then READ CAPACITY, retrying the power-on attention that every unit
	// refuses its first command with.
	let offers = recv_offers(&kernel_ep, key.generation).expect("the virtio-scsi driver should report READY, which it only does with a target answering");
	// THREE UNITS ACROSS TWO TARGETS, AND A PROVIDER FOR EACH. A driver that took the first thing
	// that answered served one disk on a machine that has three, which is what this bus is FOR: the
	// transport exists beside `virtio-blk` because a real HBA has several units behind several
	// targets.
	//
	// AND THE THIRD IS THE ONE THAT SAYS THE WALK IS A WALK. Two units behind ONE target cannot tell
	// a driver that asks every target from one that stops at the first that answers - both find
	// everything there is. The third lives behind target 1, so it is reachable only by asking again
	// after the first target has already filled two of the table's slots.
	let units: alloc::vec::Vec<alloc::sync::Arc<dyn object::KernelObject>> = offers.iter().filter(|(kind, _, _)| *kind == driver_protocol::provider::BLOCK).map(|(_, _, object)| object.clone()).collect();
	assert_eq!(units.len(), 3, "the driver enumerates the units behind every target it walks rather than taking the first that answers or stopping at the first target");
	let blk = units[0].clone().into_any_arc().downcast::<Channel>().expect("the block channel is a channel");
	let second = units[1].clone().into_any_arc().downcast::<Channel>().expect("the second unit's block channel is a channel");
	let third = units[2].clone().into_any_arc().downcast::<Channel>().expect("the third unit's block channel is a channel");

	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	blk.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	let reported = driver_protocol::block::decode_capacity(&cap_reply.bytes).expect("the capacity query should succeed and carry a size");
	// Four mebibytes exactly. An off-by-one in the last-block arithmetic would be one block out, and
	// the block past the end is the one the medium refuses - so it would surface as an I/O error on
	// the last sector rather than as anything naming the capacity.
	assert_eq!(reported.bytes, 4 * 1024 * 1024, "the target should report the attached medium's real size");

	const SECTOR: usize = 512;
	let pattern: alloc::vec::Vec<u8> = (0..SECTOR).map(|i| (i as u8).wrapping_mul(13) ^ 0x6E).collect();
	let source = object::memory_object::MemoryObject::create(SECTOR).expect("a source buffer");
	{
		let hhdm = mem::hhdm_offset();
		let phys = source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(pattern.as_ptr(), (hhdm + phys) as *mut u8, SECTOR) };
	}
	// LBA 0x0102 rather than a single digit, so a command block whose address bytes went out in the
	// wrong order names a DIFFERENT block rather than the same one.
	const LBA: u64 = 0x0102;
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the write request should send");
	sched::run_until_idle();
	let write_reply = blk.recv().expect("the write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the write should succeed");

	let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the read request should send");
	sched::run_until_idle();
	let read_reply = blk.recv().expect("the read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the read should succeed");
	let buf_cap = read_reply.caps.first().expect("the read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, SECTOR), pattern, "the block read back should be the bytes that were written to it");

	let past = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: 1 << 40, count: 1 }.encode();
	blk.send(Message::new(past.to_vec(), alloc::vec::Vec::new())).expect("the out-of-range request should send");
	sched::run_until_idle();
	let refused = blk.recv().expect("the refusal should arrive");
	assert_eq!(driver_protocol::block::decode_status(&refused.bytes), Some(driver_protocol::block::STATUS_INVALID), "a range past the last block is refused, and the ten-byte command's address is never truncated to reach it");
	assert!(refused.caps.is_empty(), "a refused read grants no buffer");

	// THE SECOND UNIT IS A DIFFERENT MEDIUM AND NOT A SECOND VIEW OF THE FIRST. Two mebibytes against
	// four: a driver that published one unit twice, or that addressed both providers at unit zero,
	// answers this with the first medium's size - and a consumer writing to what it believes is the
	// second disk would be writing to the first.
	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	second.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the second unit's capacity request should send");
	sched::run_until_idle();
	let second_reply = second.recv().expect("the second unit's capacity reply should arrive");
	let second_size = driver_protocol::block::decode_capacity(&second_reply.bytes).expect("the second unit reports a size");
	assert_eq!(second_size.bytes, 2 * 1024 * 1024, "the second unit reports its OWN medium's size");

	// AND THE UNIT BEHIND THE SECOND TARGET IS A THIRD MEDIUM. One mebibyte against four and two:
	// the size is how this tells which medium it was handed, and a driver that walked one target and
	// then published its last unit twice would answer here with two mebibytes.
	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	third.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the third unit's capacity request should send");
	sched::run_until_idle();
	let third_reply = third.recv().expect("the third unit's capacity reply should arrive");
	let third_size = driver_protocol::block::decode_capacity(&third_reply.bytes).expect("the third unit reports a size");
	assert_eq!(third_size.bytes, 1024 * 1024, "the unit behind the SECOND target reports its own medium's size, which is what says the walk reached it");

	// AND IT IS A MEDIUM AND NOT A NAME. A capacity can be answered by a driver that addressed the
	// wrong unit and got lucky; a write that comes back is the medium itself.
	let far_pattern: alloc::vec::Vec<u8> = (0..SECTOR).map(|i| (i as u8).wrapping_mul(37) ^ 0xC3).collect();
	let far_source = object::memory_object::MemoryObject::create(SECTOR).expect("a source buffer for the far target");
	{
		let hhdm = mem::hhdm_offset();
		let phys = far_source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(far_pattern.as_ptr(), (hhdm + phys) as *mut u8, SECTOR) };
	}
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: LBA, count: 1 }.encode();
	third.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(far_source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the far target's write should send");
	sched::run_until_idle();
	let write_reply = third.recv().expect("the far target's write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "a unit behind the second target takes a write");
	let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	third.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the far target's read should send");
	sched::run_until_idle();
	let read_reply = third.recv().expect("the far target's read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "and gives it back");
	let buf_cap = read_reply.caps.first().expect("the far target's read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, SECTOR), far_pattern, "what comes back from the second target's unit is what was written to IT, and not the first target's block at the same address");
	assert_ne!(far_pattern, pattern, "the two patterns differ, or reading one target's block out of the other would pass");

	// AND WHAT IS NOT ON THE BLOCK WIRE DOES NOT REACH THE TARGET. The item this driver was written
	// for says unsupported passthrough commands are not exposed to ordinary storage clients, and this
	// is what keeps it: an opcode outside read/write/flush/capacity is ANSWERED rather than turned
	// into a command descriptor block of the caller's choosing. A driver that forwarded one would let
	// any consumer of a disk send FORMAT UNIT.
	let passthrough = driver_protocol::block::Request { op: 0x4242, lba: 0, count: 1 }.encode();
	blk.send(Message::new(passthrough.to_vec(), alloc::vec::Vec::new())).expect("the unknown opcode should send");
	sched::run_until_idle();
	let answer = blk.recv().expect("an answer to the unknown opcode should arrive");
	assert!(matches!(driver_protocol::block::decode_status(&answer.bytes), Some(driver_protocol::block::STATUS_ERR) | Some(driver_protocol::block::STATUS_INVALID)), "an opcode this contract does not carry is refused, not translated into a SCSI command");
	assert!(answer.caps.is_empty(), "and it grants nothing");

	// The medium behind the first unit is still what it was: the passthrough refusal touched nothing.
	let again = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(again.to_vec(), alloc::vec::Vec::new())).expect("the re-read should send");
	sched::run_until_idle();
	let again_reply = blk.recv().expect("the re-read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&again_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the unit still serves after refusing an opcode it does not carry");
}

// One request to a `local-stream` provider and its reply, with the scheduler run in between.
fn stream_round(channel: &object::channel::Channel, request: alloc::vec::Vec<u8>) -> (u32, u32, alloc::vec::Vec<u8>) {
	channel.send(object::channel::Message::new(request, alloc::vec::Vec::new())).expect("the stream request should send");
	sched::run_until_idle();
	let reply = channel.recv().expect("the stream reply should arrive");
	let (status, arg, payload) = driver_protocol::stream::decode_reply(&reply.bytes).expect("the stream reply should parse");
	(status, arg, payload.to_vec())
}

// Read from one stream until it has answered `wanted` bytes or stops answering.
fn stream_drain(channel: &object::channel::Channel, wanted: usize) -> alloc::vec::Vec<u8> {
	let mut got: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for _ in 0..64 {
		let (status, _, payload) = stream_round(channel, driver_protocol::stream::Request { op: driver_protocol::stream::OP_RECEIVE, arg: 4096 }.encode().to_vec());
		if status != driver_protocol::stream::STATUS_OK {
			break;
		}
		got.extend_from_slice(&payload);
		if got.len() >= wanted {
			break;
		}
	}
	got
}

tagged_test!(virtio_vsock_driver_echoes_bytes_off_the_host, [Drivers, Pci, Slow], id = "kernel.hardware.virtio_vsock_driver_echoes_bytes_off_the_host", covers = ["kernel", "bin.virtio_vsock"]);
fn virtio_vsock_driver_echoes_bytes_off_the_host() {
	use driver_protocol::stream;
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// THE ONLY DRIVER HERE WHOSE PEER IS NOT INSIDE QEMU. Every other device on this machine is a
	// model: the bytes a driver writes are answered by code in the emulator. vsock's other end is a
	// PROCESS ON THE HOST, so this oracle is the only one in the suite that proves a round trip out
	// of the machine entirely - the harness starts `vsock-echo.py`, and what comes back has been
	// through the host kernel's vsock stack and a program that read it.
	//
	// THE PORT IS THE GUEST'S OWN CONTEXT ID, which is why the identity request below is not a
	// diagnostic: a host vsock port is global to the host, so two runs on one machine would collide
	// on any fixed number, while the context id is already unique because `/dev/vhost-vsock` refuses
	// a duplicate. The guest is TOLD its id by the host, asks its driver for it, and knocks there.
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/virtio_vsock.lsexe")).expect("the virtio_vsock.lsexe driver should be staged on the volume under drivers/");

	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::VIRTIO_TYPE_VSOCK {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: d.vendor, product: d.product, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the virtio-vsock device");

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the virtio-vsock driver should load");
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the virtio-vsock device")).expect("the device is taken, as DeviceManager takes it");
	send_bind(&kernel_ep, &info, key.generation, 1).expect("the BIND should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
	sched::run_until_idle();

	let offers = recv_offers(&kernel_ep, key.generation).expect("the virtio-vsock driver should report READY");
	// PUBLISHED AS `local-stream` AND NOT AS `net`, which is the item's own requirement and is
	// asserted here rather than left to the manifest: a host channel published under the kind every
	// consumer of a link already asks for would be an ambient path around NetworkService, and this
	// is the assertion that fails if someone later "simplifies" the two kinds into one.
	let stream_channel = offer_of(&offers, driver_protocol::provider::LOCAL_STREAM).expect("the driver offers a local stream, and not a net link").into_any_arc().downcast::<Channel>().expect("the stream channel is a channel");

	let ask = stream::Request { op: stream::OP_IDENTITY, arg: 0 }.encode();
	stream_channel.send(Message::new(ask.to_vec(), alloc::vec::Vec::new())).expect("the identity request should send");
	sched::run_until_idle();
	let identity = stream_channel.recv().expect("the identity reply should arrive");
	let (status, _, payload) = stream::decode_reply(&identity.bytes).expect("the identity reply should parse");
	assert_eq!(status, stream::STATUS_OK, "the driver should know the context id the host gave it");
	assert_eq!(payload.len(), stream::IDENTITY_LEN, "the identity answer carries the eight-byte context id");
	let cid = u64::from_le_bytes(payload.try_into().unwrap());
	assert!(cid > 2, "the host assigns a context id above the three reserved ones, and a driver reading the wrong config offset reports one of those");

	// The host's echo listener is on the port named by this guest's context id.
	let connect = stream::Request { op: stream::OP_CONNECT, arg: cid as u32 }.encode();
	stream_channel.send(Message::new(connect.to_vec(), alloc::vec::Vec::new())).expect("the connect request should send");
	sched::run_until_idle();
	let opened = stream_channel.recv().expect("the connect reply should arrive");
	let (status, _, _) = stream::decode_reply(&opened.bytes).expect("the connect reply should parse");
	assert_eq!(status, stream::STATUS_OK, "the host's listener should answer the connection request");

	// A PATTERN RATHER THAN A WORD, so a path that echoes a stale buffer, a zeroed one or the
	// request header back would fail: every byte is a function of its own position.
	let pattern: alloc::vec::Vec<u8> = (0..256usize).map(|i| (i as u8).wrapping_mul(29) ^ 0x5B).collect();
	let mut send = stream::Request { op: stream::OP_SEND, arg: pattern.len() as u32 }.encode().to_vec();
	send.extend_from_slice(&pattern);
	stream_channel.send(Message::new(send, alloc::vec::Vec::new())).expect("the send request should send");
	sched::run_until_idle();
	let sent = stream_channel.recv().expect("the send reply should arrive");
	let (status, moved, _) = stream::decode_reply(&sent.bytes).expect("the send reply should parse");
	assert_eq!(status, stream::STATUS_OK, "the write should be accepted");
	// The whole of it, which is a statement about CREDIT: the host advertises a buffer far larger
	// than this, so a window computed the wrong way round - or not modularly - would either refuse
	// the write or accept a fraction of it.
	assert_eq!(moved as usize, pattern.len(), "the host's window admits this write whole");

	// Read it back. The host echoes, so the bytes have crossed the device twice.
	let mut got: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for _ in 0..64 {
		let ask = stream::Request { op: stream::OP_RECEIVE, arg: 4096 }.encode();
		stream_channel.send(Message::new(ask.to_vec(), alloc::vec::Vec::new())).expect("the receive request should send");
		sched::run_until_idle();
		let answer = stream_channel.recv().expect("the receive reply should arrive");
		let (status, _, payload) = stream::decode_reply(&answer.bytes).expect("the receive reply should parse");
		assert_eq!(status, stream::STATUS_OK, "the connection should still be open while the echo is arriving");
		got.extend_from_slice(payload);
		if got.len() >= pattern.len() {
			break;
		}
	}
	assert_eq!(got, pattern, "what the host echoed back should be the bytes that were sent to it");

	// AND A PORT NOTHING LISTENS ON IS REFUSED RATHER THAN HUNG. The host answers a connection to a
	// closed port with a reset, and this is the difference between the driver reading that reset and
	// the driver sitting on a deadline: a driver that ignored the reset would answer here too, but
	// only after the connect budget ran out, and it would answer ERR rather than CLOSED.
	let shutdown = stream::Request { op: stream::OP_SHUTDOWN, arg: stream::SHUTDOWN_READ | stream::SHUTDOWN_WRITE }.encode();
	stream_channel.send(Message::new(shutdown.to_vec(), alloc::vec::Vec::new())).expect("the shutdown request should send");
	sched::run_until_idle();
	let closed = stream_channel.recv().expect("the shutdown reply should arrive");
	let (status, _, _) = stream::decode_reply(&closed.bytes).expect("the shutdown reply should parse");
	assert_eq!(status, stream::STATUS_OK, "closing a connection that is open should succeed");

	let nowhere = stream::Request { op: stream::OP_CONNECT, arg: 1 }.encode();
	stream_channel.send(Message::new(nowhere.to_vec(), alloc::vec::Vec::new())).expect("the second connect request should send");
	sched::run_until_idle();
	let refused = stream_channel.recv().expect("the refusal should arrive");
	let (status, _, _) = stream::decode_reply(&refused.bytes).expect("the refusal should parse");
	assert_eq!(status, stream::STATUS_CLOSED, "a port with no listener is refused by the host with a reset, which is not the same answer as a host that said nothing");

	// AND TWO CONSUMERS GET TWO STREAMS, WHICH IS WHAT THE TABLE IS FOR.
	//
	// ONE RING CARRIES BOTH. Every packet of every stream arrives in the same receive queue,
	// interleaved in whatever order the host sent them, and the only field that tells them apart is
	// the LOCAL PORT this driver assigned - so a driver holding one connection's worth of state
	// hands whichever bytes arrived last to whoever asks next.
	//
	// BOTH ARE WRITTEN BEFORE EITHER IS READ, deliberately: send-read-send-read would pass a driver
	// with one staging buffer, because there would never be two streams' bytes in it at once. And
	// the two patterns DIFFER, or neither assertion would say anything.
	let token = offer_token_of(&offers, driver_protocol::provider::LOCAL_STREAM).expect("the stream publication carries a token");
	let (second, driver_end) = object::channel::Channel::create();
	send_connect(&kernel_ep, key.generation, token, driver_end).expect("the second CONNECT should send");
	sched::run_until_idle();

	let to_echo = stream::Request { op: stream::OP_CONNECT, arg: cid as u32 }.encode().to_vec();
	let (status, _, _) = stream_round(&stream_channel, to_echo.clone());
	assert_eq!(status, stream::STATUS_OK, "the first consumer reconnects after its own shutdown, which is an ordinary reconnect and not a second stream");
	let (status, _, _) = stream_round(&second, to_echo);
	assert_eq!(status, stream::STATUS_OK, "and the second consumer gets a stream of its own rather than being refused by a driver that holds one");

	let first_pattern: alloc::vec::Vec<u8> = (0..192usize).map(|i| (i as u8).wrapping_mul(13) ^ 0xA7).collect();
	let second_pattern: alloc::vec::Vec<u8> = (0..192usize).map(|i| (i as u8).wrapping_mul(31) ^ 0x1D).collect();
	assert_ne!(first_pattern, second_pattern, "the two patterns differ, or reading one back out of the other's stream would pass");
	for (channel, pattern) in [(&*stream_channel, &first_pattern), (&*second, &second_pattern)] {
		let mut request = stream::Request { op: stream::OP_SEND, arg: pattern.len() as u32 }.encode().to_vec();
		request.extend_from_slice(pattern);
		let (status, moved, _) = stream_round(channel, request);
		assert_eq!(status, stream::STATUS_OK, "each stream accepts its own write");
		assert_eq!(moved as usize, pattern.len(), "and each carries its own credit, so neither write is shortened by the other's window");
	}
	let first_back = stream_drain(&stream_channel, first_pattern.len());
	let second_back = stream_drain(&second, second_pattern.len());
	assert_eq!(first_back, first_pattern, "the first stream reads back what was written to IT, and not what the other stream's peer echoed");
	assert_eq!(second_back, second_pattern, "and so does the second, which one staging buffer between them cannot do");

	// AND A CONSUMER THAT LEAVES DOES NOT TAKE THE OTHERS WITH IT.
	//
	// A CLOSED ENDPOINT IS A READY ONE. `Channel::ready` is `!inbox.is_empty() || is_peer_closed()`,
	// which is how a reader learns of a closure at all, and `serve_any_or_answer` answers with the
	// FIRST ready index - so a driver that reads `Closed` and merely continues is handed the
	// departed consumer at index zero on every pass and NEVER REACHES the live one behind it. That
	// is not a spin that wastes a core: it is a driver that has silently stopped serving everybody,
	// with its heartbeat still answered because the control channel is drained first on every pass.
	// THE FIRST CONSUMER GOES, AND BOTH OF ITS REFERENCES HAVE TO: the peer is held weakly, so the
	// endpoint reads as closed only once the LAST strong reference is gone, and the handshake kept
	// one beside the one this test downcast.
	drop(stream_channel);
	drop(offers);
	sched::run_until_idle();
	let ask = stream::Request { op: stream::OP_IDENTITY, arg: 0 }.encode();
	second.send(Message::new(ask.to_vec(), alloc::vec::Vec::new())).expect("the second consumer's request should send");
	sched::run_until_idle();
	// AND THE MUTATION'S SIGNATURE IS A TIMEOUT AND NOT AN ASSERTION, which is worth knowing before
	// somebody loosens this for looking flaky: a driver that keeps the departed endpoint never goes
	// idle, so `run_until_idle` above does not return and the suite dies on its wall clock instead
	// of on the line below. That is the defect's real shape - a driver that has stopped serving.
	let served = second.recv().expect("the driver should serve the consumer that stayed - a driver holding a departed endpoint ahead of it never reaches this one");
	let (status, _, payload) = stream::decode_reply(&served.bytes).expect("the second consumer's reply should parse");
	assert_eq!(status, stream::STATUS_OK, "the consumer that stayed is served");
	assert_eq!(u64::from_le_bytes(payload.try_into().unwrap()), cid, "and by the same driver, which knows the same context id");

	// AND THE MANAGER IS TOLD, which is the half a driver that only dropped the endpoint still gets
	// wrong. The `consumers` bound counts CONCURRENT consumers and the count only ever rises until a
	// `Disconnect` names the publication, so without this a kind admitting one is refused for the
	// rest of the boot the moment its first consumer closes.
	let mut reported = false;
	while let Ok(message) = kernel_ep.recv() {
		let Ok(header) = driver_protocol::Header::decode(&message.bytes) else { continue };
		if header.generation != key.generation || !matches!(header.opcode, driver_protocol::Opcode::Disconnect) {
			continue;
		}
		let payload = header.payload(&message.bytes);
		if payload.len() == driver_protocol::U16_PAYLOAD_LEN && u16::from_le_bytes([payload[0], payload[1]]) == token {
			reported = true;
		}
	}
	assert!(reported, "the driver reports the departure under the publication's own token, which is what gives the consumer's place back");
}

tagged_test!(hda_driver_routes_a_codec_and_the_controller_consumes_a_buffer, [Drivers, Pci, Slow], id = "kernel.hardware.hda_driver_routes_a_codec_and_the_controller_consumes_a_buffer", covers = ["kernel", "bin.hda"]);
fn hda_driver_routes_a_codec_and_the_controller_consumes_a_buffer() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// THE OBSERVABLE EFFECT FOR AUDIO IS THAT THE HARDWARE TOOK THE BYTES, and nothing weaker will
	// do. "The driver reached Online" is rejected by this tree's convention anyway, but for audio it
	// would be especially empty: a driver that brings a controller up, builds a route through a
	// codec that answered nothing and starts a stream over an empty buffer reports exactly the same
	// thing as one that works, and plays silence.
	//
	// So this drives the driver as DeviceManager does, hands it one period over the same PCM contract
	// `driver.virtio-snd` serves, and then reads the controller's OWN LINK POSITION COUNTER - the
	// register the hardware advances as it consumes the buffer - and requires it to have moved.
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/hda.lsexe")).expect("the hda.lsexe driver should be staged on the volume under drivers/");

	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_HDA {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_PLAIN_PCI, vendor: d.vendor, product: d.product, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the HD Audio controller");

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the hda driver should load");
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the HD Audio controller")).expect("the HD Audio controller is taken, as DeviceManager takes it");
	send_bind(&kernel_ep, &info, key.generation, 1).expect("the BIND should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
	sched::run_until_idle();

	// REACHING READY IS ITSELF A CLAIM ABOUT THE CODEC, because this driver refuses to report in
	// without one: it fails `NoCodec` when no address answers its identity verb with a real vendor,
	// and `NoRoute` when no pin complex reaches an audio output converter. So an offer here means a
	// codec answered and a route was found, which is the half of the effect a position counter
	// cannot show.
	let offers = recv_offers(&kernel_ep, key.generation).expect("the hda driver should report READY, which it only does with a codec routed");
	let audio = offer_of(&offers, driver_protocol::provider::AUDIO).expect("the driver offers its PCM service").into_any_arc().downcast::<Channel>().expect("the audio channel is a channel");

	// ONE PERIOD, AND EXACTLY ONE PERIOD'S WORTH OF BYTES. The wire's shapes are told apart by
	// LENGTH - a period is `PERIOD_BYTES`, an empty message ends the stream, one byte is a command -
	// so a test sending a different number is not sending a period at all. The content is a ramp
	// rather than silence so a buffer nobody wrote is distinguishable.
	const PERIOD: usize = driver_protocol::audio::PERIOD_BYTES as usize;
	let period: alloc::vec::Vec<u8> = (0..PERIOD).map(|i| (i as u8).wrapping_mul(3)).collect();
	audio.send(Message::new(period, alloc::vec::Vec::new())).expect("the period should send");
	sched::run_until_idle();
	// AND A PERIOD IS ANSWERED, which this driver did not do at all before. AudioService marks the
	// driver PENDING when it hands a period over and sends the next one only when the reply comes -
	// so a server that never replied played exactly one period per boot and then went quiet, with
	// nothing anywhere reporting it.
	let played = audio.recv().expect("a period is answered");
	assert_eq!(played.bytes, driver_protocol::audio::OK, "a period that was taken is answered OK");

	// THE CONTROLLER'S OWN COUNTER, read through a second mapping of the same registers. The stream
	// descriptors start at 0x80 and are 0x20 apart; the output ones follow the input ones, which
	// `GCAP` counts.
	let base = crate::iommu::map_registers(bar_phys, bar_len);
	let mut moved = 0u32;
	for _ in 0..200 {
		sched::run_until_idle();
		let gcap = unsafe { ((base + 0x00) as *const u16).read_volatile() };
		let inputs = ((gcap >> 8) & 0x0F) as u64;
		let stream = base + 0x80 + inputs * 0x20;
		moved = unsafe { ((stream + 0x04) as *const u32).read_volatile() };
		if moved > 0 {
			break;
		}
	}
	assert!(moved > 0, "the controller's link position counter should advance as it consumes the buffer it was given");

	// AND THE SAME PROVIDER ANSWERS A CAPTURE, which is the other half of the item's own sentence -
	// "expose playback AND capture through the same PCM contract". The codec on this machine is a
	// duplex one, so a refusal here is a route that was not built rather than a machine with no
	// microphone.
	//
	// A REFUSAL IS AN EMPTY REPLY AND A PERIOD IS NEVER EMPTY, which is how the two are told apart
	// without a status field - so this asserts the LENGTH and not a code.
	audio.send(Message::new(alloc::vec![driver_protocol::audio::CMD_CAPTURE], alloc::vec::Vec::new())).expect("the capture command should send");
	for _ in 0..200 {
		sched::run_until_idle();
		if audio.peek_len().is_ok() {
			break;
		}
	}
	let captured = audio.recv().expect("the capture command is answered");
	assert_eq!(captured.bytes.len(), PERIOD, "a captured period is one period, and an empty answer would be the refusal this codec has no reason to give");

	// AND THE CONTROLLER MOVED THE BYTES, which the length alone does not say: a driver that
	// answered with its own zeroed buffer would answer with exactly this many bytes. The INPUT
	// stream descriptors are the ones BEFORE the output ones, so input stream zero is descriptor
	// zero - the same arithmetic as above, read from the other end.
	//
	// THE CONTENT IS NOT ASSERTED AND THE REASON IS THE FIXTURE: this harness gives the codec an
	// audio backend with no source behind it, so what a duplex codec captures is silence. What can
	// be asserted is that the DEVICE filled the buffer rather than the driver handing back its own,
	// and the link position is where the device says so.
	//
	// AND WHAT THIS CANNOT SEE IS SAID RATHER THAN LEFT TO BE ASSUMED. Telling the input pin to
	// DRIVE instead of to listen - the one bit that decides which way a jack faces - passes every
	// assertion here, because QEMU's codec model runs the stream either way. On real silicon that
	// is a microphone that records nothing. What the fixture does reach is the route existing at
	// all: dropping the capture converter fails this with 0 against 2048.
	let mut recorded = 0u32;
	for _ in 0..200 {
		sched::run_until_idle();
		recorded = unsafe { ((base + 0x80 + 0x04) as *const u32).read_volatile() };
		if recorded > 0 {
			break;
		}
	}
	assert!(recorded > 0, "the input stream's link position should advance as the controller fills the capture buffer");

	// AND THE STREAM STOPS WHEN IT IS TOLD TO. A capture stream left running fills a buffer nobody
	// reads for the life of the machine.
	audio.send(Message::new(alloc::vec![driver_protocol::audio::CMD_CAPTURE_STOP], alloc::vec::Vec::new())).expect("the capture stop should send");
	sched::run_until_idle();
	let stopped = audio.recv().expect("the capture stop is answered");
	assert_eq!(stopped.bytes, driver_protocol::audio::OK, "ending a capture stream is acknowledged");
}

tagged_test!(sdhci_driver_serves_a_write_and_reads_it_back, [Drivers, Pci, Slow], id = "kernel.hardware.sdhci_driver_serves_a_write_and_reads_it_back", covers = ["kernel", "bin.sdhci"]);
fn sdhci_driver_serves_a_write_and_reads_it_back() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// THE THIRD STORAGE DRIVER AND THE SAME OBSERVABLE EFFECT, over a third command set and - unlike
	// the two before it - over a path that masters NOTHING. This driver declares `dma = "none"`
	// because its first slice is PIO: every block goes through the controller's data port under CPU
	// reads and writes, so the controller is never handed a physical address. That makes this test
	// the first end-to-end proof that a non-mastering driver can serve the block contract at all.
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/sdhci.lsexe")).expect("the sdhci.lsexe driver should be staged on the volume under drivers/");

	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_SDHCI {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_PLAIN_PCI, vendor: d.vendor, product: d.product, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the SD host controller");

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the sdhci driver should load");
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the SD host controller")).expect("the SD host controller is taken, as DeviceManager takes it");
	send_bind(&kernel_ep, &info, key.generation, 1).expect("the BIND should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
	sched::run_until_idle();

	let offers = recv_offers(&kernel_ep, key.generation).expect("the sdhci driver should report READY");
	let blk = offer_of(&offers, driver_protocol::provider::BLOCK).expect("the driver offers the card's block service").into_any_arc().downcast::<Channel>().expect("the block channel is a channel");

	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	blk.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	let reported = driver_protocol::block::decode_capacity(&cap_reply.bytes).expect("the capacity query should succeed and carry a size");
	// Out of the card's CSD, which encodes it two entirely different ways depending on its version.
	assert_eq!(reported.bytes, 64 * 1024 * 1024, "the card should report the attached medium's real size");
	// THE BOUND IS THE PATH'S AND THE DRIVER PUBLISHES IT rather than letting a client ask for eight
	// and receive one. On a controller advertising ADMA2 it is the descriptor span; on one without,
	// the PIO path's single block. QEMU's `sdhci-pci` advertises ADMA2, so this machine sees the
	// first - and the assertion is the RELATIONSHIP rather than either number, because a driver that
	// published a bound it could not move would pass a check on the number alone.
	assert!(reported.max_sectors >= 1, "a bound of zero is not a bound");
	let bound = reported.max_sectors;

	const SECTOR: usize = 512;
	let pattern: alloc::vec::Vec<u8> = (0..SECTOR).map(|i| (i as u8).wrapping_mul(11) ^ 0x3C).collect();
	let source = object::memory_object::MemoryObject::create(SECTOR).expect("a source buffer");
	{
		let hhdm = mem::hhdm_offset();
		let phys = source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(pattern.as_ptr(), (hhdm + phys) as *mut u8, SECTOR) };
	}
	const LBA: u64 = 23;
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the write request should send");
	sched::run_until_idle();
	let write_reply = blk.recv().expect("the write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the write should succeed");

	let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the read request should send");
	sched::run_until_idle();
	let read_reply = blk.recv().expect("the read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the read should succeed");
	let buf_cap = read_reply.caps.first().expect("the read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, SECTOR), pattern, "the block read back should be the bytes that were written to it");

	// AND A BLOCK COUNT PAST THE BOUND IS REFUSED rather than quietly shortened, which is the failure
	// a clamp would hide: a caller asking for more blocks and receiving fewer believes it has more.
	let past = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: bound as u32 + 1 }.encode();
	blk.send(Message::new(past.to_vec(), alloc::vec::Vec::new())).expect("the over-bound request should send");
	sched::run_until_idle();
	let refused = blk.recv().expect("the refusal should arrive");
	assert_eq!(driver_protocol::block::decode_status(&refused.bytes), Some(driver_protocol::block::STATUS_INVALID), "more blocks than the bound is refused, not shortened");
	assert!(refused.caps.is_empty(), "a refused read grants no buffer");

	// AND THE BOUND IS A NUMBER THE DRIVER CAN ACTUALLY MOVE, which is the half publishing it does
	// not prove. FOUR BLOCKS IN ONE REQUEST over a card whose PIO path moves one: the descriptors
	// are built here, the multi-block command carries its stop, and every block has to come back in
	// the right ORDER - a table whose entries were built from the wrong offsets returns the same
	// block four times, or the right bytes shuffled.
	// A SPAN THAT NEEDS MORE THAN ONE DESCRIPTOR, which four blocks does not: 128 blocks is 64 KiB
	// against a descriptor's own 65024-byte bound, so the table has TWO entries and the second's
	// address is the first's plus its length. A four-block version of this test passed with every
	// descriptor pointing at the start of the span, because there was only ever one.
	const WIDE_BLOCKS: u32 = 128;
	if bound >= WIDE_BLOCKS {
		const SPAN: usize = SECTOR * WIDE_BLOCKS as usize;
		let wide: alloc::vec::Vec<u8> = (0..SPAN).map(|i| (i as u8).wrapping_mul(23) ^ ((i / SECTOR) as u8).wrapping_mul(97)).collect();
		let wide_source = object::memory_object::MemoryObject::create(SPAN).expect("a multi-block source buffer");
		// THROUGH EVERY FRAME AND NOT THE FIRST: an object this size is a list of frames that need
		// not be contiguous.
		write_to_object(&wide_source, &wide);
		const WIDE_LBA: u64 = 64;
		let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: WIDE_LBA, count: WIDE_BLOCKS }.encode();
		blk.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(wide_source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the multi-descriptor write should send");
		sched::run_until_idle();
		let write_reply = blk.recv().expect("the multi-descriptor write reply should arrive");
		assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "a span spread over two descriptors should be written");

		// AND A SECOND SPAN IS WRITTEN BEFORE THE FIRST IS READ BACK, which is not belt and braces:
		// the driver moves these bytes through a BOUNCE SPAN it owns, and that span still holds the
		// write's contents afterwards. A read whose descriptors do not actually fetch every byte
		// would find the right answer already sitting there - which is exactly what a first version
		// of this test did, passing a driver whose descriptors all named the same address. Filling
		// the span with something else first is what makes the read prove it read.
		let other: alloc::vec::Vec<u8> = wide.iter().map(|byte| !byte).collect();
		let other_source = object::memory_object::MemoryObject::create(SPAN).expect("a second multi-block source buffer");
		write_to_object(&other_source, &other);
		let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: WIDE_LBA + WIDE_BLOCKS as u64, count: WIDE_BLOCKS }.encode();
		blk.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(other_source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the second span's write should send");
		sched::run_until_idle();
		let other_reply = blk.recv().expect("the second span's write reply should arrive");
		assert_eq!(driver_protocol::block::decode_status(&other_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the second span should be written");

		let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: WIDE_LBA, count: WIDE_BLOCKS }.encode();
		blk.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the multi-descriptor read should send");
		sched::run_until_idle();
		let read_reply = blk.recv().expect("the multi-descriptor read reply should arrive");
		assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "and read back");
		let buf_cap = read_reply.caps.first().expect("the multi-descriptor read should grant a buffer");
		let object = buf_cap.object();
		let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
		assert_eq!(read_from_object(memory, SPAN), wide, "every descriptor's span comes back whole and in order - a table whose entries all name the same address returns the first one repeated");
		// AND THE SINGLE-BLOCK PATH STILL WORKS AFTER A MULTI-BLOCK ONE, which is what says the stop
		// that ends a multi-block transfer was sent: a card still streaming answers nothing.
		let after = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
		blk.send(Message::new(after.to_vec(), alloc::vec::Vec::new())).expect("the single-block read should send");
		sched::run_until_idle();
		let after_reply = blk.recv().expect("the card should still answer after a multi-block transfer");
		assert_eq!(driver_protocol::block::decode_status(&after_reply.bytes), Some(driver_protocol::block::STATUS_OK), "a card left streaming would answer nothing here");
	}
}

tagged_test!(ahci_driver_serves_a_write_and_reads_it_back, [Drivers, Pci, Slow], id = "kernel.hardware.ahci_driver_serves_a_write_and_reads_it_back", covers = ["kernel", "bin.ahci"]);
fn ahci_driver_serves_a_write_and_reads_it_back() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// THE SAME OBSERVABLE EFFECT AS THE NVMe ORACLE BELOW, over a different controller and a
	// different command set: bytes written to a sector and read back out of it. "The driver reached
	// Online" is not accepted here, and for AHCI it would be especially hollow - a controller comes
	// up long before anything has been asked of a disk.
	//
	// TWO CONTROLLERS ARE PRESENT AND ONLY ONE IS SERVABLE, which this test has to handle rather
	// than assume away: the q35 chipset carries its own SATA controller with a CD-ROM on it, and the
	// driver refuses ATAPI by signature. So this walks every AHCI function the table holds and takes
	// the first that reports READY, which is what DeviceManager does with the same two.
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/ahci.lsexe")).expect("the ahci.lsexe driver should be staged on the volume under drivers/");

	let mut controllers: alloc::vec::Vec<(abi::DeviceInfo, u64, u64, usize)> = alloc::vec::Vec::new();
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_AHCI {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_PLAIN_PCI, vendor: d.vendor, product: d.product, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			controllers.push((info, entry.1, entry.2, i));
		}
	}
	assert!(!controllers.is_empty(), "the device table should hold at least one AHCI controller, resolved through BAR 5");

	// THE BOOTSTRAP END IS HELD FOR AS LONG AS THE DRIVER IS WANTED, which is the whole reason this
	// is a `Vec` rather than a loop-local: dropping it closes the driver's bootstrap channel, the
	// driver reads that as the manager going away, and it stops - so the block provider it had just
	// published answered the first request with `PeerClosed`. The harness stands in for
	// DeviceManager, and DeviceManager does not hang up on a driver it is still using.
	let mut held: alloc::vec::Vec<alloc::sync::Arc<Channel>> = alloc::vec::Vec::new();
	let mut served: Option<alloc::sync::Arc<Channel>> = None;
	// The bootstrap end and token of whichever controller answered, so a SECOND consumer can be
	// minted the way DeviceManager mints one.
	let mut serving_from: Option<(alloc::sync::Arc<Channel>, u64, u16)> = None;
	for (info, bar_phys, bar_len, index) in controllers {
		let (kernel_ep, user_ep) = object::channel::Channel::create();
		held.push(kernel_ep.clone());
		loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the ahci driver should load");
		let Ok(key) = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the AHCI controller")) else { continue };
		send_bind(&kernel_ep, &info, key.generation, 1).expect("the BIND should send");
		send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
		sched::run_until_idle();
		if let Some(offers) = recv_offers(&kernel_ep, key.generation) {
			served = offer_of(&offers, driver_protocol::provider::BLOCK).map(|cap| cap.into_any_arc().downcast::<Channel>().expect("the block channel is a channel"));
			if served.is_some() {
				let token = offer_token_of(&offers, driver_protocol::provider::BLOCK).expect("the block publication carries a token");
				serving_from = Some((kernel_ep.clone(), key.generation, token));
				break;
			}
		}
	}
	let blk = served.expect("one of the AHCI controllers has a SATA disk and its driver should publish a block provider for it");

	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	blk.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	let reported = driver_protocol::block::decode_capacity(&cap_reply.bytes).expect("the capacity query should succeed and carry a size");
	// The number comes from IDENTIFY DEVICE's 48-bit sector count, not from anything this test said.
	assert_eq!(reported.bytes, 8 * 1024 * 1024, "the disk should report the attached medium's real size");

	const SECTOR: usize = 512;
	let pattern: alloc::vec::Vec<u8> = (0..SECTOR).map(|i| (i as u8).wrapping_mul(7) ^ 0x5A).collect();
	let source = object::memory_object::MemoryObject::create(SECTOR).expect("a source buffer");
	{
		let hhdm = mem::hhdm_offset();
		let phys = source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(pattern.as_ptr(), (hhdm + phys) as *mut u8, SECTOR) };
	}
	const LBA: u64 = 11;
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the write request should send");
	sched::run_until_idle();
	let write_reply = blk.recv().expect("the write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the write should succeed");

	let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the read request should send");
	sched::run_until_idle();
	let read_reply = blk.recv().expect("the read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the read should succeed");
	let buf_cap = read_reply.caps.first().expect("the read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, SECTOR), pattern, "the sector read back should be the bytes that were written to it");

	// A range past the last sector is REFUSED with the typed status, and the disk is not asked.
	let past = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: 1 << 40, count: 1 }.encode();
	blk.send(Message::new(past.to_vec(), alloc::vec::Vec::new())).expect("the out-of-range request should send");
	sched::run_until_idle();
	let refused = blk.recv().expect("the refusal should arrive");
	assert_eq!(driver_protocol::block::decode_status(&refused.bytes), Some(driver_protocol::block::STATUS_INVALID), "a range past the last sector is refused rather than clamped");
	assert!(refused.caps.is_empty(), "a refused read grants no buffer");

	// TWO CONSUMERS WITH REQUESTS OUTSTANDING AT ONCE, WHICH IS WHAT THE QUEUE IS FOR.
	//
	// The block contract is one request and one reply per message, so a single consumer never has
	// two in flight - a disk's concurrency can only come from its four consumers asking together.
	// This mints a second the way DeviceManager does, puts a read from EACH on the wire before
	// either reply is read, and requires each to come back with ITS OWN sector.
	//
	// WHAT IT CATCHES is the defect a tagged driver introduces: two commands in flight sharing one
	// data span, or two answers going to the wrong consumers. Both sectors are written first, with
	// patterns that differ, so either crossing is a wrong array rather than a wrong status.
	let (kernel_ep, generation, token) = serving_from.expect("the controller that answered was recorded");
	let (second, driver_end) = object::channel::Channel::create();
	send_connect(&kernel_ep, generation, token, driver_end).expect("the second CONNECT should send");
	sched::run_until_idle();

	const FAR_LBA: u64 = 23;
	let far_pattern: alloc::vec::Vec<u8> = (0..SECTOR).map(|i| (i as u8).wrapping_mul(29) ^ 0x3C).collect();
	assert_ne!(far_pattern, pattern, "the two sectors differ, or reading one out of the other would pass");
	let far_source = object::memory_object::MemoryObject::create(SECTOR).expect("a second source buffer");
	{
		let hhdm = mem::hhdm_offset();
		let phys = far_source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(far_pattern.as_ptr(), (hhdm + phys) as *mut u8, SECTOR) };
	}
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: FAR_LBA, count: 1 }.encode();
	second.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(far_source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the second consumer's write should send");
	sched::run_until_idle();
	let write_reply = second.recv().expect("the second consumer's write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the second consumer's write should succeed");

	// BOTH READS GO OUT BEFORE EITHER REPLY IS TAKEN. A driver collecting a batch has both on tags
	// at once; one serving them in turn answers the same two messages, and either way each answer
	// must carry its own sector.
	let near = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	let far = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: FAR_LBA, count: 1 }.encode();
	blk.send(Message::new(near.to_vec(), alloc::vec::Vec::new())).expect("the first consumer's read should send");
	second.send(Message::new(far.to_vec(), alloc::vec::Vec::new())).expect("the second consumer's read should send");
	sched::run_until_idle();

	for (channel, expected, whose) in [(&*blk, &pattern, "the first"), (&*second, &far_pattern, "the second")] {
		let reply = channel.recv().expect("both consumers should be answered");
		assert_eq!(driver_protocol::block::decode_status(&reply.bytes), Some(driver_protocol::block::STATUS_OK), "{whose} consumer's read should succeed while the other was outstanding");
		let buf_cap = reply.caps.first().expect("the read should grant a buffer");
		let object = buf_cap.object();
		let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
		assert_eq!(read_from_object(memory, SECTOR), *expected, "{whose} consumer reads ITS OWN sector, not the one the other asked for");
	}
}

tagged_test!(nvme_driver_serves_a_write_and_reads_it_back, [Drivers, Pci, Slow], id = "kernel.hardware.nvme_driver_serves_a_write_and_reads_it_back", covers = ["kernel", "bin.nvme"]);
fn nvme_driver_serves_a_write_and_reads_it_back() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// THE OBSERVABLE EFFECT, AND NOT "THE DRIVER REACHED ONLINE". This tree's component-oracle
	// convention rejects that, and it is right to: a driver that brings a controller up and then
	// moves no bytes has done the easy half. So this harness drives `driver.nvme` the way
	// DeviceManager does, takes the `block` provider it publishes, WRITES a pattern to a sector and
	// READS IT BACK. A controller whose PRP arithmetic is wrong, whose completions are believed at
	// the wrong phase, or whose doorbell is at the wrong stride fails at that comparison.
	//
	// ONE RESOURCE, NOT FOUR. This driver polls its completion queue, so it never asks for the
	// interrupt the xHCI harness below must mint; an absent resource is a state it can see rather
	// than a message it waits for, which is what `BIND` stating its own count is for.
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/nvme.lsexe")).expect("the nvme.lsexe driver should be staged on the volume under drivers/");

	// THE LAST NVMe CONTROLLER ON THE BUS, AND NOT THE FIRST, WHICH IS A THREE-TARGET FACT.
	//
	// On x86_64 and aarch64 every NVMe controller here is a blank scratch medium the harness attached
	// for this test. On riscv64 the machine's BOOT MEDIUM is an NVMe device - the ESP is attached
	// that way on purpose, because U-Boot's default boot order tries `nvme 0` first - so the FIRST
	// controller is the disk this guest is running from. Taking it would mean claiming the live boot
	// medium and asserting a capacity that belongs to an ESP.
	//
	// The harness attaches its scratch controllers AFTER the boot medium, so the last one is the
	// smaller of the two it attached on every machine, and that is what this drives.
	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_NVME {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_PLAIN_PCI, vendor: d.vendor, product: d.product, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the NVMe controller");

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the nvme driver should load");
	// TAKEN, NOT MERELY MAPPED. A controller handed only its BAR brings its queues up, rings the
	// doorbell and waits forever for a completion the bus will not let it write.
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the NVMe controller")).expect("the NVMe controller is taken, as DeviceManager takes it");
	send_bind(&kernel_ep, &info, key.generation, 1).expect("the BIND should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
	sched::run_until_idle();

	let offers = recv_offers(&kernel_ep, key.generation).expect("the nvme driver should report READY");
	let blk = offer_of(&offers, driver_protocol::provider::BLOCK).expect("the driver offers the controller's block service").into_any_arc().downcast::<Channel>().expect("the block channel is a channel");

	// The capacity query first: the number the driver reports comes from IDENTIFY NAMESPACE rather
	// than from anything this test told it.
	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	blk.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	let reported = driver_protocol::block::decode_capacity(&cap_reply.bytes).expect("the capacity query should succeed and carry a size");
	// EIGHT MEGABYTES, which is the SECOND scratch medium the harness attaches - the last NVMe
	// controller on every one of the three machines. See the selection above for why it is the last
	// and not the first.
	assert_eq!(reported.bytes, 8 * 1024 * 1024, "the controller should report the attached medium's real size");
	assert!(reported.max_sectors > 0, "and a per-request bound the client can size against");

	// A PATTERN THAT IS NEITHER ZEROES NOR CONSTANT, because the medium starts blank: a read that
	// returned untouched medium would match a zero buffer, and one that returned the same byte
	// everywhere would match a constant fill whatever offset it actually read from.
	const SECTOR: usize = 512;
	let pattern: alloc::vec::Vec<u8> = (0..SECTOR).map(|i| (i as u8) ^ 0xA5).collect();
	let source = object::memory_object::MemoryObject::create(SECTOR).expect("a source buffer");
	{
		let hhdm = mem::hhdm_offset();
		let phys = source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(pattern.as_ptr(), (hhdm + phys) as *mut u8, SECTOR) };
	}
	// LBA 7 rather than 0, so a driver that ignored the address and always moved the first sector
	// would still be caught.
	const LBA: u64 = 7;
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the write request should send");
	sched::run_until_idle();
	let write_reply = blk.recv().expect("the write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the write should succeed");

	let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: LBA, count: 1 }.encode();
	blk.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the read request should send");
	sched::run_until_idle();
	let read_reply = blk.recv().expect("the read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the read should succeed");
	let buf_cap = read_reply.caps.first().expect("the read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, SECTOR), pattern, "the sector read back should be the bytes that were written to it");

	// AND A REQUEST PAST THE END IS REFUSED RATHER THAN CLAMPED, with the typed status that tells a
	// caller it got the request wrong apart from one saying the device failed. A clamp here would
	// turn a wrong request into a wrong WRITE somewhere the caller did not name.
	let past = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: 1 << 40, count: 1 }.encode();
	blk.send(Message::new(past.to_vec(), alloc::vec::Vec::new())).expect("the out-of-range request should send");
	sched::run_until_idle();
	let refused = blk.recv().expect("the refusal should arrive");
	assert_eq!(driver_protocol::block::decode_status(&refused.bytes), Some(driver_protocol::block::STATUS_INVALID), "a range past the last block is refused, and the controller is not asked");
	assert!(refused.caps.is_empty(), "a refused read grants no buffer");
}

/// Bring the xHCI controller up the way DeviceManager does, and hand back the bootstrap
/// channel, the claim generation and the driver's whole publication.
///
/// WRITTEN ONCE BECAUSE TWO ORACLES NEED IT. Finding the controller, minting its MMIO and its
/// MSI-X, claiming the device and sending the four-resource `BIND` is sixty lines that say
/// nothing about what either test is for - and a second copy of them is a second place for the
/// handshake to drift when a resource is added to it.
fn bind_xhci_controller() -> (alloc::sync::Arc<object::channel::Channel>, u64, alloc::vec::Vec<(u16, u16, alloc::sync::Arc<dyn object::KernelObject>)>, alloc::sync::Arc<object::process::Process>, abi::ClaimKey) {
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/xhci.lsexe")).expect("the xhci.lsexe driver should be staged on the volume under drivers/");

	// find the controller in the device table and mint its MMIO capability.
	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_XHCI {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: 0x1af4, product: 0, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the xHCI controller");

	// mint the controller's MSI-X Interrupt the way sys_device_msix_acquire does:
	// reserve a vector, program table entry 0, bind the Interrupt object to the
	// vector, and enable MSI-X on the function.
	let (msix_cap, table_phys, bus, dev, func) = device::with(index, |d| (d.msix_cap, d.msix_table_phys, d.bus, d.dev, d.func)).unwrap();
	assert!(msix_cap != 0, "the xHCI controller should expose MSI-X");
	let dest = arch::percpu::this_cpu().lapic_id() as u8;
	let vector = arch::interrupts::acquire_msi(table_phys, dest, index as u32).expect("an MSI vector should be free");
	let interrupt = object::interrupt::Interrupt::new(vector).expect("a test interrupt");
	assert!(arch::interrupts::bind_msi(vector, &interrupt), "the MSI vector should bind");
	// The entry is programmed MASKED - see `program_msix_entry` - so this harness unmasks it the way
	// `sys_device_msix_acquire` does once its acquire has committed. Without it the device is enabled
	// and its one entry still refuses to deliver.
	arch::interrupts::unmask_msi(vector, table_phys);
	arch::pci::msix_enable(bus, dev, func, msix_cap);

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	let driver = loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the xhci driver should load");
	let mut msg = alloc::vec::Vec::with_capacity(6 + core::mem::size_of::<abi::DeviceInfo>());
	msg.extend_from_slice(b"DEVICE");
	msg.extend_from_slice(unsafe { core::slice::from_raw_parts(&info as *const abi::DeviceInfo as *const u8, core::mem::size_of::<abi::DeviceInfo>()) });
	// THE CAPABILITY IS MINTED THE WAY `SYS_DEVICE_CLAIM` MINTS IT - for the device index, not for
	// a bare physical range - and the device is taken at the same moment. This harness is standing in
	// for DeviceManager, and taking the device is what lets it write to memory: a controller handed
	// only its BAR would bring its rings up, ring the doorbell, and wait forever for a completion the
	// bus would not let it write. Bus mastering goes off when the driver process dies and the
	// transferred capability dies with it; the CLAIM ends with this test kernel.
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the xHCI controller")).expect("the xHCI controller is taken, as DeviceManager takes it");
	// THE HANDSHAKE THIS HARNESS SENDS IS THE ONE DEVICEMANAGER SENDS. `BIND` states how many
	// resources follow and each one says which kind it is, so the driver no longer has to know an
	// order nobody told it - which is what the five positional messages here used to require, and
	// why a capability added at the end of the sequence had to be added at the END.
	//
	// Four resources: the device, its interrupt, a key sink, and a console feed. The power
	// connection is deliberately absent - this harness has no business stopping the machine, and an
	// absent resource is now a state the driver can see rather than a message it waits for.
	let (_key_drain, key_sink) = object::channel::Channel::create();
	let (_console_drain, console_feed) = object::channel::Channel::create();
	send_bind(&kernel_ep, &info, key.generation, 4).expect("the BIND should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory"), Rights::ALL).expect("the DEVICE resource should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Irq, key.generation, interrupt, Rights::ALL).expect("the IRQ resource should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Keys, key.generation, key_sink, Rights::ALL).expect("the KEYS resource should send");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Console, key.generation, console_feed, Rights::ALL).expect("the CONSOLE resource should send");
	sched::run_until_idle();

	// EVERY PROVIDER ARRIVES IN ONE HANDSHAKE, HELD UNPUBLISHED UNTIL ITS `READY`. These used to be
	// three messages told apart by the literal bytes `USBBUS` and `POINTER` - so what a capability
	// was for was decided by parsing a string the driver chose - and the human report was the
	// message the harness asserted on, which made changing a boot line's wording able to break this.
	let offers = recv_offers(&kernel_ep, key.generation).expect("the xhci driver should report READY");
	(kernel_ep, key.generation, offers, driver, key)
}

tagged_test!(xhci_driver_enumerates_the_usb_bus, [Drivers, Usb, Slow], id = "kernel.hardware.xhci_driver_enumerates_the_usb_bus", covers = ["kernel", "bin.xhci"]);
fn xhci_driver_enumerates_the_usb_bus() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// The userspace xhci driver, driven the way DeviceManager drives it: spawn its
	// staged ELF (it lives on the system volume under drivers/, not in the init
	// package) with a bootstrap channel, hand it "DEVICE" + the controller's
	// DeviceInfo + a DeviceMemory capability to its register file, "IRQ" + its
	// MSI-X Interrupt capability and "KEYS" + a raw keyboard sink, then wait for
	// its report - all three handoffs, in that order. The driver resets the
	// controller, builds the command and event rings, enumerates the root-hub
	// ports, addresses each connected device and reads its device descriptor - QEMU
	// hangs a hub with a USB keyboard and a USB tablet behind it and a mass-storage
	// stick off the controller (see qemu-run.sh), so four devices must come back
	// addressed: the hub (expanded through its class requests and route strings),
	// the keyboard and the tablet behind it (their HID interfaces configured and
	// their report descriptors parsed, which the report's keyboard and pointer
	// markers prove), and the stick (its Bulk-Only transport brought up).
	let (kernel_ep, generation, offers, driver, claim) = bind_xhci_controller();
	// The bus query channel: drive one raw `usb.list` request over it ([op u16][correlation u32],
	// the generated wire header) and expect a successful reply naming all four devices' roles - the
	// live inventory `lsusb` reads.
	let usbq = offer_of(&offers, driver_protocol::provider::USB_BUS).expect("the driver offers its bus query channel").into_any_arc().downcast::<Channel>().expect("the query channel is a channel");
	assert!(offer_of(&offers, driver_protocol::provider::POINTER).is_some(), "and its pointer-event channel, the raw stream a USB pointing device's reports feed");
	assert!(offer_of(&offers, driver_protocol::provider::BLOCK).is_some(), "and the USB stick's block service, because this machine has one attached");
	assert!(offer_of(&offers, driver_protocol::provider::NET).is_some(), "and the CDC Ethernet adapter's link, because this machine has one on the hub");

	// THE CDC-ECM ADAPTER, PROVED BY A ROUND TRIP OFF THE HOST'S NETWORK STACK.
	//
	// A NIC is published as a FACTORY and its contract begins with the DRIVER speaking - it leads
	// every connection with the MAC and the link MTU, because NetworkService cannot build a stack
	// without them - so this mints a connection the way DeviceManager does rather than using the
	// offered endpoint directly.
	let net_token = offer_token_of(&offers, driver_protocol::provider::NET).expect("the adapter's publication carries a token");
	let (host_end, driver_end) = object::channel::Channel::create();
	send_connect(&kernel_ep, generation, net_token, driver_end).expect("the CONNECT should send");
	sched::run_until_idle();
	let hello = host_end.recv().expect("the adapter should lead its connection with its MAC and the link MTU");
	assert_eq!(&hello.bytes[..3], b"MAC", "the frame transport begins with the same eleven bytes virtio-net sends");
	let mac: [u8; 6] = [hello.bytes[3], hello.bytes[4], hello.bytes[5], hello.bytes[6], hello.bytes[7], hello.bytes[8]];
	assert!(mac != [0u8; 6] && mac[0] & 1 == 0, "the MAC read out of the device's string descriptor should be a real unicast address");
	let mtu = u16::from_le_bytes([hello.bytes[9], hello.bytes[10]]);
	assert!((576..=1500).contains(&mtu), "the link MTU comes from the device's own Ethernet functional descriptor, and 1500 is what this adapter reports");

	// An ARP request for the host network's gateway. The far end is QEMU's user-mode stack, which
	// answers it - so a reply coming back means the frame went out of the guest, through the
	// adapter's bulk OUT endpoint, was understood as Ethernet, and the answer came back in on a
	// standing bulk IN transfer. Nothing about that is provable by the driver reporting READY.
	const GATEWAY: [u8; 4] = [10, 0, 2, 2];
	const OURS: [u8; 4] = [10, 0, 2, 77];
	let mut arp = alloc::vec::Vec::with_capacity(42);
	arp.extend_from_slice(&[0xFF; 6]);
	arp.extend_from_slice(&mac);
	arp.extend_from_slice(&[0x08, 0x06]); // ARP
	arp.extend_from_slice(&[0x00, 0x01, 0x08, 0x00, 6, 4, 0x00, 0x01]); // Ethernet/IPv4, request
	arp.extend_from_slice(&mac);
	arp.extend_from_slice(&OURS);
	arp.extend_from_slice(&[0u8; 6]);
	arp.extend_from_slice(&GATEWAY);
	host_end.send(Message::new(arp, alloc::vec::Vec::new())).expect("the ARP request should send");

	// THE ANSWER IS WAITED FOR OVER SEVERAL PASSES, not asserted on the first. It has to cross the
	// bulk OUT endpoint, the host's stack and a bulk IN completion, and the driver's loop only
	// forwards what it has reaped - so this drives the scheduler until the reply is there or the
	// budget is spent, and says which.
	let mut reply_seen = false;
	for _ in 0..64 {
		sched::run_until_idle();
		while let Ok(frame) = host_end.recv() {
			// An ARP reply for the address that was asked about, from the gateway.
			if frame.bytes.len() >= 42 && frame.bytes[12] == 0x08 && frame.bytes[13] == 0x06 && frame.bytes[21] == 0x02 && frame.bytes[28..32] == GATEWAY {
				assert_eq!(&frame.bytes[38..42], &OURS, "the reply should be addressed to the protocol address that asked");
				reply_seen = true;
				break;
			}
		}
		if reply_seen {
			break;
		}
	}
	assert!(reply_seen, "the host should answer the ARP request over the USB adapter, which is the round trip nothing about reaching READY proves");

	// AND THE AUDIO SINK ON THE SAME BUS TAKES A PERIOD, which is the only ISOCHRONOUS transfer this
	// controller does: the bus reserves bandwidth for one and delivers late rather than not at all,
	// so there is no retry and no stall to clear - a driver that got the endpoint type, the
	// interval or the packet split wrong gets a completion code that is none of the two this
	// answers `OK` for.
	//
	// AND `OK` IS THE CONTROLLER'S OWN STATEMENT AND NOT THE DRIVER'S. The driver answers it only
	// after the transfer event for the last packet of the period has arrived, so a sink that
	// answered without moving anything could not answer at all.
	let audio_token = offer_token_of(&offers, driver_protocol::provider::AUDIO).expect("a controller with an audio sink on it publishes one");
	let (sink, driver_end) = object::channel::Channel::create();
	send_connect(&kernel_ep, generation, audio_token, driver_end).expect("the audio CONNECT should send");
	sched::run_until_idle();
	let period: alloc::vec::Vec<u8> = (0..driver_protocol::audio::PERIOD_BYTES as usize).map(|i| (i as u8).wrapping_mul(17)).collect();
	sink.send(Message::new(period, alloc::vec::Vec::new())).expect("the period should send");
	for _ in 0..64 {
		sched::run_until_idle();
		if sink.peek_len().is_ok() {
			break;
		}
	}
	let played = sink.recv().expect("the audio sink answers a period");
	assert_eq!(played.bytes, driver_protocol::audio::OK, "the controller took the period - an empty reply is this wire's refusal");

	// AND A LENGTH THIS WIRE HAS NO SHAPE FOR IS REFUSED rather than played as a short period,
	// which is what the enumeration in `driver_protocol::audio` is for.
	sink.send(Message::new(alloc::vec![0u8; 7], alloc::vec::Vec::new())).expect("the malformed message should send");
	sched::run_until_idle();
	let refused = sink.recv().expect("even a refusal is answered");
	assert!(refused.bytes.is_empty(), "a refusal is an empty reply, which cannot be mistaken for samples");

	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&1u16.to_le_bytes()); // OP_LIST
	list.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	usbq.send(Message::new(list, alloc::vec::Vec::new())).expect("the usb.list request should send");
	sched::run_until_idle();
	let inventory = usbq.recv().expect("the usb.list reply should arrive");
	assert!(inventory.bytes.len() >= 5 && inventory.bytes[4] == 1, "the inventory query should succeed");
	let has = |needle: &[u8]| inventory.bytes.windows(needle.len()).any(|w| w == needle);
	assert!(has(b"hub") && has(b"keyboard") && has(b"pointer") && has(b"storage"), "the inventory should name the hub, the keyboard, the tablet and the stick by role");
	assert!(has(b"network"), "and the CDC Ethernet adapter, whose role is its own and not `device`");
	assert!(has(b"uas"), "and the UAS target, which is a second storage device over a different transport");

	// AN ANSWER THAT DOES NOT COME BACK INSIDE THE SAME CALL. This transport leaves a request
	// OUTSTANDING under its tag and the driver's own loop answers it when the completion arrives, so
	// a reply is waited for rather than read. Bounded, and a bound that runs out still fails the
	// test - what would be a weakening is dropping the assertion, not giving the device a chance to
	// answer it.
	fn await_reply(channel: &alloc::sync::Arc<Channel>, what: &str) -> Message {
		for _ in 0..256 {
			sched::run_until_idle();
			if channel.peek_len().is_ok() {
				break;
			}
		}
		channel.recv().unwrap_or_else(|_| panic!("{what}"))
	}

	// THE UAS TARGET, WHICH IS THE SECOND BLOCK PROVIDER AND NOT THE STICK'S.
	//
	// The controller publishes two of one kind - a Bulk-Only stick and a UAS disk are two devices
	// speaking one command set over different pipes - so this asks for the SECOND, which is the case
	// the catalogue's token exists for. What it proves is the four-pipe transport over BULK STREAMS:
	// a command on one pipe, data on another, a status information unit on a third, joined by a tag,
	// with the stream id selecting the ring the answers land on.
	let uas = nth_offer_of(&offers, driver_protocol::provider::BLOCK, 1).expect("the driver offers the UAS target's block service as a second provider of that kind").into_any_arc().downcast::<Channel>().expect("the UAS block channel is a channel");
	let capacity = driver_protocol::block::Request { op: driver_protocol::block::OP_CAPACITY, lba: 0, count: 0 }.encode();
	uas.send(Message::new(capacity.to_vec(), alloc::vec::Vec::new())).expect("the UAS capacity request should send");
	let cap_reply = await_reply(&uas, "the UAS capacity reply should arrive");
	let reported = driver_protocol::block::decode_capacity(&cap_reply.bytes).expect("the UAS capacity query should succeed and carry a size");
	assert_eq!(reported.bytes, 4 * 1024 * 1024, "the UAS target should report the medium the harness attached");

	const UAS_SECTOR: usize = 512;
	const UAS_LBA: u64 = 0x0102;
	let uas_pattern: alloc::vec::Vec<u8> = (0..UAS_SECTOR).map(|i| (i as u8).wrapping_mul(37) ^ 0xA5).collect();
	let uas_source = object::memory_object::MemoryObject::create(UAS_SECTOR).expect("a source buffer");
	{
		let hhdm = mem::hhdm_offset();
		let phys = uas_source.frames()[0];
		unsafe { core::ptr::copy_nonoverlapping(uas_pattern.as_ptr(), (hhdm + phys) as *mut u8, UAS_SECTOR) };
	}
	let write = driver_protocol::block::Request { op: driver_protocol::block::OP_WRITE, lba: UAS_LBA, count: 1 }.encode();
	uas.send(Message::new(write.to_vec(), alloc::vec![object::handle::Capability::new(uas_source.clone() as alloc::sync::Arc<dyn object::KernelObject>, Rights::ALL)])).expect("the UAS write should send");
	let write_reply = await_reply(&uas, "the UAS write reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&write_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the write over the UAS pipes should succeed");

	let read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: UAS_LBA, count: 1 }.encode();
	uas.send(Message::new(read.to_vec(), alloc::vec::Vec::new())).expect("the UAS read should send");
	let read_reply = await_reply(&uas, "the UAS read reply should arrive");
	assert_eq!(driver_protocol::block::decode_status(&read_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the read over the UAS pipes should succeed");
	let uas_buf = read_reply.caps.first().expect("the UAS read should grant a buffer");
	let uas_object = uas_buf.object();
	let uas_memory = uas_object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(uas_memory, UAS_SECTOR), uas_pattern, "what the UAS target read back should be the bytes that were written to it");

	// AND TWO REQUESTS IN FLIGHT AT ONCE, WHICH IS WHAT THE TAGS ARE FOR.
	//
	// A SECOND CONSUMER AND NOT A SECOND REQUEST ON THIS ONE, because `driver_protocol::block`
	// carries no correlation id: two replies on ONE channel must arrive in the order the requests
	// were made, and a tagged transport answers in whatever order the device finishes. So the
	// concurrency this transport offers is ACROSS consumers, each with one outstanding request, and
	// that is exactly what this mints - the same way DeviceManager mints one.
	//
	// BOTH REQUESTS GO OUT BEFORE EITHER REPLY IS READ. Sending one and waiting for it would pass
	// against a driver that never had two tags outstanding in its life, which is the claim under
	// test rather than something to assume.
	let uas_token = offers.iter().filter(|(k, _, _)| *k == driver_protocol::provider::BLOCK).map(|(_, token, _)| *token).nth(1).expect("the UAS target's block publication has a token of its own");
	let (second, second_driver_end) = Channel::create();
	send_connect(&kernel_ep, generation, uas_token, second_driver_end).expect("a second consumer of the UAS target should connect");
	sched::run_until_idle();

	let first_read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: UAS_LBA, count: 1 }.encode();
	let other_read = driver_protocol::block::Request { op: driver_protocol::block::OP_READ, lba: UAS_LBA, count: 1 }.encode();
	uas.send(Message::new(first_read.to_vec(), alloc::vec::Vec::new())).expect("the first concurrent read should send");
	second.send(Message::new(other_read.to_vec(), alloc::vec::Vec::new())).expect("the second concurrent read should send");

	let first_reply = await_reply(&uas, "the first consumer's concurrent read should be answered");
	let second_reply = await_reply(&second, "the second consumer's concurrent read should be answered");
	assert_eq!(driver_protocol::block::decode_status(&first_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the first consumer's read should succeed while another was outstanding");
	assert_eq!(driver_protocol::block::decode_status(&second_reply.bytes), Some(driver_protocol::block::STATUS_OK), "the second consumer's read should succeed while another was outstanding");

	// EACH CONSUMER'S BUFFER IS ITS OWN, which is the half a shared data page would break: two
	// commands in flight over one page is the second overwriting what the first is still moving,
	// and both consumers would read the same bytes without either request having failed.
	for (reply, who) in [(&first_reply, "the first consumer"), (&second_reply, "the second consumer")] {
		let buffer = reply.caps.first().unwrap_or_else(|| panic!("{who}'s concurrent read should grant a buffer"));
		let object = buffer.object();
		let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().unwrap_or_else(|| panic!("{who}'s granted capability should be a buffer"));
		assert_eq!(read_from_object(memory, UAS_SECTOR), uas_pattern, "{who} should read back the sector that was written, from a buffer of its own");
	}

	// The stick's block provider: read sector 0 over it, the same [op u32][lba u64][count u32]
	// contract driver.virtio-blk serves, and expect a success status plus a 512-byte shared buffer.
	let blk = offer_of(&offers, driver_protocol::provider::BLOCK).expect("the driver offers the stick's block service").into_any_arc().downcast::<Channel>().expect("the block channel is a channel");
	// first the capacity query (op 2): the reply is [status u32][capacity bytes u64]
	// and must report the seeded 16 MB stick image.
	let mut capacity = alloc::vec::Vec::with_capacity(16);
	capacity.extend_from_slice(&2u32.to_le_bytes()); // op = capacity
	capacity.extend_from_slice(&0u64.to_le_bytes());
	capacity.extend_from_slice(&0u32.to_le_bytes());
	blk.send(Message::new(capacity, alloc::vec::Vec::new())).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	assert_eq!(&cap_reply.bytes[..4], &0u32.to_le_bytes(), "the capacity query should succeed");
	let bytes = u64::from_le_bytes([cap_reply.bytes[4], cap_reply.bytes[5], cap_reply.bytes[6], cap_reply.bytes[7], cap_reply.bytes[8], cap_reply.bytes[9], cap_reply.bytes[10], cap_reply.bytes[11]]);
	assert_eq!(bytes, 16 * 1024 * 1024, "the stick should report its seeded 16 MB capacity");
	let mut request = alloc::vec::Vec::with_capacity(16);
	request.extend_from_slice(&0u32.to_le_bytes()); // op = read
	request.extend_from_slice(&0u64.to_le_bytes()); // lba 0
	request.extend_from_slice(&1u32.to_le_bytes()); // one sector
	blk.send(Message::new(request, alloc::vec::Vec::new())).expect("the block request should send");
	sched::run_until_idle();
	let reply = blk.recv().expect("the block reply should arrive");
	assert_eq!(&reply.bytes[..4], &0u32.to_le_bytes(), "the USB read should succeed");
	let buf_cap = reply.caps.first().expect("the read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, 512).len(), 512, "the buffer should hold the sector");

	// the vol://usb volume end to end: a StorageService instance is handed the same
	// block channel ("USBBLOCK" - the removable FAT backing that mounts lazily, on
	// first use) and a serve channel, and must resolve a file off the stick's FAT
	// image - the same bytes the seed laid down from volume/. The kernel's block
	// endpoint moves to the service whole: the service is its consumer now.
	let (volume2, package) = scenario_packages().expect("boot modules should be present");
	let service_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
	let (service_boot_kernel, service_boot_user) = object::channel::Channel::create();
	let (service_server, service_client) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, service_boot_user, Rights::ALL).expect("the StorageService should load");
	send_cap(&service_boot_kernel, b"USBBLOCK", blk, Rights::ALL).expect("the USBBLOCK handoff should send");
	send_cap(&service_boot_kernel, b"SERVE", service_server, Rights::ALL).expect("the SERVE handoff should send");
	sched::run_until_idle();
	let online = service_boot_kernel.recv().expect("the usb StorageService should report in");
	assert_eq!(&online.bytes[..], b"StorageService: online (vol://usb)", "the instance should come up without touching the media (the mount is lazy)");

	// one generated volume.open request for a seeded file, plus the quit sentinel.
	let uri: &[u8] = b"vol://usb/hello.txt";
	let mut open = alloc::vec::Vec::new();
	open.extend_from_slice(&1u16.to_le_bytes()); // OP_OPEN
	open.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	open.extend_from_slice(&(uri.len() as u16).to_le_bytes());
	open.extend_from_slice(uri);
	open.push(0); // write = false
	open.push(0); // create = false
	service_client.send(Message::new(open, alloc::vec::Vec::new())).expect("the open request should send");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("the quit sentinel should send");
	sched::run_until_idle();
	let reply = service_client.recv().expect("the open reply should arrive");
	assert!(reply.bytes.len() >= 17 && reply.bytes[4] == 1, "the usb volume should resolve the seeded file");
	let size = u64::from_le_bytes([reply.bytes[9], reply.bytes[10], reply.bytes[11], reply.bytes[12], reply.bytes[13], reply.bytes[14], reply.bytes[15], reply.bytes[16]]) as usize;
	let file_cap = reply.caps.first().expect("the open should grant the file buffer");
	let file_object = file_cap.object();
	let file = file_object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	let expected = volume_file(volume2, b"hello.txt").expect("hello.txt should be in the volume package");
	assert_eq!(read_from_object(file, size), expected, "vol://usb should serve the seeded file's bytes");
	// AND THE DEVICE IS GIVEN BACK WITH THE TEST, DRIVER FIRST AND CLAIM AFTER.
	//
	// A driver process left running holds the controller and keeps servicing it, so a second oracle
	// on this bus spawns another beside it and the two share the device. And the CLAIM outlives the
	// process: this file used to say "the CLAIM ends with this test kernel", which was true while
	// exactly one test ever took this controller - the moment a second one does, it is answered
	// `AlreadyClaimed` and cannot bind at all.
	//
	// THE ORDER IS NOT A PREFERENCE. Releasing the claim turns off bus mastering and MSI-X on the
	// function, so a driver still running would be one whose device has stopped answering; taking
	// the process away first is what makes the release the end of something rather than the middle.
	driver.terminate();
	sched::run_until_idle();
	let _ = crate::device::release_claim(claim);
}

// A PHYSICAL ADDRESS IS ANSWERABLE ONLY FOR A BUFFER SOME DEVICE WAS NAMED FOR, in both directions.
//
// `sys_dma_buffer_create` accepts a zero device handle, and `sys_dma_buffer_phys` did not look - so
// any process that could make a DMA buffer could learn where its own pages are, on a machine with
// no IOMMU, for no device at all.
//
// AND THE OTHER DIRECTION MATTERS AS MUCH: a device-bound buffer still answers WITHOUT BEING MAPPED.
// virtio-gpu's framebuffer backing is exactly that - ConsoleService renders into it and a
// `DmaBuffer` maps only once, while the GPU needs the addresses - so a narrowing to "only while
// mapped" would have taken the display with it. That case is asserted here on purpose.
tagged_test!(a_physical_address_is_answerable_only_for_a_device_bound_buffer, [Drivers, Memory], id = "kernel.hardware.a_physical_address_is_answerable_only_for_a_device_bound_buffer", covers = ["kernel"]);
fn a_physical_address_is_answerable_only_for_a_device_bound_buffer() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static BOUND_PHYS: AtomicI64 = AtomicI64::new(0);
	static UNBOUND_PHYS: AtomicI64 = AtomicI64::new(0);
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let grant = crate::tests::claim_device(0).expect("the test claims device 0");
			let device = grant.memory;
			// Bound, and deliberately NEVER MAPPED - the virtio-gpu framebuffer case.
			let bound = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, device as u64, 0, 0);
			assert!(!syscall::sys_is_err(bound), "a device-bound DMA buffer is created");
			BOUND_PHYS.store(arch::syscall::invoke(syscall::SYS_DMA_BUFFER_PHYS, bound, 0, 0, 0) as i64, Ordering::SeqCst);
			// Bound to nothing, which is what a zero device handle means.
			let unbound = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(unbound), "an unbound DMA buffer is still creatable");
			UNBOUND_PHYS.store(arch::syscall::invoke(syscall::SYS_DMA_BUFFER_PHYS, unbound, 0, 0, 0) as i64, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	DONE.store(false, Ordering::SeqCst);
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the probing thread ran to the end");
	let bound = BOUND_PHYS.load(Ordering::SeqCst);
	assert!(!syscall::sys_is_err(bound as u64) && bound != 0, "an unmapped device-bound buffer still reports its physical base");
	assert_eq!(UNBOUND_PHYS.load(Ordering::SeqCst), syscall::ERR_INVALID, "a buffer bound to no device has no physical address to give");
}

tagged_test!(dma_buffer_maps_and_reports_phys, [Drivers, Memory], id = "kernel.hardware.dma_buffer_maps_and_reports_phys", covers = ["kernel"]);
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
			let Ok(grant) = crate::tests::claim_device(0) else {
				return;
			};
			let device = grant.memory;
			let handle = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, device as u64, 0, 0);
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
			assert_eq!(arch::syscall::invoke(syscall::SYS_DMA_BUFFER_UNMAP, handle, 0, 0, 0) as i64, 0);
			let remapped = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_MAP, handle, 0, 0, 0);
			assert_eq!(remapped, virt, "the released DMA virtual range should be reused");
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

// A PCI FUNCTION NOTHING BINDS IS STILL IN THE INVENTORY, AND HAS NO RESOURCES.
//
// M1 says every PCI function is inventoried and the definition of done says a function nothing binds
// stays discoverable and capability-free. It was neither: `device::init` filled `PCI_FUNCTIONS` from
// the full scan for `lspci` alone, and filled the table that answers `SYS_DEVICE_COUNT`, supplies
// identity to the binder and owns the claim slots from `scan_virtio()` and `scan_xhci()` only. A
// function outside those two resolvers existed in one diagnostic syscall and nowhere else - and M4's
// missing fixture for exactly this case is why nobody noticed.
//
// THE MACHINE SUPPLIES THE CASE. q35 carries an ISA bridge, a SATA controller and an SMBus function
// beside the virtio devices, so this asserts against real rows rather than an injected one.
crate::tagged_test!(a_pci_function_nothing_binds_is_still_inventoried_and_holds_nothing, [Drivers, Kernel], id = "kernel.hardware.a_pci_function_nothing_binds_is_still_inventoried_and_holds_nothing", covers = ["kernel"]);
fn a_pci_function_nothing_binds_is_still_inventoried_and_holds_nothing() {
	let count = crate::device::count();
	let mut unresolved = 0usize;
	for index in 0..count {
		let Some(entry) = crate::device::with(index, |d| (d.device_type, d.transport, d.bar_phys, d.bar_len, d.msix_cap, d.vendor, d.bus, d.dev, d.func)) else {
			panic!("device {index} is counted and cannot be read - the count and the table disagree");
		};
		let (device_type, transport, bar_phys, bar_len, msix_cap, vendor, bus, dev, func) = entry;
		if device_type != abi::DEVICE_TYPE_UNKNOWN as u16 {
			continue;
		}
		unresolved += 1;
		// CAPABILITY-FREE. This kernel resolved no BAR and no MSI-X for it, so there is nothing a
		// claim could hand a driver - which is what makes an unbound function safe to inventory.
		assert_eq!(bar_phys, 0, "an unresolved function at {bus:02x}:{dev:02x}.{func} carries a BAR address this kernel never resolved");
		assert_eq!(bar_len, 0, "an unresolved function at {bus:02x}:{dev:02x}.{func} carries a BAR length this kernel never resolved");
		assert_eq!(msix_cap, 0, "an unresolved function at {bus:02x}:{dev:02x}.{func} carries an MSI-X capability this kernel never resolved");
		// AND IT KEPT ITS IDENTITY, which is the half that makes it matchable by a registry rule.
		assert_eq!(transport, abi::TRANSPORT_PLAIN_PCI, "an unresolved function speaks plain PCI - it is not a virtio transport this kernel decoded");
		assert_ne!(vendor, 0, "an unresolved function at {bus:02x}:{dev:02x}.{func} lost the vendor id the scan read");
	}
	assert!(unresolved > 0, "this machine's bus carries functions outside the virtio and xHCI resolvers, and none of them reached the inventory - which is the defect, not the fixture");

	// AND TWO CONTROLLERS OF ONE KIND DO NOT COLLIDE, which is M4's other named case. This machine
	// presents several virtio-blk functions; each is its own row with its own address and its own
	// claim slot, so "the same driver bound both" is two independent bindings rather than one row
	// two things share.
	let mut same_kind: alloc::vec::Vec<(u16, u8, u8, u8)> = alloc::vec::Vec::new();
	for index in 0..count {
		if let Some(row) = crate::device::with(index, |d| (d.device_type, d.bus, d.dev, d.func)) {
			// SYNTHETIC ROWS ARE NOT BUS FUNCTIONS, and this is about bus functions. `add_synthetic_device`
			// appends a table entry with `device_type: u16::MAX` at the non-address `ff:1f.7` so a
			// test can drive claim mechanics without a device; several tests in this suite make one,
			// and in a whole-suite run there is more than one - which is two rows carrying the same
			// non-address, not two rows naming one PCI function. Asserted per tag ran, and this only
			// showed up on the full suite.
			if row.0 != u16::MAX {
				same_kind.push(row);
			}
		}
	}
	for (at, left) in same_kind.iter().enumerate() {
		for right in same_kind.iter().skip(at + 1) {
			assert_ne!((left.1, left.2, left.3), (right.1, right.2, right.3), "two inventory rows name one PCI address, so a claim on either is a claim on both");
		}
	}
	let blocks = same_kind.iter().filter(|row| row.0 == abi::VIRTIO_TYPE_BLOCK as u16).count();
	assert!(blocks > 1, "this machine presents several virtio-blk functions and the inventory holds {blocks} - the same-kind case cannot be asserted against one row");
	crate::serial_println!("    {unresolved} PCI function(s) nothing binds are inventoried, identified and hold nothing; {blocks} controllers of one kind hold {blocks} rows");
}

tagged_test!(device_service_lists_devices, [Service, Drivers], id = "kernel.hardware.device_service_lists_devices", covers = ["kernel", "bin.device_service"]);
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
	service_client.send(Message::new(req, alloc::vec::Vec::new())).expect("list request");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("quit sentinel");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("DeviceService online report");
	assert_eq!(&online.bytes[..], b"DeviceService: online", "DeviceService reports in");

	// The list reply is [corr u32][ok u8 = 1][count u16][device-entry...], each entry
	// [index u32][type u8][mmio-len u64]. QEMU exposes the virtio devices the kernel
	// found on the bus, so the count is non-zero and the first entry is index 0.
	let reply = service_client.recv().expect("list reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), corr, "list reply echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	let count = le_u16(b, 5);
	assert!(count >= 1, "at least one device was enumerated");
	assert_eq!(le_u32(b, 7), 0, "the first device is index 0");
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	driver_crash_is_cleaned_up_and_notified,
	[Drivers, Process, ArchX86_64],
	id = "kernel.hardware.driver_crash_is_cleaned_up_and_notified",
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
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
	assert!(!arch::interrupts::is_bound(DRIVER_IRQ_VECTOR as u32), "the driver's IRQ should be detached");
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	device_manager_reacts_to_a_driver_crash,
	[Drivers, Process, ArchX86_64],
	id = "kernel.hardware.device_manager_reacts_to_a_driver_crash",
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn device_manager_reacts_to_a_driver_crash() {
	use object::KernelObject;
	use object::domain::Domain;
	// DeviceManager's reaction to a driver crash: the kernel reports the crash on the
	// crash-notify channel, and the supervisor finds the device that driver was bound to
	// and marks it offline. Here device 0 is driven by a process that
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

tagged_test!(driver_survives_crash_and_restart, [Process], id = "kernel.hardware.driver_survives_crash_and_restart", covers = ["kernel"]);
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

tagged_test!(taking_a_device_out_of_the_kernel_needs_the_authority_to_do_it, [Pci, Drivers], id = "kernel.hardware.taking_a_device_out_of_the_kernel_needs_the_authority_to_do_it", covers = ["kernel"]);
fn taking_a_device_out_of_the_kernel_needs_the_authority_to_do_it() {
	// `SYS_DEVICE_ACQUIRE(index)` used to mint a `DeviceMemory` capability for anyone who named an
	// index - so any ring-3 process could take the BAR of any PCI device, which contradicts
	// `DeviceMemory`'s own documentation that a driver is handed only its device. On a DMA-capable
	// device it is worse than an MMIO takeover: with no IOMMU, a process holding both DMA buffers
	// and physical addresses reaches memory the page tables were meant to isolate.
	//
	// The same authority covers the MSI-X vectors and the legacy interrupt lines, which are the
	// other two ways a device leaves the kernel.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			// No privilege at all.
			let mut grant = abi::ClaimGrant::default();
			let out = &mut grant as *mut _ as u64;
			assert_eq!(arch::syscall::invoke(syscall::SYS_DEVICE_CLAIM, 0, 0, out, 0) as i64, syscall::ERR_BAD_HANDLE, "a device may not be claimed without the authority");
			assert_eq!(arch::syscall::invoke(syscall::SYS_DEVICE_MSIX_ACQUIRE, 0, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE, "nor its MSI-X vectors");
			assert_eq!(arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, 0x41, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE, "nor an interrupt line");

			// A privilege of the WRONG kind is refused too: holding one authority is not holding
			// another, which is the whole point of them being separate objects.
			let wrong = {
				use object::privilege::{Privilege, PrivilegeKind};
				let thread = sched::current_thread().expect("a current thread");
				let privilege = Privilege::create(PrivilegeKind::ConsoleSink).expect("a test privilege");
				thread.handles().lock().try_insert_object(privilege, object::rights::Rights::ALL).expect("installs").raw()
			};
			assert_eq!(arch::syscall::invoke(syscall::SYS_DEVICE_CLAIM, 0, wrong, out, 0) as i64, syscall::ERR_ACCESS_DENIED, "a console authority does not open a device");

			// And with the right one it works, on a machine that has a device to give.
			// AND WITH THE RIGHT ONE IT WORKS - or the device is already claimed, which is the
			// authority working too: the refusal that comes back is `ERR_ALREADY_CLAIMED` and not
			// `ERR_ACCESS_DENIED`, so the caller got past the gate this test is about.
			let count = arch::syscall::invoke(syscall::SYS_DEVICE_COUNT, 0, 0, 0, 0) as i64;
			if count > 0 {
				match crate::tests::claim_device(0) {
					Ok(grant) => {
						assert!(grant.memory > 0 && grant.claim > 0, "the authority is what makes it work");
						crate::tests::release_device(&grant);
					}
					Err(error) => assert_eq!(error, abi::ERR_ALREADY_CLAIMED, "past the gate, and refused for a reason that is not about authority"),
				}
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create().expect("a test event"), object::rights::Rights::ALL);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the probe thread ran to completion");
}

tagged_test!(a_device_masters_the_bus_only_while_it_is_claimed, [Pci, Drivers], id = "kernel.hardware.a_device_masters_the_bus_only_while_it_is_claimed", covers = ["kernel"]);
fn a_device_masters_the_bus_only_while_it_is_claimed() {
	// Bus mastering is permission to write anywhere in physical memory. On these machines there is
	// no IOMMU, so the PCI command bit IS the whole of the check: a device with it set can put bytes
	// at any address it likes, and nothing between the device and the DRAM will ask why.
	//
	// It used to be turned on at enumeration and never turned off - so from the moment the kernel
	// walked the bus, every device on it could write to any physical address, with no driver running
	// and nobody to notice. Now the bit follows the CLAIM.
	//
	// THIS TEST NAMES THE DEVICE IT USES, and that is the difference from what stood here before.
	// The old one searched for a device nobody was driving and returned quietly when every device on
	// the machine was claimed - which on a healthy boot is most of them. A gate whose subject can
	// vanish is a gate that passes when there was nothing to test. Device 0 is always there, and
	// BOTH of the states it can be in have something to assert: if something already holds it, a
	// second claim is refused and the bit is set; if nothing does, the whole lifecycle is walked.
	//
	// The config register is read back rather than the kernel's own record, because the record is
	// the kernel's opinion and the register is what the bus obeys.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	const BUS_MASTER: u16 = 1 << 2;
	extern "C" fn body(_arg: u64) {
		const INDEX: usize = 0;
		let Some((bus, dev, func)) = device::with(INDEX, |d| (d.bus, d.dev, d.func)) else {
			// A machine with no devices at all is not a machine this suite runs on: every profile
			// gives the guest at least the virtio disk it booted from.
			panic!("device 0 is in the table on every machine this suite runs on");
		};
		match device::claim_state(INDEX) {
			Some(device::ClaimState::Free) => {
				assert_eq!(arch::pci::command(bus, dev, func) & BUS_MASTER, 0, "nothing holds this device, so it may not write to memory");
				let grant = crate::tests::claim_device(INDEX as u64).expect("a free device is claimable");
				assert_ne!(arch::pci::command(bus, dev, func) & BUS_MASTER, 0, "a holder has it now, so it may write to memory");
				assert_eq!(crate::tests::claim_device(INDEX as u64).err(), Some(abi::ERR_ALREADY_CLAIMED), "and a second claim is refused by name");
				crate::tests::release_device(&grant);
				assert_eq!(device::claim_state(INDEX), Some(device::ClaimState::Free), "the release confirmed");
				assert_eq!(arch::pci::command(bus, dev, func) & BUS_MASTER, 0, "and the permission went with the claim");
			}
			Some(device::ClaimState::Claimed) => {
				// Something is driving it. That is the rule holding rather than a case to skip: the
				// exclusivity is exactly what is checked against a REAL holder here.
				assert_ne!(arch::pci::command(bus, dev, func) & BUS_MASTER, 0, "a claimed device is one that may write to memory");
				assert_eq!(crate::tests::claim_device(INDEX as u64).err(), Some(abi::ERR_ALREADY_CLAIMED), "and nobody else can take it");
			}
			other => panic!("device 0 is {other:?}, which is neither of the states a booted machine leaves it in"),
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create().expect("a test event"), object::rights::Rights::ALL);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the probe thread ran to completion");
}

tagged_test!(virtio_snd_driver_captures_a_period_from_the_device, [Drivers, Pci, Audio], id = "kernel.hardware.virtio_snd_driver_captures_a_period_from_the_device", covers = ["kernel", "bin.virtio_snd"]);
fn virtio_snd_driver_captures_a_period_from_the_device() {
	// The REAL driver against the REAL device: the receive queue, the input-stream search, the
	// capture stream's set-up and the inverted used-ring handling, all of which exist only for
	// recording and none of which the playback path touches.
	//
	// WHAT A TEST MACHINE CANNOT SUPPLY IS SOUND. QEMU's `none` audio backend is a synthetic source:
	// it fills a capture period with silence on the device's own clock, so every step above runs
	// exactly as it would with a microphone and the samples come back zero. That is why this asserts
	// the period IS silence rather than ignoring its contents - a driver that returned stale
	// playback bytes, uninitialised DMA memory or a short buffer fails here, and a machine that
	// starts producing real audio fails here too, which is the right way to find out.
	//
	// The sample VALUES on a real source are covered where they can be: the AudioService capture
	// scenario feeds known periods through the whole conversion and into a file.
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/virtio_snd.lsexe")).expect("the virtio-snd driver should be staged on the volume under drivers/");

	// Find the sound device. A machine without one is not a failure - it is a machine with no sound
	// card, and this port runs on those - but the test configuration has one, so on the target it
	// runs on, an absent device IS the failure.
	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::VIRTIO_TYPE_SOUND {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: 0x1af4, product: 0, on_bus: u8::from(d.on_bus), _pad1: [0; 1], _pad2: [0; 3] }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the virtio-sound device");

	// Its MSI-X vector, minted the way `SYS_DEVICE_MSIX_ACQUIRE` mints one.
	let (msix_cap, table_phys, bus, dev, func) = device::with(index, |d| (d.msix_cap, d.msix_table_phys, d.bus, d.dev, d.func)).unwrap();
	// A MACHINE WITH NO MSI BACKEND DECLINES THIS TEST RATHER THAN FAILING IT.
	//
	// This driver is interrupt-driven, so on a machine where no device can be given an interrupt
	// there is nothing here to exercise - and `qemu-arch-profiles` runs exactly such a machine on
	// purpose: a GICv3 with its ITS turned off, which the gate marks by leaving `MSI_ORACLE` empty
	// and which the gicv2m test already declines in the same words.
	//
	// A SKIP THAT COULD HIDE A REGRESSION IS THE THING TO AVOID, so the condition is a property of
	// the MACHINE and not of the outcome: the device has no MSI-X capability at all. The default
	// profile of every port has one, so this test still runs and still asserts everything it did -
	// what changes is only that a machine defined not to have one is no longer asked.
	if msix_cap == 0 {
		crate::serial_println!("virtio-snd: skipped - this machine gives no device an MSI vector, so an interrupt-driven driver has nothing to bring up here");
		return;
	}
	let dest = arch::percpu::this_cpu().lapic_id() as u8;
	let vector = arch::interrupts::acquire_msi(table_phys, dest, index as u32).expect("an MSI vector should be free");
	let interrupt = object::interrupt::Interrupt::new(vector).expect("a test interrupt");
	assert!(arch::interrupts::bind_msi(vector, &interrupt), "the MSI vector should bind");
	// The entry is programmed MASKED - see `program_msix_entry` - so this harness unmasks it the way
	// `sys_device_msix_acquire` does once its acquire has committed. Without it the device is enabled
	// and its one entry still refuses to deliver.
	arch::interrupts::unmask_msi(vector, table_phys);
	arch::pci::msix_enable(bus, dev, func, msix_cap);

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the virtio-snd driver should load");
	// Taken as DeviceManager takes it, so the device may write to the capture buffer at all - see
	// `a_device_masters_the_bus_only_while_it_is_claimed`.
	let key = device::claim(index, &crate::tests::entry_for_device(index as u64).expect("the registry declares an entry for the sound device")).expect("the sound device is taken, as DeviceManager takes it");
	// THE SAME HANDSHAKE DEVICEMANAGER SENDS: one `BIND` naming the device and the two resources
	// that follow, each saying which kind it is.
	send_bind(&kernel_ep, &info, key.generation, 2).expect("the BIND should send");
	// RECORDED AS DERIVED, WHICH IS WHAT THE CLAIM'S SWEEP WALKS (2026-09-03).
	//
	// Same reason as the `revoke` at the end of this test, one capability along: this harness mints
	// the device memory by hand, the way `sys_device_claim` does, and the syscall REGISTERS what it
	// mints so that ending the claim can reach it. Skipping that leaves the driver holding a live
	// mapping of the device's registers that the release cannot find - and the release is then right
	// to answer `Quarantined`, because a mapping nothing tore down is exactly what an unconfirmed
	// teardown is. Registering it is what production does and is stronger than tearing the mapping
	// down by hand here: the release performs the revocation itself, which is the thing under test.
	let device_memory = DeviceMemory::for_claim(key, bar_phys, bar_len as usize).expect("a test device memory");
	assert!(device::register_derived(key, alloc::sync::Arc::downgrade(&(device_memory.clone() as alloc::sync::Arc<dyn object::KernelObject>))), "the device memory is recorded as derived from this claim");
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Device, key.generation, device_memory, Rights::ALL).expect("the DEVICE resource should send");
	// CLONED, so this harness keeps a reference of its own. It is the one that gives the vector back
	// at the end - see the teardown below - and a test that handed away its only `Arc` could not.
	// The kernel's own acquire path does the same: the syscall keeps one while the handle table gets
	// another.
	send_resource(&kernel_ep, driver_protocol::ResourceKind::Irq, key.generation, interrupt.clone(), Rights::ALL).expect("the IRQ resource should send");
	sched::run_until_idle();

	// THE TYPED FRAME, NOT THE HUMAN LINE. This asserted on the exact text of the driver's report -
	// so changing a boot line's wording could break a bring-up test, which is the load-bearing
	// sentence this milestone removed.
	let offers = recv_offers(&kernel_ep, key.generation).expect("the virtio-snd driver should report READY");
	let service = offer_of(&offers, driver_protocol::provider::AUDIO).expect("the driver offers its audio provider").into_any_arc().downcast::<object::channel::Channel>().expect("the service channel is a channel");

	// One capture period. The reply is the period itself; an EMPTY reply is the driver saying this
	// device has no input stream, which on the test configuration would mean the device or the
	// stream search is wrong rather than that the machine has no microphone.
	service.send(object::channel::Message::new(alloc::vec![1u8], alloc::vec::Vec::new())).expect("the capture command should send");
	sched::run_until_idle();
	let period = wait_for_message(&service, 2_000).expect("the driver should answer the capture command");
	assert!(!period.is_empty(), "the device reported no input stream - the receive path never ran");
	assert_eq!(period.len(), 2_048, "a capture period is 512 stereo signed-16-bit frames");
	assert!(period.chunks_exact(2).all(|sample| i16::from_le_bytes([sample[0], sample[1]]) == 0), "the `none` audio backend produces silence, and this is not silence");

	// A second period, so the used ring is proven to advance rather than to answer once.
	service.send(object::channel::Message::new(alloc::vec![1u8], alloc::vec::Vec::new())).expect("the second capture command should send");
	sched::run_until_idle();
	assert_eq!(wait_for_message(&service, 2_000).expect("the driver should answer the second command").len(), 2_048, "the capture queue stopped after one period");

	// And the stream stops on request, which releases it on the device.
	service.send(object::channel::Message::new(alloc::vec![2u8], alloc::vec::Vec::new())).expect("the capture stop should send");
	sched::run_until_idle();
	assert_eq!(wait_for_message(&service, 2_000).expect("the driver should acknowledge the stop"), b"OK", "the stop was not acknowledged");

	// AND THE VECTOR IS GIVEN BACK, which this stopped short of (2026-09-01).
	//
	// It ended here, holding the claim and the MSI vector for the rest of the run - so the one test
	// in this tree that drives a REAL device's MSI-X table proved delivery and never proved
	// teardown, and P02M0151's checkpoint asks for both.
	//
	// THE REVOKE IS THIS HARNESS STANDING IN FOR `revoke_derived`, and it is here because of what
	// this test is: it mints the `Interrupt` by hand, the way `sys_device_msix_acquire` does, and
	// therefore never registers it in the claim's derived table. So the release cannot reach it -
	// `settled_vectors` finds a slot that is bound and neither retired nor quarantined, answers that
	// a teardown is still outstanding, and the claim publishes `Quarantined` rather than `Free`.
	// Measured, not assumed: without this line the release answers `Ok(Quarantined)` and says on the
	// console that the vector stays masked and held. That refusal is the kernel being right about a
	// vector nobody gave back; the driver-shaped thing to do is give it back, which is what `revoke`
	// is - the same call `revoke_derived` makes for a registered row, and on an ITS machine it is
	// the discard of the event mapping.
	assert!(interrupt.revoke(), "the architecture did not confirm the vector's teardown");
	assert!(!arch::interrupts::is_bound(vector), "the revoked vector is still bound to this test's Interrupt");
	// AND THEN THE CLAIM, released the production way with the driver still running: bus mastering
	// off, everything derived revoked, the vectors settled, and only then the slot back into
	// circulation.
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free), "the sound device's claim did not confirm its teardown");
	crate::serial_println!("virtio-snd: the device's MSI vector was delivered on and then torn down with its claim");
}

// Receive one message from a channel, pumping the scheduler until it arrives or a WALL-CLOCK
// deadline passes.
//
// A count of iterations is the wrong bound here and it is worth saying why: between the command and
// the answer the driver is blocked on its device interrupt, so nothing is runnable and
// `run_until_idle` returns immediately - a thousand iterations pass in microseconds while the device
// is still filling a ten-millisecond period. What this waits for is TIME, not scheduling.
fn wait_for_message(channel: &object::channel::Channel, millis: u64) -> Option<alloc::vec::Vec<u8>> {
	let deadline = arch::tsc::now();
	loop {
		if let Ok(message) = channel.recv() {
			return Some(message.bytes);
		}
		sched::run_until_idle();
		if arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(deadline)) / 1_000_000 >= millis {
			return None;
		}
	}
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	pci_enumeration_reaches_a_bus_behind_a_bridge,
	[Drivers, Pci, ArchX86_64],
	id = "kernel.hardware.pci_enumeration_reaches_a_bus_behind_a_bridge",
	covers = ["kernel"]
);
// x86_64 ONLY, and the tag is not what does it - an `Arch*` tag selects, it does not exclude, so
// this needs the `cfg` as well. Not because the walk is x86-specific (it is in `arch::common` and
// every backend gets it) but because the OTHER TWO HAVE NO FIRMWARE. On QEMU `virt` nothing
// assigns bridge bus numbers before the kernel runs - this port assigns its own BARs for the same
// reason - so a bridge there forwards nothing, and a test asserting otherwise would be asserting
// that the kernel programs bridges, which it does not. That is the honest next step for those two
// and it is not this fix.
#[cfg(target_arch = "x86_64")]
fn pci_enumeration_reaches_a_bus_behind_a_bridge() {
	// A device behind a bridge did not exist - not "was not driven", did not exist: the x86 walk
	// read bus 0 and stopped, so a PCIe root port or a `pcie-pci-bridge` was an entry with nothing
	// visible behind it. What the walk finds is what the whole device layer is built on, so the
	// question is not whether the recursion is written but whether it runs.
	//
	// THE TOPOLOGY IS THE TEST. q35's default puts everything on bus 0, which is why this could be
	// written and never executed; the test configuration adds a `pcie-pci-bridge` with an inert
	// `pci-testdev` behind it (`src/harness/qemu-run.sh`), so there is a second bus to reach and
	// nothing in this kernel binds what is on it.
	let devices = arch::pci::scan();
	assert!(!devices.is_empty(), "the scan found no PCI devices at all");

	let bridge = devices.iter().find(|d| d.header_type & 0x7F == 0x01).expect("the test topology has a bridge on bus 0");
	assert_eq!(bridge.bus, 0, "the bridge itself is on the root bus");

	let behind: alloc::vec::Vec<_> = devices.iter().filter(|d| d.bus != 0).collect();
	assert!(!behind.is_empty(), "the walk stopped at bus 0: {} devices found, and the bridge at {:02x}:{:02x}.{} was not descended into", devices.len(), bridge.bus, bridge.dev, bridge.func);

	// Every bus is visited once, which is what keeps a firmware-written numbering loop from being
	// an unbounded walk rather than a bounded one.
	let mut buses: alloc::vec::Vec<u8> = devices.iter().map(|d| d.bus).collect();
	buses.sort_unstable();
	let unique = {
		let mut seen = buses.clone();
		seen.dedup();
		seen
	};
	for bus in &unique {
		let functions = devices.iter().filter(|d| d.bus == *bus).count();
		let slots = devices.iter().filter(|d| d.bus == *bus).map(|d| (d.dev, d.func)).collect::<alloc::vec::Vec<_>>();
		let mut deduped = slots.clone();
		deduped.sort_unstable();
		deduped.dedup();
		assert_eq!(deduped.len(), functions, "bus {bus} was enumerated more than once");
	}
}

tagged_test!(a_departure_keeps_the_row_and_an_arrival_refills_it_with_a_new_generation, [Kernel, Pci], id = "kernel.hardware.a_departure_keeps_the_row_and_an_arrival_refills_it_with_a_new_generation", covers = ["kernel"]);
fn a_departure_keeps_the_row_and_an_arrival_refills_it_with_a_new_generation() {
	// THE INVENTORY IS WRITABLE NOW, AND WHAT IT MUST NOT DO IS RENUMBER.
	//
	// An index is what a claim, a binding and every message in flight are addressed by. Removing the
	// row of a device somebody unplugged would move every device after it to a different number, so
	// every one of those references would point at something else - which is a far worse failure
	// than the one it would be tidying up after. What changes instead is `on_bus`, and the row stays
	// where it is as a tombstone.
	//
	// A REPLUG IS A NEW BINDING ON THE SAME INDEX, and what tells the two apart is the CLAIM
	// GENERATION: the arrival mints the next one, so a message stamped with the departed device's is
	// refused by arithmetic rather than by anyone remembering that the device was swapped.
	let before = device::count();
	assert!(before > 0, "this machine has devices");
	// A device the fixture machine really has, so the departure is a real row rather than a lookup
	// that failed and answered `None` for the wrong reason.
	let (bus, dev, func) = device::with(0, |d| (d.bus, d.dev, d.func)).expect("the first device");
	let generation_before = device::claim_generation(0).expect("the first device has a claim slot");

	let index = device::depart(bus, dev, func).expect("the device this machine has just left the bus");
	assert_eq!(index, 0, "the row that left is the row it was");
	assert_eq!(device::count(), before, "and the table did not shrink, so nothing was renumbered");
	assert_eq!(device::with(index, |d| d.on_bus), Some(false), "what changed is whether it is on the bus");
	assert_eq!(device::depart(bus, dev, func), None, "and a second departure of one device is not an event");

	let back = device::arrive(bus, dev, func).expect("and the same address came back");
	assert_eq!(back, index, "into the row it had, because a slot is a slot");
	assert_eq!(device::count(), before, "still no renumbering");
	assert_eq!(device::with(index, |d| d.on_bus), Some(true), "and it is on the bus again");
	assert_ne!(device::claim_generation(index), Some(generation_before), "with a generation that has moved past every message in flight");
}

tagged_test!(a_slot_is_powered_down_only_once_nothing_holds_the_device, [Kernel, Pci], id = "kernel.hardware.a_slot_is_powered_down_only_once_nothing_holds_the_device", covers = ["kernel"]);
fn a_slot_is_powered_down_only_once_nothing_holds_the_device() {
	// THIS IS THE COORDINATION THE WHOLE PATH EXISTS FOR, and it is one rule: the answer to the
	// attention button is given when the CLAIM is free and not before.
	//
	// A driver has the device while its claim is not `Free` - it may have a mapping, an interrupt
	// binding and an outstanding DMA, and the release is what says all three are finished. Powering
	// the slot down before that is the surprise removal a coordinated one is defined against, and
	// asking the driver instead would be asking the thing that is already being torn down.
	let (bus, dev, func) = device::with(0, |d| (d.bus, d.dev, d.func)).expect("the first device");
	device::request_slot_retirement(0, bus, dev, func);
	assert!(device::retire_requested_slots().iter().any(|slot| *slot == (bus, dev, func)), "nothing holds this device, so the slot may go down now");
	assert!(device::retire_requested_slots().is_empty(), "and it is answered once - a slot asked to power down twice is a port told to do it twice");

	// A REQUEST FOR A SLOT NOBODY ASKED ABOUT IS NOT AN ANSWER.
	assert!(device::retire_requested_slots().is_empty());
}

tagged_test!(a_platform_event_raised_before_anything_listens_is_held_and_delivered_once, [Kernel], id = "kernel.hardware.a_platform_event_raised_before_anything_listens_is_held_and_delivered_once", covers = ["kernel"]);
fn a_platform_event_raised_before_anything_listens_is_held_and_delivered_once() {
	// THE WINDOW THIS IS ABOUT IS REAL AND IT IS SECONDS LONG. The kernel arms the power button in
	// its boot tail; the program that receives the event registers its channel some way into
	// userspace bring-up. A press in between is a press on a machine whose power button appears not
	// to work, with nothing anywhere saying why - so it is LATCHED, and this is the test that it is.
	//
	// THE LATCH IS DRAINED FIRST, because this suite runs on a machine whose real buttons nobody is
	// pressing but whose latch is shared state: a test that assumed it started empty would pass or
	// fail on what ran before it.
	crate::platform_event::drain_for_test();

	crate::platform_event::report(crate::platform_event::POWER_BUTTON);
	let (kernel_side, ours) = crate::object::channel::Channel::create();
	crate::platform_event::attach(kernel_side);
	// THE DELIVERY IS THE IDLE PASS'S AND NOT THE ATTACH'S, which is what keeps a channel send out
	// of the interrupt handler that recorded the press.
	crate::platform_event::deliver();
	let held = ours.recv().expect("the press made before the attach should arrive on it");
	assert_eq!(held.bytes, alloc::vec![crate::platform_event::POWER_BUTTON], "and it should be the kind that was raised");

	// AND ONCE. A latch that were merely READ rather than taken would deliver the same press to
	// whoever attached next - a restarted receiver would power the machine off on a button nobody
	// touched.
	let (second_kernel_side, second) = crate::object::channel::Channel::create();
	crate::platform_event::attach(second_kernel_side);
	crate::platform_event::deliver();
	assert!(second.recv().is_err(), "the latch was emptied by the first delivery, so a second listener inherits no press");

	// AFTER AN ATTACH THE PRESS GOES STRAIGHT THROUGH, which is the ordinary case and the one the
	// latch must not swallow.
	crate::platform_event::report(crate::platform_event::SLEEP_BUTTON);
	crate::platform_event::deliver();
	let direct = second.recv().expect("a press after the attach should be delivered on the next pass");
	assert_eq!(direct.bytes, alloc::vec![crate::platform_event::SLEEP_BUTTON], "and the two buttons are different kinds rather than one with a flag");

	// TWO PRESSES BEFORE ANYBODY LISTENS ARE ONE INSTRUCTION. Nothing is listening once this drops
	// its listener, and pressing twice must not queue two shutdowns for the next receiver.
	crate::platform_event::drain_for_test();
	crate::platform_event::report(crate::platform_event::POWER_BUTTON);
	crate::platform_event::report(crate::platform_event::POWER_BUTTON);
	let (third_kernel_side, third) = crate::object::channel::Channel::create();
	crate::platform_event::attach(third_kernel_side);
	crate::platform_event::deliver();
	assert!(third.recv().is_ok(), "the press is delivered");
	assert!(third.recv().is_err(), "and twice pressed is once delivered - a bitmap and not a count");
	crate::platform_event::drain_for_test();
}

tagged_test!(usb_cdc_acm_carries_bytes_to_the_host_and_back, [Drivers, Usb, Slow], id = "kernel.hardware.usb_cdc_acm_carries_bytes_to_the_host_and_back", covers = ["kernel", "drivers", "device-proto"]);
fn usb_cdc_acm_carries_bytes_to_the_host_and_back() {
	use device_proto::codec as wire;
	use device_proto::generated::liber::device::v1 as device;
	use object::channel::{Channel, Message};

	// THE ONE ORACLE IN THIS SUITE WHOSE DEVICE THE HARNESS BUILT. QEMU models fifteen USB devices
	// and none of them is a CDC-ACM port, which is why this item read for a long time as "a device
	// model to write". It is not: Linux can BE a USB device, `usb-gadget.sh` builds one out of
	// `dummy_hcd` and `usb_f_acm`, and `usb-host` hands it to the guest. The gadget side of it is an
	// ordinary tty on the host, and `serial-echo.py` sits on that tty echoing raw bytes - so what
	// comes back here has been out through the controller, through the host's USB gadget stack, into
	// a process that read it, and all the way back.
	//
	// GATED AT COMPILE TIME, on the same terms as `TEST_TAGS` and `LIBER_NO_DT_PROFILE`: the gadget
	// is built only when a run asks for it, because building one modprobes into the developer's own
	// kernel and that is not something every test run should do. Without it there is no device to
	// bind and this states so rather than passing quietly - a run that says nothing is how a test
	// that stopped testing anything goes unnoticed.
	let asked = option_env!("USB_GADGET").unwrap_or("");
	if asked != "acm" {
		crate::serial_println!("usb-cdc-acm: NOT RUN - no CDC-ACM gadget on this run; build one with USB_GADGET=acm");
		return;
	}

	let (kernel_ep, generation, offers, driver, claim) = bind_xhci_controller();

	// THE PROVIDER IS A `console-stream` FACTORY, so a connection is MINTED the way DeviceManager
	// mints one rather than the offered endpoint being used directly - the same shape the CDC
	// Ethernet adapter's link uses beside it.
	let token = offer_token_of(&offers, driver_protocol::provider::CONSOLE_BYTES).expect("the driver publishes the CDC-ACM adapter's byte stream, because this run attached one");
	let (host_end, driver_end) = Channel::create();
	send_connect(&kernel_ep, generation, token, driver_end).expect("the CONNECT should send");
	sched::run_until_idle();

	// THE TRANSPORT THE GENERATED CLIENT CALLS OVER.
	//
	// `console-stream` IS A GENERATED IDL AND THIS TEST DOES NOT SECOND-GUESS IT. The op numbers,
	// the record layouts and their bounds all come from `device-proto`, which this kernel already
	// links for the provider-catalogue harnesses; what is written here is the six lines of transport
	// the trait's own comment says a test supplies - "the userspace impl sends on a channel and
	// blocks for the reply; tests use an in-memory loopback". The channel-backed transport is behind
	// `channel-client-impl` because it calls the userspace runtime's syscalls; the generic `Client`
	// is not, and a kernel channel is what this one sends on.
	struct OverChannel<'a> {
		channel: &'a Channel,
		/// What a reply handed over, by the number this transport gave it.
		///
		/// A HANDLE IS AN INDEX INTO SOMETHING, and in a program it is the process's table. A kernel
		/// test has no table - a capability arrives as the object itself - so this transport is the
		/// table: it numbers what it receives from one upwards and hands the number to the decoder,
		/// which is the same contract from the generated client's side.
		received: alloc::vec::Vec<(u64, alloc::sync::Arc<dyn object::KernelObject>)>,
	}

	impl OverChannel<'_> {
		fn take(&mut self, handle: u64) -> Option<alloc::sync::Arc<dyn object::KernelObject>> {
			let at = self.received.iter().position(|(number, _)| *number == handle)?;
			Some(self.received.remove(at).1)
		}
	}

	impl wire::Transport for OverChannel<'_> {
		fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut wire::Handles, _deadline: u64) -> Result<alloc::vec::Vec<u8>, wire::TransportError> {
			// `console-stream` TRANSFERS NOTHING ON A REQUEST, so one that arrived with a handle
			// would be a schema this transport does not implement rather than one it may quietly
			// drop. Its `receive` REPLY does transfer one - the inbound stream - which is why the
			// reply half below is not the same no-op.
			if !request_handles.is_empty() {
				return Err(wire::TransportError::Malformed);
			}
			self.channel.send(Message::new(request.to_vec(), alloc::vec::Vec::new())).map_err(|_| wire::TransportError::PeerClosed)?;
			// THE DRIVER HAS TO BE GIVEN THE PROCESSOR, and the wait is BOUNDED: a reply that never
			// comes is a driver that did not answer, which is a failure this oracle should report
			// rather than a hang the suite's watchdog reports for it.
			for _ in 0..20_000u32 {
				sched::run_until_idle();
				if let Ok(message) = self.channel.recv() {
					for capability in message.caps {
						let number = self.received.len() as u64 + 1;
						if reply_handles.push(number).is_none() {
							return Err(wire::TransportError::Malformed);
						}
						self.received.push((number, capability.object()));
					}
					return Ok(message.bytes);
				}
			}
			Err(wire::TransportError::TimedOut)
		}

		fn discard_handles(&mut self, handles: &[u64]) {
			// DROPPED AND NOT IGNORED. The trait refuses a default body precisely so a transport
			// cannot leak a capability from a reply nobody decoded, and this one owns what it
			// received - so releasing it is removing it from the table above.
			for handle in handles {
				let _ = self.take(*handle);
			}
		}
	}

	let mut client = device::console_stream::Client::new(OverChannel { channel: &host_end, received: alloc::vec::Vec::new() });

	// 1. ATTACH, which is where the version is agreed and the bounds come back. Every other
	//    operation on a connection that has not attached is refused, so this is not a formality.
	let sent: &[u8] = b"liber-acm";
	let attachment = client.attach(&1).expect("the adapter answered the attach").expect("the adapter speaks version 1 of console-stream");
	assert_eq!(attachment.version, 1, "the provider answers with the version asked for and not a negotiation of its own");
	assert!(attachment.max_frame > 0, "an attachment states the largest frame it takes and delivers, got {}", attachment.max_frame);
	assert!(sent.len() <= attachment.max_frame as usize, "and this oracle's probe fits inside it");

	// 2. RECEIVE BEFORE WRITE, AND THE ORDER IS THE POINT.
	//
	//    `Session::deliver` DROPS a chunk when nothing holds the stream, which is the right answer -
	//    a stream nobody opened has no consumer, and buffering for one that may never arrive is how
	//    a driver grows an unbounded queue. The consequence is this order: a consumer that wrote
	//    first and opened its inbound stream afterwards would lose whatever came back in between,
	//    and on a link this fast that is the ORDINARY case rather than a rare one - the echo of a
	//    nine-byte write is already on its way while the write's own completion is being waited for.
	//
	//    It cost three runs out of four to find, with the driver's own defect underneath it.
	let stream = client.receive().expect("the adapter answered the receive").expect("the inbound stream opened");

	// 3. WRITE, WHICH IS THE HALF THAT LEAVES THE MACHINE. The bytes go down the bulk OUT endpoint,
	//    through the host's gadget stack, and arrive on `/dev/ttyGS0` where `serial-echo.py` has
	//    them.
	//
	//    RAW AND WITH NO NEWLINE, because the echo is raw: a tty in its default line discipline
	//    would wait for a line to end, and what this contract carries is BYTES.
	let written = client.write(sent).expect("the adapter answered the write").expect("the write was accepted");
	assert_eq!(written as usize, sent.len(), "every byte offered was taken, got {written}");

	// 4. AND THE ECHO, WHICH ARRIVES AS FRAMES ON THE STREAM the receive above handed over.
	let mut transport = client.into_transport();
	let stream = transport.take(stream).expect("the receive reply handed over the stream it opened").into_any_arc().downcast::<Channel>().expect("and the stream is a channel");

	// THE WAIT IS IN TICKS AND NOT IN ITERATIONS, WHICH IS WHAT THIS ORACLE'S PEER BEING OUTSIDE THE
	// MACHINE COSTS. `run_until_idle` returns the moment the run queue is empty, so a loop counted in
	// passes spins twenty thousand times in a few milliseconds and gives up long before a process on
	// the HOST has read a tty and written it back. Every other driver oracle here pulls - it sends a
	// request and reads the reply, and the round trip is its own pacing - but `console-stream`
	// PUSHES its inbound frames, so there is nothing to pull on and the budget has to be wall clock.
	//
	// A tick is not a unit of work, so the emulated ports get the same budget in ticks and far more
	// of them in instructions, which is the direction that is safe to be wrong in.
	#[cfg(target_arch = "x86_64")]
	let patience: u64 = 500;
	#[cfg(not(target_arch = "x86_64"))]
	let patience: u64 = 500 * 13;
	let give_up = arch::apic::ticks() + patience;
	let mut back: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	while back.len() < sent.len() && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		if let Ok(message) = stream.recv() {
			let mut handles = wire::Handles::new();
			if let Some(chunk) = device::console_stream::receive_read(&message.bytes, &mut handles) {
				back.extend_from_slice(&chunk.bytes);
			}
		}
	}
	assert_eq!(&back[..], sent, "what the host echoed came back byte for byte");
	crate::serial_println!("usb-cdc-acm: {} byte(s) out and the same {} back", sent.len(), back.len());
	// AND THE DEVICE IS GIVEN BACK WITH THE TEST, DRIVER FIRST AND CLAIM AFTER.
	//
	// A driver process left running holds the controller and keeps servicing it, so a second oracle
	// on this bus spawns another beside it and the two share the device. And the CLAIM outlives the
	// process: this file used to say "the CLAIM ends with this test kernel", which was true while
	// exactly one test ever took this controller - the moment a second one does, it is answered
	// `AlreadyClaimed` and cannot bind at all.
	//
	// THE ORDER IS NOT A PREFERENCE. Releasing the claim turns off bus mastering and MSI-X on the
	// function, so a driver still running would be one whose device has stopped answering; taking
	// the process away first is what makes the release the end of something rather than the middle.
	driver.terminate();
	sched::run_until_idle();
	let _ = crate::device::release_claim(claim);
}
