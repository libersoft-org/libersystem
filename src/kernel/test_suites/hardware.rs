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

tagged_test!(xhci_driver_enumerates_the_usb_bus, [Drivers, Usb, Slow], id = "kernel.hardware.xhci_driver_enumerates_the_usb_bus", covers = ["kernel", "bin.xhci"]);
fn xhci_driver_enumerates_the_usb_bus() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
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
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/xhci.lsexe")).expect("the xhci.lsexe driver should be staged on the volume under drivers/");

	// find the controller in the device table and mint its MMIO capability.
	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_XHCI {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: 0x1af4, product: 0, _pad1: [0; 1], _pad2: [0; 4] }).unwrap();
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
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL).expect("the xhci driver should load");
	let mut msg = alloc::vec::Vec::with_capacity(6 + core::mem::size_of::<abi::DeviceInfo>());
	msg.extend_from_slice(b"DEVICE");
	msg.extend_from_slice(unsafe { core::slice::from_raw_parts(&info as *const abi::DeviceInfo as *const u8, core::mem::size_of::<abi::DeviceInfo>()) });
	// THE CAPABILITY IS MINTED THE WAY `SYS_DEVICE_CLAIM` MINTS IT - for the device index, not for
	// a bare physical range - and the device is taken at the same moment. This harness is standing in
	// for DeviceManager, and taking the device is what lets it write to memory: a controller handed
	// only its BAR would bring its rings up, ring the doorbell, and wait forever for a completion the
	// bus would not let it write. Bus mastering goes off when the driver process dies and the
	// transferred capability dies with it; the CLAIM ends with this test kernel.
	let key = device::claim(index).expect("the xHCI controller is taken, as DeviceManager takes it");
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
	// The bus query channel: drive one raw `usb.list` request over it ([op u16][correlation u32],
	// the generated wire header) and expect a successful reply naming all four devices' roles - the
	// live inventory `lsusb` reads.
	let usbq = offer_of(&offers, driver_protocol::provider::USB_BUS).expect("the driver offers its bus query channel").into_any_arc().downcast::<Channel>().expect("the query channel is a channel");
	assert!(offer_of(&offers, driver_protocol::provider::POINTER).is_some(), "and its pointer-event channel, the raw stream a USB pointing device's reports feed");
	assert!(offer_of(&offers, driver_protocol::provider::BLOCK).is_some(), "and the USB stick's block service, because this machine has one attached");
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&1u16.to_le_bytes()); // OP_LIST
	list.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	usbq.send(Message::new(list, alloc::vec::Vec::new())).expect("the usb.list request should send");
	sched::run_until_idle();
	let inventory = usbq.recv().expect("the usb.list reply should arrive");
	assert!(inventory.bytes.len() >= 5 && inventory.bytes[4] == 1, "the inventory query should succeed");
	let has = |needle: &[u8]| inventory.bytes.windows(needle.len()).any(|w| w == needle);
	assert!(has(b"hub") && has(b"keyboard") && has(b"pointer") && has(b"storage"), "the inventory should name the hub, the keyboard, the tablet and the stick by role");

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
	covers = ["kernel", "bin.device_manager"]
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
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: 0x1af4, product: 0, _pad1: [0; 1], _pad2: [0; 4] }).unwrap();
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
	let key = device::claim(index).expect("the sound device is taken, as DeviceManager takes it");
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
