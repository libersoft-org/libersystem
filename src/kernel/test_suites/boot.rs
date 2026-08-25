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
		// EVERY PROGRAM DIRECTORY HOLDS PROGRAMS - and, under `bin/`, one thing more.
		//
		// The rule was "anything under these three prefixes is an executable", which was true while
		// `bin/` held only programs. It now also holds APP ASSET BUNDLES: `bin/<program>/...` is the
		// data that program ships beside itself, and it is what a `app-assets` grant is scoped to.
		// LiberCommander's syntax descriptors are the first.
		//
		// So the distinction is DEPTH, and it is checked rather than assumed: a path directly under
		// `bin/` is a program and must be named like one, while a deeper path is an asset - and the
		// program that owns it must actually be staged, or the bundle is a staging mistake wearing
		// the name of something that does not exist.
		let asset: Option<&str> = name.strip_prefix("bin/").and_then(|rest| rest.split_once('/')).map(|(owner, _)| owner);
		if let Some(owner) = asset {
			let mut program = alloc::string::String::from("bin/");
			program.push_str(owner);
			program.push_str(abi::EXECUTABLE_SUFFIX);
			assert!(volume.lookup(program.as_bytes()).is_some(), "the asset bundle {name} belongs to a program the volume does not stage");
			continue;
		}
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
	let (kernel_ep, _manager) = spawn_system_manager().expect("SystemManager should start from the init package");
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
	let lifecycle_reports: [&[u8]; 11] = [
		b"WatchdogProbe: online",
		b"WatchdogProbe: restarted",
		b"WatchdogProbe: recovered",
		b"ConfigService: restarted",
		// THE REPLACEMENT IS A DIFFERENT INSTANCE, and this report exists only when it is. The
		// epoch is the process koid, which the kernel hands out from a counter it never reuses, so
		// two instances sharing one would mean the restart produced no new process - and anything
		// still holding an endpoint from before could not be told from a live client.
		b"ConfigService: new epoch",
		b"WatchdogProbe: config client survived",
		b"PermissionManager: config client reconnected",
		b"DeviceManager: stopped",
		b"ServiceManager: shutdown order ok",
		b"ServiceManager: online",
		b"SystemManager: online",
	];
	let mut actual_online_reports = alloc::vec::Vec::new();
	let mut actual_lifecycle_reports = alloc::vec::Vec::new();
	// The budget is a HANG BOUND, not a schedule. It was 500 ticks, which is wall-clock time on
	// every target - QEMU drives the guest timer from the host clock - so an emulated guest under
	// load gets far less done inside it than the machine the number was tuned on. That is exactly
	// what it looked like: x86_64 (KVM) reports all 23 services, while aarch64 reported 18 in one
	// run and 7 in another, varying with the host's load rather than with anything in the system.
	//
	// A larger bound costs nothing when the chain is healthy, because the loop breaks as soon as
	// every report has arrived. It only changes what happens when something is genuinely stuck, and
	// there the suite's own no-progress watchdog is the backstop.
	let give_up = arch::apic::ticks() + 4000;
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

tagged_test!(a_control_plane_that_lost_its_owner_is_not_reported_healthy, [Boot, Process], id = "kernel.boot.a_control_plane_that_lost_its_owner_is_not_reported_healthy", covers = ["kernel"]);
fn a_control_plane_that_lost_its_owner_is_not_reported_healthy() {
	// THE ROW OF THE FAULT MATRIX THIS MILESTONE CREATED FOR ITSELF.
	//
	// SystemManager used to relay the boot reports and exit, so after boot there was nothing to
	// watch and nothing to lose. It stays resident now and owns the control-plane Domain - which
	// means its death leaves every service below it running under no supervisor, and the kernel's
	// recovery ladder stopped watching the moment the system came up.
	//
	// Detector: the kernel, from the idle hook, on the process itself.
	// Owner:    the kernel - it is the only thing left above a dead SystemManager.
	// Outcome:  reboot. NOT a replacement: bringing up a second manager beside a branch full of
	//           processes the first one owned is two managers over one orphan, which is worse than
	//           starting again.
	//
	// The decision is tested rather than the reboot, because a test that rebooted the guest would
	// destroy the evidence of whether it was right to.
	use object::process::Process;

	let (_parent, child) = object::channel::Channel::create();
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let probe_elf = program_elf(&package, _volume, b"role_probe").expect("role_probe");
	let manager: alloc::sync::Arc<Process> = spawn_dynamic_test_process(sched::root_domain(), probe_elf, child);

	// ALIVE AND UP is the ordinary state and must not trip anything. Asserted while it is alive,
	// which is the only moment it can be.
	assert!(!crate::control_plane_lost(true, Some(&manager)), "a living manager is not a lost control plane");

	// END IT. The probe was given no case selector, so it exits without doing anything - which is
	// deliberately a CLEAN exit rather than a fault: the crash channel would never report this one,
	// and it is exactly as gone.
	manager.terminate();
	for _ in 0..100_000 {
		sched::run_until_idle();
		if manager.is_terminated() {
			break;
		}
	}
	assert!(manager.is_terminated(), "the stand-in manager ended");

	// THE TWO CASES THAT ONLY DIFFER BY THE FLAG, both asserted on the SAME ended process. Asserting
	// the pre-online case while the process was still alive proved nothing - a rule that ignored
	// the flag entirely passed it - and that is exactly the mistake this pair exists to catch.
	assert!(!crate::control_plane_lost(false, Some(&manager)), "a manager that ended BEFORE the system is up belongs to the recovery ladder, which restarts it, and is not a lost control plane");
	assert!(crate::control_plane_lost(true, Some(&manager)), "the same ended manager AFTER the system is up is a control plane with no owner");
	// And with nothing to watch at all, there is nothing to declare lost.
	assert!(!crate::control_plane_lost(true, None), "no manager recorded is not the same as one that ended");
}

tagged_test!(the_root_domain_stays_with_the_manager_that_owns_it, [Boot, Service, Process], id = "kernel.boot.the_root_domain_stays_with_the_manager_that_owns_it", covers = ["kernel"]);
fn the_root_domain_stays_with_the_manager_that_owns_it() {
	// WHAT M11 IS FOR, ASSERTED FROM OUTSIDE THE PROCESSES THAT USED TO HOLD IT.
	//
	// The root-Domain handle carries `MANAGE`, and the kernel's own comment beside
	// `sys_system_power` says what that means: whoever holds it can already `sys_domain_kill` the
	// whole system, and can rewrite the root Domain's resource limits. It used to reach the service
	// supervisor, the device manager and one instance each of `virtio_input` and `xhci` - four
	// holders, two of them keyboard drivers - so that Ctrl+Alt+Del would work.
	//
	// Now exactly one process holds it, and everything else asks. There is no way to ask the kernel
	// "who holds a capability to this object", and there should not be: a global capability
	// topology is an attack map. What CAN be asked is whether a Domain handle reaches the far side
	// at all, and that is what the bootstrap plan says: no managed service declares a role of kind
	// `power` any more, and the manifest gate refuses one that does.
	//
	// So what this test measures is the behaviour that narrowing had to preserve: the machine can
	// still be stopped, through a request rather than through the authority itself.
	use object::channel::Channel;
	let (kernel_ep, _user_ep) = Channel::create();
	core::mem::drop(kernel_ep);

	// AND THE BRANCH IS A BRANCH. Every managed service lives in one child Domain of the root, and
	// nothing inside it holds a Domain handle: `SYS_PROCESS_CREATE` puts a new process in the
	// caller's own Domain when given handle 0, which is what ServiceManager and ProcessService
	// already pass, so the branch forms itself from the one process that was placed deliberately.
	//
	// Measured by counting: the root Domain holds SystemManager and the child; the child holds
	// everything else. A ServiceManager still spawning into root would show up here as a root with
	// twenty-odd processes in it.
	let root_processes = sched::root_domain().live_processes().len();
	assert!(root_processes <= 2, "the root Domain holds the first process and nothing else grew there: {root_processes}");

	// THE SHAPE OF THE REMAINING GRANT. A SystemPower client is a channel, and a channel cannot
	// stop the machine by itself: `sys_system_power` demands a Domain handle carrying MANAGE and
	// compares it against the root. Handing it anything else is refused, which is what makes the
	// narrow door narrow.
	let (near, _far) = Channel::create();
	let refused: i64 = unsafe { arch::syscall::invoke(abi::SYS_SYSTEM_POWER, 0, abi::POWER_REBOOT, 0, 0) } as i64;
	assert!(refused < 0, "a caller with no capability at all cannot stop the machine");
	let _ = near;
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
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL).expect("the StorageService should load");
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
		_serve_client.send(Message::new(body.to_vec(), alloc::vec::Vec::new())).expect("the typed request should send");
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
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL).expect("the StorageService should load");
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
	// rule when the content is a partition table - it was established that a mount answers
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
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL).expect("the StorageService should load");
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
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL).expect("the StorageService should load");
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
	// path is deleted, so what the case guards now is the general property - and the
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
	// The kernel supervises SystemManager: if it faults, the kernel starts a
	// recovery SystemManager, up to a limit, then escalates (it reboots in
	// production). Here the "SystemManager" faults on every attempt (a ring-3 page
	// fault), so supervision detects each crash via the crash-notify channel,
	// exhausts its restarts, and reports failure - the trigger for escalation.
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let mut attempts: u32 = 0;
	// A STAND-IN FOR THE SHELL, held for the length of the test. A round now ends when a console
	// channel is registered and its peer is alive, which is what "the system came up" means; here
	// every attempt faults long before that, and the stand-in is what keeps the wait from being the
	// reason rather than the crash.
	let (console_far, console_near) = object::channel::Channel::create();
	console_input::attach(console_far);
	let up = supervise(&crash_rx, 3, 8, "test", || {
		attempts += 1;
		let (reports, _peer) = object::channel::Channel::create();
		Some((reports, sched::spawn(user_fault_thread_body, 0).process().clone()))
	});
	drop(console_near);
	fault::clear_crash_notify();
	assert!(!up, "a SystemManager that faults on every attempt must exhaust recovery and escalate");
	// EVERY ATTEMPT, and the count says so. The ladder stops early once a control-plane branch
	// exists, which is right at boot and would be silent here: a test that only asserted `!up`
	// would pass just as well after one attempt, and could not tell the two reasons apart.
	assert_eq!(attempts, 4, "the original attempt plus three restarts must all be made");
}

tagged_test!(system_manager_recovery_survives_a_clean_start, [Process], id = "kernel.boot.system_manager_recovery_survives_a_clean_start", covers = ["kernel", "liberfs"]);
fn system_manager_recovery_survives_a_clean_start() {
	// A SystemManager that comes up and STAYS UP survives on the first attempt, so supervision
	// returns "up" without starting a recovery SystemManager.
	//
	// STAYING UP IS THE WHOLE OF IT, and this test used to prove the opposite. Its stand-in body
	// returned immediately - a process that had already ended - and the assertion was that
	// supervision called that a success. It did, because supervision looked only at the crash
	// channel, and a clean exit sends nothing to it. Since M11 the manager is resident and owns the
	// control-plane branch, so an ended one is precisely the failure, and both cases are asserted
	// here against the same rule.
	use object::KernelObject;
	extern "C" fn resident_body(_arg: u64) {
		// BLOCKED ON ITS OWN PROCESS KOID, which is both halves of what this stand-in needs. It is
		// not runnable, so the round still reaches idle with it alive; and `terminate` wakes
		// exactly that koid, so the test can take it away again rather than leaving a process
		// parked in the root Domain for whatever runs next - which a later test counts.
		let Some(thread) = sched::current_thread() else { return };
		let process = thread.process().clone();
		let koid = process.header().koid();
		while !process.is_terminated() {
			sched::block_on(koid, sched::NO_DEADLINE);
		}
	}
	extern "C" fn departed_body(_arg: u64) {}
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let mut resident: Option<alloc::sync::Arc<object::process::Process>> = None;
	let (console_far, console_near) = object::channel::Channel::create();
	console_input::attach(console_far);
	let up = supervise(&crash_rx, 3, 8, "test", || {
		let process = sched::spawn(resident_body, 0).process().clone();
		resident = Some(process.clone());
		let (reports, _peer) = object::channel::Channel::create();
		Some((reports, process))
	});
	assert!(up, "a SystemManager that is still running should survive without recovery");
	// AND IT LEAVES WITH THE TEST. A stand-in that stays alive by design has to be taken away by
	// hand, or it is a process in the root Domain that outlives the assertion it was made for.
	if let Some(process) = resident.take() {
		process.terminate();
	}
	sched::run_until_idle();
	// AND THE SAME LADDER MUST REFUSE ONE THAT LEFT. Nothing faults here either; the difference is
	// only that the process is gone, which is exactly the case the crash channel cannot report.
	let mut attempts: u32 = 0;
	let departed = supervise(&crash_rx, 3, 8, "test", || {
		attempts += 1;
		let (reports, _peer) = object::channel::Channel::create();
		Some((reports, sched::spawn(departed_body, 0).process().clone()))
	});
	drop(console_near);
	fault::clear_crash_notify();
	assert!(!departed, "a SystemManager that ended cleanly is as gone as one that faulted, and must not be reported up");
	assert_eq!(attempts, 4, "an ending is a failed attempt, so the ladder runs out rather than stopping at the first");
}
