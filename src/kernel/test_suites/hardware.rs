use super::*;

tagged_test!(device_memory_maps_mmio_region, [Drivers]);
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	interrupt_bind_delivers_to_driver,
	[Drivers, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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

tagged_test!(device_table_exposes_virtio_mmio, [Drivers]);
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

tagged_test!(device_table_exposes_the_xhci_controller, [Drivers, Usb]);
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
				let handle = arch::syscall::invoke(syscall::SYS_DEVICE_ACQUIRE, i, 0, 0, 0);
				if !syscall::sys_is_err(handle) {
					MAPPED.store(arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, handle, 0, 0, 0), Ordering::SeqCst);
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

tagged_test!(xhci_driver_enumerates_the_usb_bus, [Drivers, Usb, Slow]);
fn xhci_driver_enumerates_the_usb_bus() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// The userspace xhci driver, driven the way DeviceManager drives it: spawn its
	// staged ELF (it lives on the system volume under drivers/, not in the init
	// package) with a bootstrap channel, hand it "DEVICE" + the controller's
	// DeviceInfo + a DeviceMemory capability to its register file and "IRQ" + its
	// MSI-X Interrupt capability, and wait for its report. The driver resets the
	// controller, builds the command and event rings, enumerates the root-hub
	// ports, addresses each connected device and reads its device descriptor - QEMU
	// hangs a hub with a USB keyboard and a USB tablet behind it and a mass-storage
	// stick off the controller (see qemu-run.sh), so four devices must come back
	// addressed: the hub (expanded through its class requests and route strings),
	// the keyboard and the tablet behind it (their HID interfaces configured and
	// their report descriptors parsed, which the report's keyboard and pointer
	// markers prove), and the stick (its Bulk-Only transport brought up).
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/xhci.lsexe")).expect("the xhci.lsexe driver should be staged on the volume under drivers/");

	// find the controller in the device table and mint its MMIO capability.
	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_XHCI {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset }).unwrap();
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
	let interrupt = object::interrupt::Interrupt::new(vector);
	assert!(arch::interrupts::bind_msi(vector, &interrupt), "the MSI vector should bind");
	arch::pci::msix_enable(bus, dev, func, msix_cap);

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL, 0).expect("the xhci driver should load");
	let mut msg = alloc::vec::Vec::with_capacity(6 + core::mem::size_of::<abi::DeviceInfo>());
	msg.extend_from_slice(b"DEVICE");
	msg.extend_from_slice(unsafe { core::slice::from_raw_parts(&info as *const abi::DeviceInfo as *const u8, core::mem::size_of::<abi::DeviceInfo>()) });
	send_cap(&kernel_ep, &msg, DeviceMemory::new(bar_phys, bar_len as usize), Rights::ALL).expect("the DEVICE handoff should send");
	send_cap(&kernel_ep, b"IRQ", interrupt, Rights::ALL).expect("the IRQ handoff should send");
	sched::run_until_idle();

	let report = kernel_ep.recv().expect("the xhci driver should report in");
	assert_eq!(&report.bytes[..], b"driver.xhci: online (4 device(s)) (keyboard) (pointer) (storage)", "the driver should expand the hub, address the QEMU USB keyboard and tablet behind it and the stick, and configure all three classes");

	// the report is followed by the bus query channel ("USBBUS"): drive one raw
	// `usb.list` request over it ([op u16][correlation u32], the generated wire
	// header) and expect a successful reply naming all four devices' roles - the
	// live inventory `lsusb` reads.
	let usbq_msg = kernel_ep.recv().expect("the USBBUS message should follow the report");
	assert_eq!(&usbq_msg.bytes[..], b"USBBUS", "the second message carries the bus query channel");
	let usbq_cap = usbq_msg.caps.first().expect("the query channel is transferred with it");
	let usbq = usbq_cap.object().into_any_arc().downcast::<Channel>().expect("the query channel is a channel");
	// the pointer-event channel follows ("POINTER"): the raw stream a USB pointing
	// device's reports feed, routed to InputService live.
	let ptr_msg = kernel_ep.recv().expect("the POINTER message should follow USBBUS");
	assert_eq!(&ptr_msg.bytes[..], b"POINTER", "the third message carries the pointer-event channel");
	assert!(ptr_msg.caps.first().is_some(), "the pointer channel is transferred with it");
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&1u16.to_le_bytes()); // OP_LIST
	list.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	usbq.send(Message::new(list, alloc::vec::Vec::new(), 0)).expect("the usb.list request should send");
	sched::run_until_idle();
	let inventory = usbq.recv().expect("the usb.list reply should arrive");
	assert!(inventory.bytes.len() >= 5 && inventory.bytes[4] == 1, "the inventory query should succeed");
	let has = |needle: &[u8]| inventory.bytes.windows(needle.len()).any(|w| w == needle);
	assert!(has(b"hub") && has(b"keyboard") && has(b"pointer") && has(b"storage"), "the inventory should name the hub, the keyboard, the tablet and the stick by role");

	// the report carries the disk's block channel: read sector 0 over it, the same
	// [op u32][lba u64][count u32] contract driver.virtio-blk serves, and expect a
	// success status plus a 512-byte shared buffer.
	let cap = report.caps.first().expect("the block channel is transferred with the report");
	let blk = cap.object().into_any_arc().downcast::<Channel>().expect("the block channel is a channel");
	// first the capacity query (op 2): the reply is [status u32][capacity bytes u64]
	// and must report the seeded 16 MB stick image.
	let mut capacity = alloc::vec::Vec::with_capacity(16);
	capacity.extend_from_slice(&2u32.to_le_bytes()); // op = capacity
	capacity.extend_from_slice(&0u64.to_le_bytes());
	capacity.extend_from_slice(&0u32.to_le_bytes());
	blk.send(Message::new(capacity, alloc::vec::Vec::new(), 0)).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	assert_eq!(&cap_reply.bytes[..4], &0u32.to_le_bytes(), "the capacity query should succeed");
	let bytes = u64::from_le_bytes([cap_reply.bytes[4], cap_reply.bytes[5], cap_reply.bytes[6], cap_reply.bytes[7], cap_reply.bytes[8], cap_reply.bytes[9], cap_reply.bytes[10], cap_reply.bytes[11]]);
	assert_eq!(bytes, 16 * 1024 * 1024, "the stick should report its seeded 16 MB capacity");
	let mut request = alloc::vec::Vec::with_capacity(16);
	request.extend_from_slice(&0u32.to_le_bytes()); // op = read
	request.extend_from_slice(&0u64.to_le_bytes()); // lba 0
	request.extend_from_slice(&1u32.to_le_bytes()); // one sector
	blk.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("the block request should send");
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
	loader::spawn_elf_process(sched::root_domain(), service_elf, service_boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&service_boot_kernel, b"USBBLOCK", blk, Rights::ALL).expect("the USBBLOCK handoff should send");
	send_cap(&service_boot_kernel, b"SERVE", service_server, Rights::ALL).expect("the SERVE handoff should send");
	sched::run_until_idle();
	let online = service_boot_kernel.recv().expect("the usb StorageService should report in");
	assert_eq!(&online.bytes[..], b"StorageService: online", "the instance should come up without touching the media (the mount is lazy)");

	// one generated volume.open request for a seeded file, plus the quit sentinel.
	let uri: &[u8] = b"vol://usb/hello.txt";
	let mut open = alloc::vec::Vec::new();
	open.extend_from_slice(&1u16.to_le_bytes()); // OP_OPEN
	open.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	open.extend_from_slice(&(uri.len() as u16).to_le_bytes());
	open.extend_from_slice(uri);
	open.push(0); // write = false
	open.push(0); // create = false
	service_client.send(Message::new(open, alloc::vec::Vec::new(), 0)).expect("the open request should send");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("the quit sentinel should send");
	sched::run_until_idle();
	let reply = service_client.recv().expect("the open reply should arrive");
	assert!(reply.bytes.len() >= 17 && reply.bytes[4] == 1, "the usb volume should resolve the seeded file");
	let size = u64::from_le_bytes([reply.bytes[9], reply.bytes[10], reply.bytes[11], reply.bytes[12], reply.bytes[13], reply.bytes[14], reply.bytes[15], reply.bytes[16]]) as usize;
	let file_cap = reply.caps.first().expect("the open should grant the file buffer");
	let file_object = file_cap.object();
	let file = file_object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	let expected = volume_file(volume2, b"hello.txt").expect("hello.txt should be in the volume package");
	assert_eq!(read_from_object(file, size), expected, "vol://usb should serve the seeded file's bytes");
}

tagged_test!(dma_buffer_maps_and_reports_phys, [Drivers, Memory]);
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

tagged_test!(device_service_lists_devices, [Service, Drivers]);
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
	[Drivers, Process, ArchX86_64]
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	device_manager_reacts_to_a_driver_crash,
	[Drivers, Process, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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

tagged_test!(driver_survives_crash_and_restart, [Process]);
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
