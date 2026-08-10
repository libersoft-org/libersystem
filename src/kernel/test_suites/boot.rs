use super::*;

tagged_test!(system_packages_use_canonical_executable_names, [Boot, Storage, VolumeLayout], id = "kernel.boot.system_packages_use_canonical_executable_names", covers = ["kernel", "liberfs"]);
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
tagged_test!(init_package_starts_system_manager, [Boot, Service, VolumeLayout], id = "kernel.boot.init_package_starts_system_manager", covers = ["kernel", "liberfs"]);
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
	// Seven StorageService instances: the system volume, media, iso, udf, usb, and the two
	// memory volumes (ram and tmp). They report the same line, so the count is what says the
	// whole set came up.
	let online_reports: [&[u8]; 23] = [
		b"LogService: online",
		b"DeviceManager: online",
		b"StorageService: online",
		b"StorageService: online",
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

tagged_test!(system_volume_spans_the_disks_capacity, [Service, Storage, Filesystem, Slow], id = "kernel.boot.system_volume_spans_the_disks_capacity", covers = ["kernel", "liberfs", "partition", "storage"]);
fn system_volume_spans_the_disks_capacity() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// A system volume spans the whole disk - StorageService asks the block device for its capacity
	// (the block protocol's op 2) and derives the container from it, instead of a fixed 32 MB.
	// Here we stand in for the block driver with a sparse in-memory disk (a sector map; unwritten
	// sectors read back as zeros) reporting a 64 MB capacity, carrying a volume that spans it.
	//
	// The volume is laid down HERE. This test used to get one by handing the service a blank disk
	// and letting it format, which is a shape that no longer occurs - the service mounts disks and
	// does not make them, and a volume is made by `mkpackages` on the build host. What the test is
	// FOR survives unchanged: the service must mount the whole container the disk allows and report
	// its size, rather than a constant.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	const SECTOR: usize = 512;
	let expected_pool: u64 = (CAPACITY - FALLBACK_START_SECTOR * SECTOR as u64) / 4096;

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");

	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	// serve the raw block protocol over the sparse disk until the service reports in.
	let mut disk: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> = crate::tests::whole_device_volume((expected_pool * 4096) as usize);
	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service should come up on the prepared disk");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service should mount the disk and report in");
	// The superblock records the capacity-derived pool. num_blocks sits at bytes 16..24 of the
	// superblock - its stable on-disk ABI. The FIXTURE wrote it, which is the point: the service
	// must mount a container of exactly this size and report it below, and a service that derived
	// a different one would say so through `status`.
	let sb = disk.get(&FALLBACK_START_SECTOR).expect("superblock slot 0");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the volume spans everything past the start sector, derived from the reported capacity");

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
	assert_eq!(read_only, 0, "the volume mounts read-write");

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
	assert_eq!((failures, damaged), (0, 0), "a newly written volume is clean");
}

tagged_test!(storage_harness_mounts_seeded_fat16, [Storage, Filesystem], id = "kernel.boot.storage_harness_mounts_seeded_fat16", covers = ["fat", "kernel", "liberfs", "storage"]);
fn storage_harness_mounts_seeded_fat16() {
	let (_, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let image = fat16_image(&[(*b"HELLO   TXT", b"hello")], false);
	let mut storage = StorageHarness::start(storage_elf, b"FATBLOCK", &image, image.len() as u64);
	assert_eq!(storage.open(b"vol://media/HELLO.TXT", 0xfa16), Some(b"hello".to_vec()));
}

tagged_test!(system_volume_lands_in_a_gpt_partition, [Service, Storage, Filesystem, Slow], id = "kernel.boot.system_volume_lands_in_a_gpt_partition", covers = ["kernel", "liberfs", "partition", "storage"]);
fn system_volume_lands_in_a_gpt_partition() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// A disk partitioned by another system: a GPT whose entry array names a LiberFS
	// partition (the type GUID 4C424653-0001-4000-8000-4C6962657246) starting at LBA
	// 8192 - NOT the fixed factory layout's FALLBACK_START_SECTOR. StorageService must
	// find the partition and mount the volume INSIDE it, sized to the partition.
	//
	// The volume is written into the partition HERE. The service used to format it, and the
	// property that mattered was never the formatting - it was that the service finds the
	// PARTITION and treats its span as the container, rather than reaching for LBA 0.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	const PART_FIRST: u64 = 8192;
	const PART_BLOCKS: u64 = 4096; // 16 MB
	const PART_LAST: u64 = PART_FIRST + PART_BLOCKS * 8 - 1;

	// a complete GPT - protective MBR, primary and backup headers, a checksummed entry array
	// - naming a LiberFS partition. The checksums are real: the probe verifies both of them
	// now, and a hand-assembled header without them would be testing the refusal path.
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	lay_gpt(&mut disk, CAPACITY / 512, &[(partition::LIBERFS_TYPE_GUID, PART_FIRST, PART_LAST)]);
	disk.extend(crate::tests::prepared_volume(PART_FIRST, PART_BLOCKS));

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
	assert!(online, "the service should mount the volume inside the partition and report in");

	// the superblock sits at the partition's first LBA, sized to the partition - and no volume was
	// laid at the fixed factory-layout offset.
	let sb = disk.get(&PART_FIRST).expect("superblock slot 0 should sit at the partition start");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the partition should carry a LiberFS superblock");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, PART_BLOCKS, "the pool should span exactly the partition");
	// The fallback offset is now LBA 0, which on a GPT disk carries the protective MBR, so the
	// check is that no volume was laid there rather than that nothing is there at all.
	assert!(disk.get(&FALLBACK_START_SECTOR).is_none_or(|sector| &sector[0..8] != b"LIBERFS1"), "a GPT disk must carry its volume in the partition, not at LBA 0");
}

tagged_test!(a_degenerate_gpt_entry_cannot_kill_the_storage_service, [Service, Storage, Filesystem, Slow], id = "kernel.boot.a_degenerate_gpt_entry_cannot_kill_the_storage_service", covers = ["kernel", "liberfs", "partition", "storage"]);
fn a_degenerate_gpt_entry_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// A GPT names a LiberFS partition too small to use (8 sectors - below even the
	// superblock slots). The probe skips that entry, finds no usable one, and the disk is
	// then a GPT disk with no LiberFS partition - which is somebody's partition table,
	// not a blank disk.
	//
	// This test used to demand the opposite: fall back to the factory layout and format.
	// The factory layout starts at sector ZERO, so "falling back" meant laying a
	// filesystem over the protective MBR, the GPT header, the entry array and whatever
	// else the disk carried. "The disk's content must never deny storage" is the wrong
	// rule when the content is a partition table - P02M0113 established that a mount answers
	// "I could not tell" by changing nothing, and this is the same rule one layer down.
	//
	// So the service refuses, and the whole point is what the disk looks like afterwards.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let expected_pool: u64 = (CAPACITY - FALLBACK_START_SECTOR * 512) / 4096;

	// a complete, correctly checksummed GPT whose only LiberFS-typed entry spans 8 sectors:
	// syntactically valid, unusably small. The table itself is beyond reproach, so what the
	// service refuses is the entry.
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	lay_gpt(&mut disk, CAPACITY / 512, &[(partition::LIBERFS_TYPE_GUID, 100, 107)]);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let header_before = disk.get(&1).expect("the GPT header is on the disk").clone();
	let entries_before = disk.get(&2).expect("the entry array is on the disk").clone();
	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "if it reports at all it reports online");
			online = true;
			break 'serve;
		}
	}
	assert!(!online, "a GPT disk with no usable LiberFS partition is not a disk to format");

	// nothing was written. The partition table is exactly as it was, and no superblock
	// was laid at sector zero on top of it.
	assert_eq!(disk.get(&1), Some(&header_before), "the GPT header must be untouched");
	assert_eq!(disk.get(&2), Some(&entries_before), "the partition entry array must be untouched");
	assert!(disk.get(&FALLBACK_START_SECTOR).is_none_or(|s| &s[0..8] != b"LIBERFS1"), "no filesystem may be laid over a partition table");
	let _ = expected_pool;
}

// Start StorageService on `disk` and report whether it came online, leaving `disk` as the
// service left it. The shared body of the refusal cases below, which are all the same
// experiment on different media: give the service a disk that is not blank, and look at what
// the disk says afterwards.
fn storage_on_disk(disk: &mut alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>, capacity: u64) -> bool {
	use object::channel::Channel;
	use object::rights::Rights;
	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, disk, capacity);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "if it reports at all it reports online");
			return true;
		}
	}
	false
}

tagged_test!(an_mbr_partitioned_disk_is_not_a_disk_to_format, [Service, Storage, Filesystem, Slow], id = "kernel.boot.an_mbr_partitioned_disk_is_not_a_disk_to_format", covers = ["kernel", "liberfs", "partition", "storage"]);
fn an_mbr_partitioned_disk_is_not_a_disk_to_format() {
	use alloc::collections::BTreeMap;

	// The headline case of the third audit. The probe answered "raw disk" for anything whose
	// LBA 1 lacked the GPT signature, and LBA 1 of an MBR disk holds whatever the boot loader
	// put there - so an ordinary MBR-partitioned disk was formatted from sector ZERO, over
	// the partition table and every partition it named.
	//
	// LBA 1 is never even looked at here. The evidence is at LBA 0, which the old probe did
	// not read.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut mbr = alloc::vec![0u8; 512];
	mbr[446 + 4] = 0x83; // one Linux partition
	mbr[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
	mbr[446 + 12..446 + 16].copy_from_slice(&100_000u32.to_le_bytes());
	mbr[510] = 0x55;
	mbr[511] = 0xAA;
	disk.insert(0, mbr.clone());

	assert!(!storage_on_disk(&mut disk, CAPACITY), "a disk with an MBR partition table is not a disk to format");
	assert_eq!(disk.get(&0), Some(&mbr), "the partition table must be exactly as it was");
	assert_eq!(disk.len(), 1, "nothing at all may be written to a disk that belongs to something else");
}

tagged_test!(a_foreign_superfloppy_is_not_a_disk_to_format, [Service, Storage, Filesystem, Slow], id = "kernel.boot.a_foreign_superfloppy_is_not_a_disk_to_format", covers = ["kernel", "liberfs", "partition", "storage"]);
fn a_foreign_superfloppy_is_not_a_disk_to_format() {
	use alloc::collections::BTreeMap;

	// A USB stick as most of the world ships them: FAT laid straight onto the medium at LBA
	// 0, with no partition table anywhere. Every partition-table check passes, which is
	// exactly why this one needs a filesystem recogniser behind it - the old probe found no
	// GPT, called the disk raw, and formatted over somebody's files.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut boot = alloc::vec![0u8; 512];
	boot[0] = 0xEB;
	boot[1] = 0x3C;
	boot[2] = 0x90;
	boot[3..11].copy_from_slice(b"MSDOS5.0");
	boot[54..59].copy_from_slice(b"FAT16");
	boot[510] = 0x55;
	boot[511] = 0xAA;
	disk.insert(0, boot.clone());

	assert!(!storage_on_disk(&mut disk, CAPACITY), "a disk carrying a filesystem is not a disk to format");
	assert_eq!(disk.get(&0), Some(&boot), "the boot sector must be exactly as it was");
	assert_eq!(disk.len(), 1, "nothing may be written over somebody else's filesystem");
}

tagged_test!(a_volume_on_the_whole_device_survives_a_remount, [Service, Storage, Filesystem, Slow], id = "kernel.boot.a_volume_on_the_whole_device_survives_a_remount", covers = ["kernel", "liberfs", "storage"]);
fn a_volume_on_the_whole_device_survives_a_remount() {
	use alloc::collections::BTreeMap;

	// The other side of the recogniser above, and the one that would break every second boot
	// if it were got wrong: the fixed whole-device layout puts this system's OWN superblock
	// at LBA 0 with no partition table, which is precisely the shape a superfloppy has. It
	// must be mounted, not refused as foreign and not rewritten.
	//
	// The volume is laid down HERE, the way a real one is: `mkpackages` formats a LiberFS image on
	// the build host and it is written to the medium. This test used to get its volume by handing
	// the service a blank disk and letting it format one, which stopped working the moment the
	// service stopped formatting disks - and that is the right outcome twice over, because the
	// shape it was exercising is not a shape that occurs. What it is FOR is the remount, and the
	// remount is what it still tests.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = crate::tests::whole_device_volume(CAPACITY as usize);
	let volume = disk.clone();
	assert_eq!(&volume.get(&0).expect("the fixture wrote superblock slot 0")[0..8], b"LIBERFS1");

	assert!(storage_on_disk(&mut disk, CAPACITY), "a volume written straight onto the medium mounts");
	assert_eq!(disk.get(&0), volume.get(&0), "and a mount does not rewrite the volume it found");

	// second boot, same disk.
	assert!(storage_on_disk(&mut disk, CAPACITY), "and it mounts again");
	assert_eq!(disk.get(&0), volume.get(&0), "a remount does not rewrite it either");
}

tagged_test!(garbage_where_the_superblock_should_be_cannot_kill_the_storage_service, [Service, Storage, Filesystem, Slow], id = "kernel.boot.garbage_where_the_superblock_should_be_cannot_kill_the_storage_service", covers = ["kernel", "liberfs", "storage"]);
fn garbage_where_the_superblock_should_be_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;

	// A disk whose first sector is neither a superblock nor a partition table is the least
	// trustworthy disk there is, and the service must survive it.
	//
	// The bytes are a PKGARCH1 header claiming a ~137 GB entry table, which is what this test was
	// originally written for: the seeding path sized a read buffer straight off that word. That
	// path is deleted (P02M0108), so what the case guards now is the general property - and the
	// property CHANGED with the fourth audit. It used to demand an empty volume formatted over the
	// garbage, on the reasoning that unrecognised bytes are as good as no bytes.
	//
	// They are not. There is no complete list of what a disk can hold, so "I did not recognise
	// this" is not evidence the disk is free - and the one disk in the world that most needs not to
	// be formatted is the one nobody can identify. Surviving still means not dying; it no longer
	// means writing.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"PKGARCH1");
	header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
	disk.insert(0, header.clone());

	assert!(!storage_on_disk(&mut disk, CAPACITY), "unrecognised bytes are not permission to format");
	assert_eq!(disk.get(&0), Some(&header), "the sector must be exactly as it was");
	assert_eq!(disk.len(), 1, "and nothing may be written anywhere else either");
}

tagged_test!(a_raw_foreign_filesystem_past_the_first_sector_is_not_formatted, [Service, Storage, Filesystem, Slow], id = "kernel.boot.a_raw_foreign_filesystem_past_the_first_sector_is_not_formatted", covers = ["kernel", "liberfs", "storage"]);
fn a_raw_foreign_filesystem_past_the_first_sector_is_not_formatted() {
	use alloc::collections::BTreeMap;

	// The fourth audit's case, end to end. ext4 leaves the first 1024 bytes of the device alone and
	// puts its superblock there - LBA 2, one sector past where the probe used to stop looking - so
	// a whole-device ext4 showed nothing at LBA 0, nothing at LBA 1, answered "blank", and had a
	// LiberFS laid over it.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut sb = alloc::vec![0u8; 512];
	sb[56] = 0x53;
	sb[57] = 0xEF;
	sb[4..8].copy_from_slice(&65_536u32.to_le_bytes());
	disk.insert(2, sb.clone());

	assert!(!storage_on_disk(&mut disk, CAPACITY), "a filesystem that begins past the first sector is still a filesystem");
	assert_eq!(disk.get(&2), Some(&sb), "its superblock must be exactly as it was");
	assert_eq!(disk.len(), 1, "and nothing may be written over the rest of it");
}

tagged_test!(system_manager_recovery_escalates_after_repeated_crashes, [Process], id = "kernel.boot.system_manager_recovery_escalates_after_repeated_crashes", covers = ["kernel", "liberfs"]);
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

tagged_test!(system_manager_recovery_survives_a_clean_start, [Process], id = "kernel.boot.system_manager_recovery_survives_a_clean_start", covers = ["kernel", "liberfs"]);
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
