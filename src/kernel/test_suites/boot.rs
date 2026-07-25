use super::*;

tagged_test!(system_packages_use_canonical_executable_names, [Boot, Storage, VolumeLayout]);
fn system_packages_use_canonical_executable_names() {
	let init = pkg::Package::parse(init_package_bytes().expect("init package present")).expect("init package parses");
	let volume = pkg::Package::parse(volume_package_bytes().expect("volume package present")).expect("volume package parses");
	for index in 0..init.len() {
		let name = init.name(index).expect("init entry name");
		assert!(name.ends_with(b".lsexe"), "init package contains an extensionless native artifact");
	}
	for index in 0..volume.len() {
		let name = volume.name(index).expect("volume entry name");
		let name = core::str::from_utf8(name).expect("volume entry name is UTF-8");
		assert!(test_volume_path_is_declared(name), "system volume contains an undeclared entry {name}");
		if name.starts_with("bin/") || name.starts_with("libexec/") || name.starts_with("drivers/") {
			assert!(name.ends_with(abi::EXECUTABLE_SUFFIX), "system volume contains an extensionless native artifact");
		}
	}
	for stale in [b"app.wasm" as &[u8], b"config.tree", b"bin/wasi_host.lsexe", b"bin/config_service.lsexe"] {
		assert!(volume.lookup(stale).is_none(), "system volume retains stale path {}", core::str::from_utf8(stale).unwrap());
	}
	for package in [&init, &volume] {
		for index in 0..package.len() {
			let name = package.name(index).expect("package entry name");
			if name.ends_with(b".lsexe") {
				let mut collision = name.to_vec();
				collision.extend_from_slice(b".lsexe");
				assert!(package.lookup(&collision).is_none(), "package contains an ambiguous executable alias pair");
			}
		}
	}
}

// This end-to-end test asserts the complete boot-chain report set, which requires the
// interrupt-driven services (NetworkService over virtio-net, and its transitive
// dependents TimeService/PermissionManager/ConsoleService/SystemGraphService/Shell) to
// all settle inside the harness's single `run_until_idle()`. It was previously gated off
// riscv64 (`#[cfg(not(target_arch = "riscv64"))]`) because those services intermittently
// failed to report in there - which turned out to be the riscv trap-frame register clobber
// (a trap could corrupt the interrupted thread's t0/x5), not an interrupt-timing issue;
// with that fixed the chain settles deterministically on riscv64 too.
tagged_test!(init_package_starts_system_manager, [Boot, Service, VolumeLayout]);
fn init_package_starts_system_manager() {
	// The boot chain, end to end: SystemManager starts from the init package, spawns
	// ServiceManager and delegates the package and the ramdisk to it, and
	// ServiceManager brings up the core services in dependency order - LogService
	// first, then DeviceService and ConfigService (they depend only on LogService, so
	// they come up right after), then ResourceManager (which also depends only on
	// LogService, so it comes up among them, and in turn launches the component it
	// governs and caps its Domain before reporting in), then DeviceManager,
	// StorageService (handed the disk block channel DeviceManager routes up), the
	// media StorageService (handed the second disk's block channel, mounting it as the
	// writable FAT / exFAT vol://media), the iso StorageService (handed the third disk's
	// block channel, mounting it as the read-only ISO9660 vol://iso), and the udf
	// StorageService (handed the fourth disk's block channel, mounting it as the read-only
	// UDF vol://udf - so four StorageService reports arrive),
	// NetworkService (handed the net driver's frame channel the same way), then
	// ProcessService (which depends on StorageService, since it loads the on-disk program
	// binaries from their manifest-declared system-volume paths, so it comes up once storage is running),
	// PermissionManager (which needs storage and network to grant onward, so it comes up
	// once they are running, and in turn launches its sandboxed component before reporting
	// in), and finally - after every component it observes - SystemGraphService, then the
	// shell. Every report is relayed up, so the kernel observes the
	// services come up in dependency order, then the watchdog canary brought up,
	// restarted after a commanded crash and recovered after a missed heartbeat
	// (ServiceManager exercises the restart policy and watchdog), then the
	// transparent-restart drill: ConfigService - a REAL service other components hold
	// channels to - is killed and restarted per policy, and the canary (a standing
	// client with a CONFIG grant) re-resolves it through the broker and round-trips a
	// typed request against the restarted instance, proving a client survives a
	// service restart (and that an un-granted resolve is denied). Then DeviceManager
	// stopped (ServiceManager exercises the stop path on that service - after the
	// restart drill, whose replacement is launched from the system volume that
	// DeviceManager's virtio-blk backs), the graceful-shutdown ordering check
	// (ServiceManager confirms the reverse-dependency teardown order the `poweroff`
	// path uses is valid against the live manifest), followed by the two managers.
	let (kernel_ep, _koid) = spawn_system_manager().expect("SystemManager should start from the init package");
	sched::run_until_idle();
	let online_reports: [&[u8]; 21] = [
		b"LogService: online",
		b"DeviceManager: online",
		b"StorageService: online",
		b"StorageService: online",
		b"StorageService: online",
		b"StorageService: online",
		b"StorageService: online",
		b"ProcessService: online",
		b"ConfigService: online",
		b"AudioService: online",
		b"InputService: online",
		b"ResourceManager: online",
		b"SessionService: online",
		b"NetworkService: online",
		b"DeviceService: online",
		b"TimeService: online",
		b"DisplayService: online",
		b"PermissionManager: online",
		b"ConsoleService: online",
		b"SystemGraphService: online",
		b"Shell: online",
	];
	let lifecycle_reports: [&[u8]; 10] = [
		b"WatchdogProbe: online",
		b"WatchdogProbe: restarted",
		b"WatchdogProbe: recovered",
		b"ConfigService: restarted",
		b"WatchdogProbe: config client survived",
		b"PermissionManager: config client reconnected",
		b"DeviceManager: stopped",
		b"ServiceManager: shutdown order ok",
		b"ServiceManager: online",
		b"SystemManager: online",
	];
	let mut actual_online_reports = alloc::vec::Vec::new();
	let mut actual_lifecycle_reports = alloc::vec::Vec::new();
	let give_up = arch::apic::ticks() + 500;
	while arch::apic::ticks() < give_up {
		sched::run_until_idle();
		while let Ok(message) = kernel_ep.recv() {
			if online_reports.iter().any(|expected| message.bytes.as_slice() == *expected) {
				actual_online_reports.push(message.bytes);
			} else {
				actual_lifecycle_reports.push(message.bytes);
			}
		}
		if actual_online_reports.len() >= online_reports.len() && actual_lifecycle_reports.len() >= lifecycle_reports.len() {
			break;
		}
		arch::idle_halt();
	}
	let missing_reports = online_reports.iter().filter(|expected| !actual_online_reports.iter().any(|actual| actual.as_slice() == **expected)).collect::<alloc::vec::Vec<_>>();
	assert_eq!(actual_online_reports.len(), online_reports.len(), "every manifest service must report online; missing={missing_reports:?}");
	actual_online_reports.sort();
	let mut expected_online_reports = online_reports.iter().map(|report| report.to_vec()).collect::<alloc::vec::Vec<_>>();
	expected_online_reports.sort();
	assert_eq!(actual_online_reports, expected_online_reports, "every manifest service reports online; independent ready services may use any deterministic tie order");
	assert_eq!(actual_lifecycle_reports.len(), lifecycle_reports.len(), "every lifecycle report must arrive");
	let expected_lifecycle_reports = lifecycle_reports.iter().map(|report| report.to_vec()).collect::<alloc::vec::Vec<_>>();
	assert_eq!(actual_lifecycle_reports, expected_lifecycle_reports, "lifecycle drill reports must preserve their causal order");
}

tagged_test!(system_volume_formats_to_the_disks_capacity, [Service, Storage, Filesystem, Slow]);
fn system_volume_formats_to_the_disks_capacity() {
	use alloc::collections::BTreeMap;
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// A fresh system volume spans the whole disk - StorageService asks the block
	// device for its capacity (the block protocol's op 2) and derives the pool from
	// it, instead of formatting a fixed 32 MB. Here we stand in for the block driver
	// with a sparse in-memory disk (a sector map; unwritten sectors read back as
	// zeros) reporting a 64 MB capacity: the mount probe finds no superblock and the
	// seed probe no archive, so the service formats fresh - and the superblock it
	// lays down must record a pool spanning everything past the factory-archive
	// region, not the old fixed constant.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	const SECTOR: usize = 512;
	let expected_pool: u64 = (CAPACITY - FACTORY_START_SECTOR * SECTOR as u64) / 4096;

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");

	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	// serve the raw block protocol over the sparse disk until the service reports in.
	let mut disk: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service should come up on the fresh disk");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service should format the disk and report in");
	// the freshly laid superblock (filesystem block 0 = the first sector past the
	// factory-archive region) must record the capacity-derived pool. num_blocks sits
	// at bytes 16..24 of the superblock - its stable on-disk ABI.
	let sb = disk.get(&FACTORY_START_SECTOR).expect("the format should write superblock slot 0");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the pool should span everything past the archive region, derived from the reported capacity");

	// The typed volume health/policy ops over the serve channel. Send a generated
	// request ([op u16][corr u32][args]) and pump block traffic until the reply lands.
	let mut request = |body: &[u8]| -> alloc::vec::Vec<u8> {
		_serve_client.send(Message::new(body.to_vec(), alloc::vec::Vec::new(), 0)).expect("the typed request should send");
		for _ in 0..100_000 {
			sched::run_until_idle();
			pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
			if let Ok(reply) = _serve_client.recv() {
				return reply.bytes;
			}
		}
		panic!("no typed reply arrived");
	};

	// status (op 12): the label is "system", the pool matches the derived size,
	// compression starts OFF, and the mount is read-write.
	let mut st = alloc::vec::Vec::new();
	st.extend_from_slice(&12u16.to_le_bytes());
	st.extend_from_slice(&1u32.to_le_bytes());
	let reply = request(&st);
	assert_eq!(reply[4], 1, "status should succeed");
	let label_len = u16::from_le_bytes([reply[5], reply[6]]) as usize;
	assert_eq!(&reply[7..7 + label_len], b"system", "the volume should carry its label");
	let total = u64::from_le_bytes(reply[7 + label_len..15 + label_len].try_into().unwrap());
	assert_eq!(total, expected_pool * 4096, "status reports the pool in bytes");
	let compression = reply[23 + label_len];
	let read_only = reply[24 + label_len];
	assert_eq!(compression, 0, "compression starts off by default");
	assert_eq!(read_only, 0, "the fresh volume mounts read-write");

	// set-compression on (op 13) flips the live volume; status reflects it.
	let mut sc = alloc::vec::Vec::new();
	sc.extend_from_slice(&13u16.to_le_bytes());
	sc.extend_from_slice(&2u32.to_le_bytes());
	sc.push(1);
	let reply = request(&sc);
	assert_eq!(reply[4], 1, "set-compression should succeed");
	let reply = request(&st);
	assert_eq!(reply[23 + label_len], 1, "compression should now be on");

	// fsck (op 14): a fresh volume verifies clean, with no damaged files named.
	let mut fs = alloc::vec::Vec::new();
	fs.extend_from_slice(&14u16.to_le_bytes());
	fs.extend_from_slice(&3u32.to_le_bytes());
	let reply = request(&fs);
	assert_eq!(reply[4], 1, "fsck should succeed");
	let failures = u32::from_le_bytes(reply[5..9].try_into().unwrap());
	let damaged = u16::from_le_bytes([reply[9], reply[10]]);
	assert_eq!((failures, damaged), (0, 0), "a fresh volume is clean");
}

tagged_test!(storage_harness_mounts_seeded_fat16, [Storage, Filesystem]);
fn storage_harness_mounts_seeded_fat16() {
	let (_, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let image = fat16_image(&[(*b"HELLO   TXT", b"hello")], false);
	let mut storage = StorageHarness::start(storage_elf, b"FATBLOCK", &image, image.len() as u64);
	assert_eq!(storage.open(b"vol://media/HELLO.TXT", 0xfa16), Some(b"hello".to_vec()));
}

tagged_test!(system_volume_lands_in_a_gpt_partition, [Service, Storage, Filesystem, Slow]);
fn system_volume_lands_in_a_gpt_partition() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// A disk partitioned by another system: a GPT whose entry array names a LiberFS
	// partition (the type GUID 4C424653-0001-4000-8000-4C6962657246) starting at LBA
	// 8192 - NOT the fixed factory layout's FACTORY_START_SECTOR. StorageService must
	// find the partition, format the volume INSIDE it, and size the pool to it.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	const PART_FIRST: u64 = 8192;
	const PART_BLOCKS: u64 = 4096; // 16 MB
	const PART_LAST: u64 = PART_FIRST + PART_BLOCKS * 8 - 1;

	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	// the GPT header at LBA 1: signature, entry-array LBA, entry count and size.
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"EFI PART");
	header[72..80].copy_from_slice(&2u64.to_le_bytes());
	header[80..84].copy_from_slice(&128u32.to_le_bytes());
	header[84..88].copy_from_slice(&128u32.to_le_bytes());
	disk.insert(1, header);
	// entry 0 at LBA 2: the LiberFS type GUID (on-disk byte order) and the span.
	let mut entries = alloc::vec![0u8; 512];
	entries[0..16].copy_from_slice(&[0x53, 0x46, 0x42, 0x4C, 0x01, 0x00, 0x00, 0x40, 0x80, 0x00, 0x4C, 0x69, 0x62, 0x65, 0x72, 0x46]);
	entries[32..40].copy_from_slice(&PART_FIRST.to_le_bytes());
	entries[40..48].copy_from_slice(&PART_LAST.to_le_bytes());
	disk.insert(2, entries);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service should come up on the GPT disk");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service should format inside the partition and report in");

	// the superblock lands at the partition's first LBA, sized to the partition -
	// and nothing was written at the fixed factory-layout offset.
	let sb = disk.get(&PART_FIRST).expect("superblock slot 0 should sit at the partition start");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the partition should carry a LiberFS superblock");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, PART_BLOCKS, "the pool should span exactly the partition");
	assert!(disk.get(&FACTORY_START_SECTOR).is_none(), "the fixed factory offset must stay untouched on a GPT disk");
}

tagged_test!(a_degenerate_gpt_entry_cannot_kill_the_storage_service, [Service, Storage, Filesystem, Slow]);
fn a_degenerate_gpt_entry_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// The disk's content must never deny storage. A GPT names a LiberFS
	// partition too small to format (8 sectors - below even the superblock slots):
	// the probe must SKIP it and fall back to the fixed factory layout instead of
	// failing the format and exiting.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let expected_pool: u64 = (CAPACITY - FACTORY_START_SECTOR * 512) / 4096;

	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"EFI PART");
	header[72..80].copy_from_slice(&2u64.to_le_bytes());
	header[80..84].copy_from_slice(&128u32.to_le_bytes());
	header[84..88].copy_from_slice(&128u32.to_le_bytes());
	disk.insert(1, header);
	// a LiberFS-typed entry spanning 8 sectors: syntactically valid, unusably small.
	let mut entries = alloc::vec![0u8; 512];
	entries[0..16].copy_from_slice(&[0x53, 0x46, 0x42, 0x4C, 0x01, 0x00, 0x00, 0x40, 0x80, 0x00, 0x4C, 0x69, 0x62, 0x65, 0x72, 0x46]);
	entries[32..40].copy_from_slice(&100u64.to_le_bytes());
	entries[40..48].copy_from_slice(&107u64.to_le_bytes());
	disk.insert(2, entries);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service must survive the degenerate entry");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service must fall back to the factory layout and report in");

	// the fallback formatted at the factory offset, sized by the disk's capacity.
	let sb = disk.get(&FACTORY_START_SECTOR).expect("the fallback should write superblock slot 0 at the factory offset");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the factory layout should carry the volume");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the pool should span the capacity-derived factory region");
}

tagged_test!(a_lying_seed_archive_cannot_kill_the_storage_service, [Service, Storage, Filesystem, Slow]);
fn a_lying_seed_archive_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// The boot-time seeding path runs exactly on a disk WITHOUT a valid
	// filesystem - the least trustworthy disk there is. A PKGARCH1 header whose
	// entry count claims a ~137 GB table used to size the read buffer straight off
	// the disk's word; the claim must be bounded by the seed region and treated as
	// "no archive", so the service formats an empty volume and reports in.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let expected_pool: u64 = (CAPACITY - FACTORY_START_SECTOR * 512) / 4096;

	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"PKGARCH1");
	header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
	disk.insert(0, header);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service must survive the lying archive");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service must treat the hostile claim as no archive and report in");

	// the volume formatted normally (empty - nothing was seeded from the "archive").
	let sb = disk.get(&FACTORY_START_SECTOR).expect("superblock slot 0 should sit at the factory offset");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the factory layout should carry the volume");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the pool should span the capacity-derived factory region");
}

tagged_test!(system_manager_recovery_escalates_after_repeated_crashes, [Process]);
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

tagged_test!(system_manager_recovery_survives_a_clean_start, [Process]);
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
