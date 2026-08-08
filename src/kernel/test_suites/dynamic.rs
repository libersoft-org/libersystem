use super::*;

tagged_test!(an_et_exec_may_not_name_the_kernel_half, [Dynamic, Memory, Process, Boot]);
fn an_et_exec_may_not_name_the_kernel_half() {
	// The first link of a complete escalation chain on x86_64, and the only one that was
	// a design decision rather than an oversight: segments were checked against a window
	// that is `None` for ET_EXEC, so an executable could name any address at all.
	//
	// Mapped into the higher half with the USER bit, such a page runs ring-3 code at a
	// negative address - and the syscall entry stub used to read a negative return
	// address as a kernel self-call, which skips the stack switch and clears `from_user`,
	// after which every `user_buf_ok` in the kernel accepts any pointer. The stub no
	// longer decides that way; this test is about the other two defences, either of which
	// refuses the premise on its own.
	use crate::elf::ElfError;
	use crate::memlayout::USER_VA_END;
	use crate::object::address_space::AddressSpace;

	fn put16(bytes: &mut [u8], offset: usize, value: u16) {
		bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	}
	fn put32(bytes: &mut [u8], offset: usize, value: u32) {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}
	fn put64(bytes: &mut [u8], offset: usize, value: u64) {
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}

	// A minimal ET_EXEC with one executable PT_LOAD at `address`.
	fn exec_image(address: u64) -> Vec<u8> {
		const CODE_OFFSET: usize = 0x200;
		let mut bytes = alloc::vec![0u8; CODE_OFFSET + 0x40];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 2); // ET_EXEC
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, address); // entry
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 1);
		let base = 64;
		put32(&mut bytes, base, 1); // PT_LOAD
		put32(&mut bytes, base + 4, 5); // R+X
		put64(&mut bytes, base + 8, CODE_OFFSET as u64);
		put64(&mut bytes, base + 16, address);
		put64(&mut bytes, base + 24, 0);
		put64(&mut bytes, base + 32, 1);
		put64(&mut bytes, base + 40, 1);
		put64(&mut bytes, base + 48, 1);
		bytes[CODE_OFFSET] = 0xc3;
		bytes
	}

	let space = AddressSpace::create().expect("address space");
	let mut frames = Vec::new();
	let mut shared = Vec::new();

	// a legitimate low address still loads, so the bound is not simply refusing.
	assert!(crate::elf::load_into(&exec_image(0x40_0000), &space, &mut frames, &mut shared).is_ok(), "an ordinary ET_EXEC must still load");

	// the kernel half, one page below the ceiling, and the very top - all refused.
	for address in [USER_VA_END, USER_VA_END + 0x1000, 0xffff_8000_0000_0000, u64::MAX & !0xfff] {
		let mut f = Vec::new();
		let mut sh = Vec::new();
		assert_eq!(crate::elf::load_into(&exec_image(address), &space, &mut f, &mut sh).err(), Some(ElfError::BadImage), "an ET_EXEC naming {address:#x} may not load");
		assert!(f.is_empty(), "and it may not have left frames mapped behind it");
	}

	// and the mapper refuses a USER mapping there even asked directly, which is the
	// defence that does not depend on the image being an ELF at all.
	let frame = crate::mem::frame::allocate().expect("a frame");
	assert!(space.try_map(USER_VA_END, frame, crate::arch::paging::PRESENT | crate::arch::paging::USER).is_err(), "no USER mapping outside the user half");
	assert!(space.try_map(USER_VA_END - 0x1000, frame, crate::arch::paging::PRESENT | crate::arch::paging::USER).is_ok(), "the last user page is still mappable");
	unsafe { crate::mem::frame::deallocate(frame) };
}

tagged_test!(elf_dyn_applies_relative_relocations_and_rejects_symbols, [Dynamic, DynamicReject, Memory, Process]);
fn elf_dyn_applies_relative_relocations_and_rejects_symbols() {
	use crate::elf::ElfError;
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	fn put16(bytes: &mut [u8], offset: usize, value: u16) {
		bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	}

	fn put32(bytes: &mut [u8], offset: usize, value: u32) {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}

	fn put64(bytes: &mut [u8], offset: usize, value: u64) {
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}

	fn program_header(bytes: &mut [u8], index: usize, kind: u32, flags: u32, offset: u64, address: u64, file_size: u64, memory_size: u64) {
		let base = 64 + index * 56;
		put32(bytes, base, kind);
		put32(bytes, base + 4, flags);
		put64(bytes, base + 8, offset);
		put64(bytes, base + 16, address);
		put64(bytes, base + 24, 0);
		put64(bytes, base + 32, file_size);
		put64(bytes, base + 40, memory_size);
		put64(bytes, base + 48, 1);
	}

	fn image(symbol: u32) -> Vec<u8> {
		const CODE_OFFSET: usize = 0x200;
		const DATA_OFFSET: usize = 0x300;
		const DATA_ADDRESS: u64 = 0x2000;
		const RELA_OFFSET: usize = 0x10;
		const DYNAMIC_OFFSET: usize = 0x30;
		const DATA_LEN: usize = 0x80;
		let mut bytes = alloc::vec![0u8; DATA_OFFSET + DATA_LEN];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 3);
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, 0);
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 3);
		program_header(&mut bytes, 0, 1, 5, CODE_OFFSET as u64, 0, 1, 1);
		program_header(&mut bytes, 1, 1, 6, DATA_OFFSET as u64, DATA_ADDRESS, DATA_LEN as u64, DATA_LEN as u64);
		program_header(&mut bytes, 2, 2, 6, (DATA_OFFSET + DYNAMIC_OFFSET) as u64, DATA_ADDRESS + DYNAMIC_OFFSET as u64, 5 * 16, 5 * 16);
		bytes[CODE_OFFSET] = 0xc3;
		let rela = DATA_OFFSET + RELA_OFFSET;
		put64(&mut bytes, rela, DATA_ADDRESS);
		put64(&mut bytes, rela + 8, (symbol as u64) << 32 | relative_relocation_type() as u64);
		put64(&mut bytes, rela + 16, 0x1234);
		let dynamic = DATA_OFFSET + DYNAMIC_OFFSET;
		for (index, (tag, value)) in [(7u64, DATA_ADDRESS + RELA_OFFSET as u64), (8, 24), (9, 24), (0x6fff_fff9, 1), (0, 0)].into_iter().enumerate() {
			put64(&mut bytes, dynamic + index * 16, tag);
			put64(&mut bytes, dynamic + index * 16 + 8, value);
		}
		bytes
	}

	fn symbol_image(provider: bool, code: u8) -> Vec<u8> {
		const CODE_OFFSET: usize = 0x200;
		const DATA_OFFSET: usize = 0x300;
		const DATA_ADDRESS: u64 = 0x2000;
		const SYMBOL_OFFSET: usize = 0x20;
		const HASH_OFFSET: usize = 0x50;
		const RELA_OFFSET: usize = 0x70;
		const DYNAMIC_OFFSET: usize = 0x90;
		const TARGET_OFFSET: usize = 0x110;
		const DATA_LEN: usize = 0x120;
		let strings = b"\0shared_value\0";
		let mut bytes = alloc::vec![0u8; DATA_OFFSET + DATA_LEN];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 3);
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, 0);
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 3);
		program_header(&mut bytes, 0, 1, 5, CODE_OFFSET as u64, 0, 1, 1);
		program_header(&mut bytes, 1, 1, 6, DATA_OFFSET as u64, DATA_ADDRESS, DATA_LEN as u64, DATA_LEN as u64);
		let dynamic_entries = if provider { 6 } else { 9 };
		program_header(&mut bytes, 2, 2, 6, (DATA_OFFSET + DYNAMIC_OFFSET) as u64, DATA_ADDRESS + DYNAMIC_OFFSET as u64, dynamic_entries * 16, dynamic_entries * 16);
		bytes[CODE_OFFSET] = code;
		bytes[DATA_OFFSET..DATA_OFFSET + strings.len()].copy_from_slice(strings);
		let symbol = DATA_OFFSET + SYMBOL_OFFSET + 24;
		put32(&mut bytes, symbol, 1);
		bytes[symbol + 4] = 0x12;
		put16(&mut bytes, symbol + 6, if provider { 1 } else { 0 });
		put64(&mut bytes, symbol + 8, 0);
		let hash = DATA_OFFSET + HASH_OFFSET;
		for (index, word) in [1u32, 2, 1, 0, 0].into_iter().enumerate() {
			put32(&mut bytes, hash + index * 4, word);
		}
		if !provider {
			let rela = DATA_OFFSET + RELA_OFFSET;
			put64(&mut bytes, rela, DATA_ADDRESS + TARGET_OFFSET as u64);
			put64(&mut bytes, rela + 8, 1u64 << 32 | import_relocation_type() as u64);
			put64(&mut bytes, rela + 16, 5);
		}
		let mut tags = alloc::vec![(5u64, DATA_ADDRESS), (10, strings.len() as u64), (6, DATA_ADDRESS + SYMBOL_OFFSET as u64), (11, 24), (4, DATA_ADDRESS + HASH_OFFSET as u64),];
		if !provider {
			tags.extend_from_slice(&[(7, DATA_ADDRESS + RELA_OFFSET as u64), (8, 24), (9, 24)]);
		}
		tags.push((0, 0));
		let dynamic = DATA_OFFSET + DYNAMIC_OFFSET;
		for (index, (tag, value)) in tags.into_iter().enumerate() {
			put64(&mut bytes, dynamic + index * 16, tag);
			put64(&mut bytes, dynamic + index * 16 + 8, value);
		}
		bytes
	}

	let address_space = AddressSpace::create().expect("ET_DYN address space");
	let mut frames = Vec::new();
	let mut shared = Vec::new();
	let entry = crate::elf::load_into(&image(0), &address_space, &mut frames, &mut shared).expect("relative-only ET_DYN loads");
	assert_eq!(entry, 0x1000_0000);
	assert_eq!((frames.len(), shared.len()), (1, 1));
	let relocated = unsafe { ((mem::hhdm_offset() + frames[0]) as *const u64).read_unaligned() };
	assert_eq!(relocated, 0x1000_1234);
	drop(address_space);
	for frame in frames {
		unsafe { mem::frame::deallocate(frame) };
	}

	let rejected_space = AddressSpace::create().expect("rejected ET_DYN address space");
	let mut rejected_frames = Vec::new();
	let mut rejected_shared = Vec::new();
	assert_eq!(crate::elf::load_into(&image(1), &rejected_space, &mut rejected_frames, &mut rejected_shared), Err(ElfError::BadImage));
	assert!(rejected_space.unmap(0x1000_0000).is_none(), "failed ET_DYN load rolled back every PTE");
	assert!(rejected_shared.is_empty());
	drop(rejected_space);
	for frame in rejected_frames {
		unsafe { mem::frame::deallocate(frame) };
	}

	let mut oversized = image(0);
	put64(&mut oversized, 64 + 56 + 40, 0x0200_0000);
	let oversized_space = AddressSpace::create().expect("oversized module address space");
	let mut oversized_frames = Vec::new();
	let mut oversized_shared = Vec::new();
	assert_eq!(crate::elf::load_module_into(&oversized, &oversized_space, &mut oversized_frames, &mut oversized_shared, 0x2000_0000, &|_| None), Err(ElfError::BadImage));
	assert!(oversized_space.unmap(0x2000_0000).is_none(), "oversized provider cannot escape its 16 MiB slot");
	for frame in oversized_frames {
		unsafe { mem::frame::deallocate(frame) };
	}

	let process = Process::new(AddressSpace::create().expect("dynamic module process address space"), sched::root_domain());
	let provider = symbol_image(true, 0xc3);
	let consumer = symbol_image(false, 0xc3);
	let colliding_provider = symbol_image(true, 0x90);
	crate::loader::load_module_into(&process, &provider, 0x2000_0000).expect("provider module loads and registers exports");
	assert_eq!(process.resolve_dynamic_symbol("shared_value"), Some(0x2000_0000));
	crate::loader::load_module_into(&process, &consumer, 0x2100_0000).expect("consumer resolves provider symbol eagerly");
	let consumer_data = process.address_space().unmap(0x2100_2000).expect("consumer data mapping");
	let imported = unsafe { ((mem::hhdm_offset() + consumer_data + 0x110) as *const u64).read_unaligned() };
	assert_eq!(imported, 0x2000_0005);
	assert_ne!(provider, colliding_provider, "colliding providers are distinct images");
	assert!(matches!(crate::loader::load_module_into(&process, &colliding_provider, 0x2200_0000), Err(crate::loader::LoadError::BadImage)), "distinct providers with a duplicate export are rejected");
	assert!(process.address_space().unmap(0x2200_0000).is_none(), "duplicate-export provider mapping is rolled back");
	let second = Process::new(AddressSpace::create().expect("second module process address space"), sched::root_domain());
	crate::loader::load_module_into(&second, &provider, 0x2000_0000).expect("same provider loads in a second process");
	let first_text = process.address_space().unmap(0x2000_0000).expect("first provider text mapping");
	let second_text = second.address_space().unmap(0x2000_0000).expect("second provider text mapping");
	assert_eq!(first_text, second_text, "two processes map one physical immutable provider page");
}

tagged_test!(dynamic_process_service_loads_probe, [Dynamic, Service, Process, Storage]);
fn dynamic_process_service_loads_probe() {
	use object::channel::{Channel, Message};
	use object::process::Process;
	use object::rights::Rights;

	let (volume, _) = scenario_packages().expect("scenario packages");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	let dynamic_name = b"dyn_probe";
	let (report, bootstrap) = Channel::create();
	let mut launch = alloc::vec::Vec::new();
	launch.extend_from_slice(&3u16.to_le_bytes());
	launch.extend_from_slice(&2u32.to_le_bytes());
	launch.extend_from_slice(&(dynamic_name.len() as u16).to_le_bytes());
	launch.extend_from_slice(dynamic_name);
	launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &launch, bootstrap, Rights::ALL).expect("dynamic probe launch request");
	sched::run_until_idle();
	let reply = process_client.recv().expect("dynamic probe launch reply");
	assert_eq!(le_u32(&reply.bytes, 0), 2);
	assert_eq!(reply.bytes[4], 1, "the staged dynamic probe loaded with its providers");
	let process = reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic probe launch capability is a Process");
	assert_eq!(&report.recv().expect("dynamic probe report").bytes, b"dynamic link ok");
	assert!(process.private_image_pages() != 0 && process.shared_image_pages() != 0, "dynamic probe has private and shared mappings");
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(lico_provider_loads_with_lsrt, [Lico, Dynamic, Process, Storage]);
fn lico_provider_loads_with_lsrt() {
	use object::address_space::AddressSpace;
	use object::process::Process;

	let (volume, _) = scenario_packages().expect("scenario packages");
	let lsrt = volume_file(volume, b"lib/runtime/lsrt.lslib").expect("staged lsrt provider");
	let ipc_client = volume_file(volume, b"lib/ipc/ipc-client.lslib").expect("staged ipc client provider");
	let wire = volume_file(volume, b"lib/ipc/wire.lslib").expect("staged wire provider");
	let base_proto = volume_file(volume, b"lib/protocol/base-proto.lslib").expect("staged base protocol provider");
	let lico = volume_file(volume, b"lib/terminal/lico.lslib").expect("staged lico provider");
	let storage_proto = volume_file(volume, b"lib/protocol/storage-proto.lslib").expect("staged storage protocol provider");
	let volume_client = volume_file(volume, b"lib/clients/volume-client.lslib").expect("staged volume client provider");
	let process = Process::new(AddressSpace::create().expect("lico provider address space"), sched::root_domain());
	crate::loader::load_module_into(&process, &lsrt, 0x2000_0000).expect("staged lsrt loads as the first dynamic provider");
	crate::loader::load_module_into(&process, &ipc_client, 0x2100_0000).expect("staged ipc client resolves its lsrt imports");
	crate::loader::load_module_into(&process, &wire, 0x2200_0000).expect("staged wire resolves its lsrt imports");
	crate::loader::load_module_into(&process, &base_proto, 0x2300_0000).expect("staged base protocol resolves its provider imports");
	crate::loader::load_module_into(&process, &lico, 0x2400_0000).expect("staged lico resolves its lsrt imports and registers its exports");
	crate::loader::load_module_into(&process, &storage_proto, 0x2500_0000).expect("staged storage protocol resolves its provider imports");
	crate::loader::load_module_into(&process, &volume_client, 0x2600_0000).expect("staged volume client resolves its provider imports");
	// Matched on the path and item rather than the whole mangled name: the leading
	// `_RNvNtCs<hash>_` disambiguator is derived from the crate's compilation metadata
	// and differs on every target, so spelling it out pins the assertion to one
	// architecture and fails on the other two for no reason of substance.
	assert!(process.resolve_dynamic_symbol_by_suffix("4lico6detect16detect_file_type").is_some(), "lico registers its file-type detector for dynamic consumers");
}

tagged_test!(dynamic_process_service_loads_programs_from_system_bin, [LicoLoad, Service, Process, ProcessService, Storage]);
fn dynamic_process_service_loads_programs_from_system_bin() {
	use object::channel::{Channel, Message};
	use object::process::Process;
	use object::rights::Rights;

	// ProcessService loads a named program's ELF from the system volume's
	// manifest-declared path through a StorageService client, not the init package. Stand up a
	// StorageService over the factory volume archive (which stages the tools under
	// `bin/`) and a ProcessService wired to its client, then START a staged tool by name:
	// ProcessService resolves it through the manifest and loads it off the volume,
	// proving the on-disk load path the shell's `run` and ConsoleService's shell spawn now
	// take.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	let mut writable_storage = StorageHarness::start_system(storage_elf, b"BLOCK", volume, 64 * 1024 * 1024);

	// START a staged static tool: [op = 1 u16][corr u32][name: [len u16][utf8]].
	let name: &[u8] = b"ptyecho";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&1u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	process_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");
	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves.
	let online = process_boot_kernel.recv().expect("ProcessService online report");
	assert_eq!(&online.bytes[..], b"ProcessService: online", "ProcessService reports in");

	// the start reply is [corr u32 = 1][ok u8 = 1][koid u64][name]: success proves the
	// binary was found and loaded from the system volume (a missing binary would
	// reply with an error, since a wired storage client does not fall back to the package).
	let reply = process_client.recv().expect("start reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 1, "start reply echoes the correlation id");
	assert_eq!(b[4], 1, "the staged tool loaded from its manifest path");
	let koid = le_u64(b, 5);
	assert!(koid >= 1, "the started process has a koid");
	let name_len = le_u16(b, 13) as usize;
	assert_eq!(&b[15..15 + name_len], b"ptyecho.lsexe", "the reply reports the canonical artifact name");

	let explicit_ptyecho = alloc::format!("vol://system/{}", test_program_path("ptyecho").expect("ptyecho destination"));
	for (corr, path, succeeds) in [(10u32, explicit_ptyecho.as_bytes(), true), (11, &b"vol://system/bin/ptyecho"[..], false), (12, &b"vol://system/bin/wasi_host.lsexe"[..], false)] {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		process_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("explicit-path start request");
		sched::run_until_idle();
		let reply = process_client.recv().expect("explicit-path start reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		assert_eq!(reply.bytes[4] == 1, succeeds, "only a manifest-declared executable path is accepted");
		if succeeds {
			let name_len = le_u16(&reply.bytes, 13) as usize;
			assert_eq!(&reply.bytes[15..15 + name_len], b"ptyecho.lsexe", "an explicit path records only the canonical basename");
		}
	}

	// Launch the first ordinary PIE tool, hand its bootstrap a stdout channel and
	// arguments, and observe output produced through lsrt. This covers the generated
	// start object and the echo.lsexe -> lsrt.lslib provider edge after volume staging.
	let echo_name: &[u8] = b"echo";
	let (echo_stdout_kernel, echo_stdout_user) = Channel::create();
	let (echo_bootstrap_kernel, echo_bootstrap_user) = Channel::create();
	let mut echo_launch = alloc::vec::Vec::new();
	echo_launch.extend_from_slice(&3u16.to_le_bytes());
	echo_launch.extend_from_slice(&20u32.to_le_bytes());
	echo_launch.extend_from_slice(&(echo_name.len() as u16).to_le_bytes());
	echo_launch.extend_from_slice(echo_name);
	echo_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &echo_launch, echo_bootstrap_user, Rights::ALL).expect("dynamic echo launch request");
	sched::run_until_idle();
	let echo_reply = process_client.recv().expect("dynamic echo launch reply");
	assert_eq!(le_u32(&echo_reply.bytes, 0), 20);
	assert_eq!(echo_reply.bytes[4], 1, "the ordinary PIE tool loaded with lsrt");
	let echo_process = echo_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic echo launch capability is a Process");
	assert!(!echo_process.is_terminated(), "dynamic echo remains blocked on its live bootstrap after launch");
	assert!(echo_process.handle_count() >= 1, "dynamic echo owns its bootstrap handle");
	assert!(!echo_bootstrap_kernel.is_peer_closed(), "dynamic echo bootstrap peer is open before initialization");
	send_cap(&echo_bootstrap_kernel, b"STDOUT", echo_stdout_user, Rights::ALL).expect("dynamic echo stdout bootstrap");
	echo_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	echo_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"dynamic echo", b""), alloc::vec::Vec::new(), 0)).expect("dynamic echo arguments");
	assert!(!echo_bootstrap_kernel.is_peer_closed(), "dynamic echo bootstrap peer remains open after initialization is queued");
	sched::run_until_idle();
	let echo_output = echo_stdout_kernel.recv().unwrap_or_else(|error| {
		let fault = echo_process.fault_info();
		panic!("dynamic echo output: {error:?}; fault={:?} terminated={} sent={} received={}", fault.map(|info| (info.kind, info.error_code, info.address, info.instruction_pointer)), echo_process.is_terminated(), echo_process.messages_sent(), echo_process.messages_received())
	});
	assert_eq!(&echo_output.bytes, b"dynamic echo");
	assert_eq!(&echo_stdout_kernel.recv().expect("dynamic echo newline").bytes, b"\n");

	// A net tool launched the way the SHELL launches one: its NetworkService client rides the
	// launch-context message rather than arriving under a tag.
	//
	// `ip` has two launchers with two different shapes. PermissionManager grants it under `NETWORK`
	// in its startup demonstration, which is the path every existing test covers - and the reason
	// nobody noticed the other one. The shell reaches the same program through `dispatch_net`,
	// which opens a client and hands it to `exec` as the single capability attached to the context
	// message. Nothing in the shell ever sends a `NETWORK` message, so a tool that waits for one
	// waits forever: every net command typed at a prompt started a process that never answered.
	//
	// The assertion is that the tool ASKS the client it was handed. That is the first thing it
	// does with a working client and the thing it cannot do with none.
	let ip_name: &[u8] = b"ip";
	let (ip_stdout_kernel, ip_stdout_user) = Channel::create();
	let (ip_bootstrap_kernel, ip_bootstrap_user) = Channel::create();
	let (ip_net_kernel, ip_net_user) = Channel::create();
	let mut ip_launch = alloc::vec::Vec::new();
	ip_launch.extend_from_slice(&3u16.to_le_bytes());
	ip_launch.extend_from_slice(&24u32.to_le_bytes());
	ip_launch.extend_from_slice(&(ip_name.len() as u16).to_le_bytes());
	ip_launch.extend_from_slice(ip_name);
	ip_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &ip_launch, ip_bootstrap_user, Rights::ALL).expect("shell-shaped ip launch request");
	sched::run_until_idle();
	let ip_reply = process_client.recv().expect("shell-shaped ip launch reply");
	assert_eq!(le_u32(&ip_reply.bytes, 0), 24);
	send_cap(&ip_bootstrap_kernel, b"STDOUT", ip_stdout_user, Rights::ALL).expect("shell-shaped ip stdout");
	ip_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	send_cap(&ip_bootstrap_kernel, &crate::tests::launch_context(b"", b""), ip_net_user, Rights::ALL).expect("shell-shaped ip context with its client attached");
	sched::run_until_idle();
	let ip_request = ip_net_kernel.recv().expect("a shell-launched net tool queries the client attached to its launch context");
	assert_eq!(le_u16(&ip_request.bytes, 0), 1, "the query is the NetworkService info op");
	let _ = ip_stdout_kernel;

	// A program given a SEPARATE error stream writes its diagnostic there and leaves stdout alone.
	//
	// Every launch today hands over one console, and `eprint` falls back to stdout when no error
	// endpoint arrives - which is why moving a hundred and forty-three diagnostics onto it changed
	// nothing anyone could observe. That is the right default and a poor test: it proves the
	// fallback, not the endpoint. This gives `cat` two channels and asks which one the message
	// came out of.
	let cat_name: &[u8] = b"cat";
	let (cat_out_kernel, cat_out_user) = Channel::create();
	let (cat_err_kernel, cat_err_user) = Channel::create();
	let (cat_bootstrap_kernel, cat_bootstrap_user) = Channel::create();
	let mut cat_launch = alloc::vec::Vec::new();
	cat_launch.extend_from_slice(&3u16.to_le_bytes());
	cat_launch.extend_from_slice(&25u32.to_le_bytes());
	cat_launch.extend_from_slice(&(cat_name.len() as u16).to_le_bytes());
	cat_launch.extend_from_slice(cat_name);
	cat_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &cat_launch, cat_bootstrap_user, Rights::ALL).expect("split-stream cat launch request");
	sched::run_until_idle();
	assert_eq!(le_u32(&process_client.recv().expect("split-stream cat launch reply").bytes, 0), 25);
	send_cap(&cat_bootstrap_kernel, b"STDOUT", cat_out_user, Rights::ALL).expect("split-stream cat stdout");
	send_cap(&cat_bootstrap_kernel, b"STDERR", cat_err_user, Rights::ALL).expect("split-stream cat stderr");
	cat_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	// A path no volume can name, so the tool fails before it needs a volume client. The bundle is
	// still sent - empty - because the tool takes it before it resolves anything.
	cat_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"::not-a-path", b""), alloc::vec::Vec::new(), 0)).expect("split-stream cat context");
	cat_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("split-stream cat empty volume bundle");
	sched::run_until_idle();
	let diagnostic = cat_err_kernel.recv().expect("the diagnostic goes to the error stream it was given");
	assert_eq!(&diagnostic.bytes, b"cat: invalid path\n");
	assert!(cat_out_kernel.recv().is_err(), "and stdout stays clean, which is the whole point of separating them");

	// Load the generated date PIE directly as well. Its capability protocol is covered by
	// PermissionManager; this assertion isolates staging, provider-DAG loading and relocation
	// from that policy layer so a loader failure cannot collapse into an empty tool result.
	let date_name: &[u8] = b"date";
	let (date_bootstrap_kernel, date_bootstrap_user) = Channel::create();
	let mut date_launch = alloc::vec::Vec::new();
	date_launch.extend_from_slice(&3u16.to_le_bytes());
	date_launch.extend_from_slice(&21u32.to_le_bytes());
	date_launch.extend_from_slice(&(date_name.len() as u16).to_le_bytes());
	date_launch.extend_from_slice(date_name);
	date_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &date_launch, date_bootstrap_user, Rights::ALL).expect("dynamic date launch request");
	sched::run_until_idle();
	let date_reply = process_client.recv().expect("dynamic date launch reply");
	assert_eq!(le_u32(&date_reply.bytes, 0), 21);
	assert_eq!(date_reply.bytes[4], 1, "the date PIE loaded with its manifest providers");
	drop(date_bootstrap_kernel);
	sched::run_until_idle();

	for (index, tool) in [
		b"play" as &[u8],
		b"graphics_probe" as &[u8],
		b"imgview" as &[u8],
		b"imgconv" as &[u8],
		b"lico" as &[u8],
		b"licoview" as &[u8],
		b"licoedit" as &[u8],
		b"config" as &[u8],
		b"set" as &[u8],
		b"log" as &[u8],
		b"snap" as &[u8],
		b"volume" as &[u8],
		b"lsdev" as &[u8],
		b"lsvol" as &[u8],
		b"lssvc" as &[u8],
		b"lsblk" as &[u8],
		b"lsusb" as &[u8],
		b"usage" as &[u8],
		b"ps" as &[u8],
		b"run" as &[u8],
		b"perm" as &[u8],
		b"stop" as &[u8],
		b"beep" as &[u8],
		b"readln" as &[u8],
		b"ptyecho" as &[u8],
		b"script" as &[u8],
		b"ping" as &[u8],
		b"ip" as &[u8],
		b"nslookup" as &[u8],
		b"tcp" as &[u8],
		b"nc" as &[u8],
		b"arp" as &[u8],
		b"ss" as &[u8],
		b"httpd" as &[u8],
	]
	.iter()
	.enumerate()
	{
		let correlation = 40 + index as u32;
		let (tool_bootstrap_kernel, tool_bootstrap_user) = Channel::create();
		let mut tool_launch = alloc::vec::Vec::new();
		tool_launch.extend_from_slice(&3u16.to_le_bytes());
		tool_launch.extend_from_slice(&correlation.to_le_bytes());
		tool_launch.extend_from_slice(&(tool.len() as u16).to_le_bytes());
		tool_launch.extend_from_slice(tool);
		tool_launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&process_client, &tool_launch, tool_bootstrap_user, Rights::ALL).expect("service-tool batch launch request");
		sched::run_until_idle();
		let tool_reply = process_client.recv().expect("service-tool batch launch reply");
		assert_eq!(le_u32(&tool_reply.bytes, 0), correlation);
		assert_eq!(tool_reply.bytes[4], 1, "service-oriented PIE {} loaded with its manifest providers", core::str::from_utf8(tool).unwrap_or("<invalid>"));
		drop(tool_bootstrap_kernel);
		sched::run_until_idle();
	}

	let (readln_output, readln_console) = Channel::create();
	let (readln_bootstrap_kernel, readln_bootstrap_user) = Channel::create();
	let mut readln_launch = alloc::vec::Vec::new();
	readln_launch.extend_from_slice(&3u16.to_le_bytes());
	readln_launch.extend_from_slice(&59u32.to_le_bytes());
	readln_launch.extend_from_slice(&6u16.to_le_bytes());
	readln_launch.extend_from_slice(b"readln");
	readln_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &readln_launch, readln_bootstrap_user, Rights::ALL).expect("dynamic readln launch request");
	sched::run_until_idle();
	let readln_reply = process_client.recv().expect("dynamic readln launch reply");
	assert_eq!(le_u32(&readln_reply.bytes, 0), 59);
	assert_eq!(readln_reply.bytes[4], 1, "dynamic readln loaded with lsrt");
	send_cap(&readln_bootstrap_kernel, b"STDOUT", readln_console, Rights::ALL).expect("dynamic readln console bootstrap");
	readln_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	readln_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"", b""), alloc::vec::Vec::new(), 0)).expect("dynamic readln arguments");
	sched::run_until_idle();
	readln_output.send(Message::new(b"hello\n".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic readln input");
	sched::run_until_idle();
	readln_output.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("dynamic readln EOF");
	sched::run_until_idle();
	let mut readln_captured = alloc::vec::Vec::new();
	while let Ok(message) = readln_output.recv() {
		readln_captured.extend_from_slice(&message.bytes);
	}
	assert!(readln_captured.windows(b"in> hello".len()).any(|window| window == b"in> hello"), "dynamic readln echoed cooked input");

	for (tool, correlation) in [
		(b"uname" as &[u8], 31u32),
		(b"uptime" as &[u8], 32u32),
		(b"free" as &[u8], 33u32),
		(b"lscpu" as &[u8], 34u32),
		(b"dmesg" as &[u8], 35u32),
		(b"lsmem" as &[u8], 36u32),
		(b"lsirq" as &[u8], 37u32),
		(b"lspci" as &[u8], 38u32),
	] {
		let (output_kernel, output_user) = Channel::create();
		let (tool_bootstrap_kernel, tool_bootstrap_user) = Channel::create();
		let mut tool_launch = alloc::vec::Vec::new();
		tool_launch.extend_from_slice(&3u16.to_le_bytes());
		tool_launch.extend_from_slice(&correlation.to_le_bytes());
		tool_launch.extend_from_slice(&(tool.len() as u16).to_le_bytes());
		tool_launch.extend_from_slice(tool);
		tool_launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&process_client, &tool_launch, tool_bootstrap_user, Rights::ALL).expect("dynamic inventory launch request");
		sched::run_until_idle();
		let tool_reply = process_client.recv().expect("dynamic inventory launch reply");
		assert_eq!(le_u32(&tool_reply.bytes, 0), correlation);
		assert_eq!(tool_reply.bytes[4], 1, "the inventory PIE loaded with its manifest providers");
		let tool_process = tool_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic inventory capability is a Process");
		send_cap(&tool_bootstrap_kernel, b"STDOUT", output_user, Rights::ALL).expect("dynamic inventory stdout bootstrap");
		tool_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
		tool_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"", b""), alloc::vec::Vec::new(), 0)).expect("dynamic inventory arguments");
		let mut captured = alloc::vec::Vec::new();
		for _ in 0..100_000 {
			sched::run_until_idle();
			while let Ok(message) = output_kernel.recv() {
				captured.extend_from_slice(&message.bytes);
			}
			if tool_process.is_terminated() {
				break;
			}
		}
		assert!(tool_process.is_terminated(), "dynamic inventory tool completed");
		let contains = |needle: &[u8]| captured.windows(needle.len()).any(|window| window == needle);
		match tool {
			b"uname" => {
				assert!(contains(env!("PRODUCT_NAME").as_bytes()) && contains(env!("PRODUCT_VERSION").as_bytes()), "dynamic uname printed product identity");
			}
			b"uptime" => assert!(captured.starts_with(b"up ") && captured.ends_with(b"\n"), "dynamic uptime rendered time since boot"),
			b"free" => assert!(captured.starts_with(b"Mem:  total ") && contains(b"Heap: total "), "dynamic free rendered memory pools"),
			b"lscpu" => assert!(contains(b"arch: ") && contains(b"name: ") && contains(b"cpu0: lapic "), "dynamic lscpu rendered CPU inventory"),
			b"dmesg" => assert!(!captured.is_empty(), "dynamic dmesg rendered the kernel boot log or its empty-log diagnostic"),
			b"lsmem" => assert!(contains(b" usable\n"), "dynamic lsmem rendered a usable memory region"),
			b"lsirq" => assert!(contains(b"vector  type   bound  device  device-type") && contains(b"fixed"), "dynamic lsirq rendered its aligned vector table"),
			b"lspci" => assert!(contains(b"1af4:") && contains(b"(network controller)"), "dynamic lspci rendered the retained virtio bus scan"),
			_ => unreachable!(),
		}
	}

	// Exercise the mutable ordinary PIEs against one block-backed StorageService: create a
	// directory, stream a file into it, read it back, reject removal while non-empty, remove
	// the file and then the empty directory, and finally prove the file remains absent.
	let mkdir_name: &[u8] = b"mkdir";
	let (mkdir_stdout_kernel, mkdir_stdout_user) = Channel::create();
	let (mkdir_bootstrap_kernel, mkdir_bootstrap_user) = Channel::create();
	let mut mkdir_launch = alloc::vec::Vec::new();
	mkdir_launch.extend_from_slice(&3u16.to_le_bytes());
	mkdir_launch.extend_from_slice(&26u32.to_le_bytes());
	mkdir_launch.extend_from_slice(&(mkdir_name.len() as u16).to_le_bytes());
	mkdir_launch.extend_from_slice(mkdir_name);
	mkdir_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &mkdir_launch, mkdir_bootstrap_user, Rights::ALL).expect("dynamic mkdir launch request");
	sched::run_until_idle();
	let mkdir_reply = process_client.recv().expect("dynamic mkdir launch reply");
	assert_eq!(le_u32(&mkdir_reply.bytes, 0), 26);
	assert_eq!(mkdir_reply.bytes[4], 1, "the mkdir PIE loaded with its manifest providers");
	send_cap(&mkdir_bootstrap_kernel, b"STDOUT", mkdir_stdout_user, Rights::ALL).expect("dynamic mkdir stdout bootstrap");
	mkdir_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	mkdir_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("dynamic mkdir arguments");
	send_cap(&mkdir_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic mkdir system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		mkdir_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic mkdir absent volume");
	}
	mkdir_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut mkdir_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = mkdir_stdout_kernel.recv() {
			mkdir_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&mkdir_prefix.expect("dynamic mkdir confirmation prefix").bytes, b"created ");
	assert_eq!(&mkdir_stdout_kernel.recv().expect("dynamic mkdir confirmation path").bytes, b"vol://system/dynamic-dir");
	assert_eq!(&mkdir_stdout_kernel.recv().expect("dynamic mkdir confirmation newline").bytes, b"\n");

	let write_name: &[u8] = b"write";
	let (write_stdout_kernel, write_stdout_user) = Channel::create();
	let (write_bootstrap_kernel, write_bootstrap_user) = Channel::create();
	let mut write_launch = alloc::vec::Vec::new();
	write_launch.extend_from_slice(&3u16.to_le_bytes());
	write_launch.extend_from_slice(&22u32.to_le_bytes());
	write_launch.extend_from_slice(&(write_name.len() as u16).to_le_bytes());
	write_launch.extend_from_slice(write_name);
	write_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &write_launch, write_bootstrap_user, Rights::ALL).expect("dynamic write launch request");
	sched::run_until_idle();
	let write_reply = process_client.recv().expect("dynamic write launch reply");
	assert_eq!(le_u32(&write_reply.bytes, 0), 22);
	assert_eq!(write_reply.bytes[4], 1, "the write PIE loaded with its manifest providers");
	send_cap(&write_bootstrap_kernel, b"STDOUT", write_stdout_user, Rights::ALL).expect("dynamic write stdout bootstrap");
	write_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	write_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir/dynamic-write.txt dynamic write", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("dynamic write arguments");
	send_cap(&write_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic write system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		write_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic write absent volume");
	}
	write_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut write_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = write_stdout_kernel.recv() {
			write_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&write_prefix.expect("dynamic write confirmation prefix").bytes, b"wrote ");
	assert_eq!(&write_stdout_kernel.recv().expect("dynamic write confirmation path").bytes, b"vol://system/dynamic-dir/dynamic-write.txt");
	assert_eq!(&write_stdout_kernel.recv().expect("dynamic write confirmation newline").bytes, b"\n");

	let cat_name: &[u8] = b"cat";
	let (cat_stdout_kernel, cat_stdout_user) = Channel::create();
	let (cat_bootstrap_kernel, cat_bootstrap_user) = Channel::create();
	let mut cat_launch = alloc::vec::Vec::new();
	cat_launch.extend_from_slice(&3u16.to_le_bytes());
	cat_launch.extend_from_slice(&23u32.to_le_bytes());
	cat_launch.extend_from_slice(&(cat_name.len() as u16).to_le_bytes());
	cat_launch.extend_from_slice(cat_name);
	cat_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &cat_launch, cat_bootstrap_user, Rights::ALL).expect("dynamic cat launch request");
	sched::run_until_idle();
	let cat_reply = process_client.recv().expect("dynamic cat launch reply");
	assert_eq!(le_u32(&cat_reply.bytes, 0), 23);
	assert_eq!(cat_reply.bytes[4], 1, "the cat PIE loaded for write read-back");
	send_cap(&cat_bootstrap_kernel, b"STDOUT", cat_stdout_user, Rights::ALL).expect("dynamic cat stdout bootstrap");
	cat_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	cat_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir/dynamic-write.txt", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("dynamic cat arguments");
	send_cap(&cat_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic cat system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		cat_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic cat absent volume");
	}
	cat_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut cat_output = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = cat_stdout_kernel.recv() {
			cat_output = Some(message);
			break;
		}
	}
	assert_eq!(&cat_output.expect("dynamic cat read-back").bytes, b"dynamic write");
	assert_eq!(&cat_stdout_kernel.recv().expect("dynamic cat read-back newline").bytes, b"\n");

	for (tool, correlation, arguments) in [(b"ls" as &[u8], 29u32, b"vol://system/dynamic-dir" as &[u8]), (b"du" as &[u8], 30u32, b"-s vol://system/dynamic-dir" as &[u8])] {
		let (output_kernel, output_user) = Channel::create();
		let (tool_bootstrap_kernel, tool_bootstrap_user) = Channel::create();
		let mut tool_launch = alloc::vec::Vec::new();
		tool_launch.extend_from_slice(&3u16.to_le_bytes());
		tool_launch.extend_from_slice(&correlation.to_le_bytes());
		tool_launch.extend_from_slice(&(tool.len() as u16).to_le_bytes());
		tool_launch.extend_from_slice(tool);
		tool_launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&process_client, &tool_launch, tool_bootstrap_user, Rights::ALL).expect("dynamic traversal tool launch request");
		sched::run_until_idle();
		let tool_reply = process_client.recv().expect("dynamic traversal tool launch reply");
		assert_eq!(le_u32(&tool_reply.bytes, 0), correlation);
		assert_eq!(tool_reply.bytes[4], 1, "the traversal PIE loaded with its manifest providers");
		let tool_process = tool_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic traversal capability is a Process");
		send_cap(&tool_bootstrap_kernel, b"STDOUT", output_user, Rights::ALL).expect("dynamic traversal stdout bootstrap");
		tool_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
		tool_bootstrap_kernel.send(Message::new(crate::tests::launch_context(arguments, b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("dynamic traversal arguments");
		send_cap(&tool_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic traversal system volume");
		for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
			tool_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic traversal absent volume");
		}
		tool_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
		let mut captured = alloc::vec::Vec::new();
		for _ in 0..100_000 {
			writable_storage.pump();
			while let Ok(message) = output_kernel.recv() {
				captured.extend_from_slice(&message.bytes);
			}
			if tool_process.is_terminated() {
				break;
			}
		}
		assert!(tool_process.is_terminated(), "dynamic traversal tool completed");
		if tool == b"ls" {
			assert!(captured.windows(b"dynamic-write.txt".len()).any(|window| window == b"dynamic-write.txt"), "dynamic ls listed the written file");
			assert!(captured.windows(b"1 file".len()).any(|window| window == b"1 file"), "dynamic ls rendered its summary");
		} else {
			assert_eq!(&captured, b"13\tvol://system/dynamic-dir\n", "dynamic du summed the nested file exactly");
		}
	}

	let rmdir_name: &[u8] = b"rmdir";
	let (full_rmdir_stdout_kernel, full_rmdir_stdout_user) = Channel::create();
	let (full_rmdir_bootstrap_kernel, full_rmdir_bootstrap_user) = Channel::create();
	let mut full_rmdir_launch = alloc::vec::Vec::new();
	full_rmdir_launch.extend_from_slice(&3u16.to_le_bytes());
	full_rmdir_launch.extend_from_slice(&27u32.to_le_bytes());
	full_rmdir_launch.extend_from_slice(&(rmdir_name.len() as u16).to_le_bytes());
	full_rmdir_launch.extend_from_slice(rmdir_name);
	full_rmdir_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &full_rmdir_launch, full_rmdir_bootstrap_user, Rights::ALL).expect("non-empty rmdir launch request");
	sched::run_until_idle();
	let full_rmdir_reply = process_client.recv().expect("non-empty rmdir launch reply");
	assert_eq!(le_u32(&full_rmdir_reply.bytes, 0), 27);
	assert_eq!(full_rmdir_reply.bytes[4], 1, "the rmdir PIE loaded for non-empty rejection");
	send_cap(&full_rmdir_bootstrap_kernel, b"STDOUT", full_rmdir_stdout_user, Rights::ALL).expect("non-empty rmdir stdout bootstrap");
	full_rmdir_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	full_rmdir_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("non-empty rmdir arguments");
	send_cap(&full_rmdir_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("non-empty rmdir system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		full_rmdir_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("non-empty rmdir absent volume");
	}
	full_rmdir_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut full_rmdir_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = full_rmdir_stdout_kernel.recv() {
			full_rmdir_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&full_rmdir_prefix.expect("non-empty rmdir error prefix").bytes, b"rmdir: could not remove ");
	assert_eq!(&full_rmdir_stdout_kernel.recv().expect("non-empty rmdir error path").bytes, b"vol://system/dynamic-dir");
	assert_eq!(&full_rmdir_stdout_kernel.recv().expect("non-empty rmdir error newline").bytes, b"\n");

	let rm_name: &[u8] = b"rm";
	let (rm_stdout_kernel, rm_stdout_user) = Channel::create();
	let (rm_bootstrap_kernel, rm_bootstrap_user) = Channel::create();
	let mut rm_launch = alloc::vec::Vec::new();
	rm_launch.extend_from_slice(&3u16.to_le_bytes());
	rm_launch.extend_from_slice(&24u32.to_le_bytes());
	rm_launch.extend_from_slice(&(rm_name.len() as u16).to_le_bytes());
	rm_launch.extend_from_slice(rm_name);
	rm_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &rm_launch, rm_bootstrap_user, Rights::ALL).expect("dynamic rm launch request");
	sched::run_until_idle();
	let rm_reply = process_client.recv().expect("dynamic rm launch reply");
	assert_eq!(le_u32(&rm_reply.bytes, 0), 24);
	assert_eq!(rm_reply.bytes[4], 1, "the rm PIE loaded with its manifest providers");
	send_cap(&rm_bootstrap_kernel, b"STDOUT", rm_stdout_user, Rights::ALL).expect("dynamic rm stdout bootstrap");
	rm_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	rm_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir/dynamic-write.txt", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("dynamic rm arguments");
	send_cap(&rm_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic rm system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		rm_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic rm absent volume");
	}
	rm_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut rm_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = rm_stdout_kernel.recv() {
			rm_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&rm_prefix.expect("dynamic rm confirmation prefix").bytes, b"removed ");
	assert_eq!(&rm_stdout_kernel.recv().expect("dynamic rm confirmation path").bytes, b"vol://system/dynamic-dir/dynamic-write.txt");
	assert_eq!(&rm_stdout_kernel.recv().expect("dynamic rm confirmation newline").bytes, b"\n");

	let (rmdir_stdout_kernel, rmdir_stdout_user) = Channel::create();
	let (rmdir_bootstrap_kernel, rmdir_bootstrap_user) = Channel::create();
	let mut rmdir_launch = alloc::vec::Vec::new();
	rmdir_launch.extend_from_slice(&3u16.to_le_bytes());
	rmdir_launch.extend_from_slice(&28u32.to_le_bytes());
	rmdir_launch.extend_from_slice(&(rmdir_name.len() as u16).to_le_bytes());
	rmdir_launch.extend_from_slice(rmdir_name);
	rmdir_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &rmdir_launch, rmdir_bootstrap_user, Rights::ALL).expect("empty rmdir launch request");
	sched::run_until_idle();
	let rmdir_reply = process_client.recv().expect("empty rmdir launch reply");
	assert_eq!(le_u32(&rmdir_reply.bytes, 0), 28);
	assert_eq!(rmdir_reply.bytes[4], 1, "the rmdir PIE loaded for empty removal");
	send_cap(&rmdir_bootstrap_kernel, b"STDOUT", rmdir_stdout_user, Rights::ALL).expect("empty rmdir stdout bootstrap");
	rmdir_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	rmdir_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("empty rmdir arguments");
	send_cap(&rmdir_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("empty rmdir system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		rmdir_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("empty rmdir absent volume");
	}
	rmdir_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut rmdir_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = rmdir_stdout_kernel.recv() {
			rmdir_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&rmdir_prefix.expect("empty rmdir confirmation prefix").bytes, b"removed ");
	assert_eq!(&rmdir_stdout_kernel.recv().expect("empty rmdir confirmation path").bytes, b"vol://system/dynamic-dir");
	assert_eq!(&rmdir_stdout_kernel.recv().expect("empty rmdir confirmation newline").bytes, b"\n");

	let (missing_stdout_kernel, missing_stdout_user) = Channel::create();
	let (missing_bootstrap_kernel, missing_bootstrap_user) = Channel::create();
	let mut missing_launch = alloc::vec::Vec::new();
	missing_launch.extend_from_slice(&3u16.to_le_bytes());
	missing_launch.extend_from_slice(&25u32.to_le_bytes());
	missing_launch.extend_from_slice(&(cat_name.len() as u16).to_le_bytes());
	missing_launch.extend_from_slice(cat_name);
	missing_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &missing_launch, missing_bootstrap_user, Rights::ALL).expect("missing-file cat launch request");
	sched::run_until_idle();
	let missing_reply = process_client.recv().expect("missing-file cat launch reply");
	assert_eq!(le_u32(&missing_reply.bytes, 0), 25);
	assert_eq!(missing_reply.bytes[4], 1, "the cat PIE loaded for negative read-back");
	send_cap(&missing_bootstrap_kernel, b"STDOUT", missing_stdout_user, Rights::ALL).expect("missing-file cat stdout bootstrap");
	missing_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	missing_bootstrap_kernel.send(Message::new(crate::tests::launch_context(b"vol://system/dynamic-dir/dynamic-write.txt", b"vol://system/"), alloc::vec::Vec::new(), 0)).expect("missing-file cat arguments");
	send_cap(&missing_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("missing-file cat system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..], &b"RAM"[..], &b"TMP"[..]] {
		missing_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("missing-file cat absent volume");
	}
	missing_bootstrap_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut missing_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = missing_stdout_kernel.recv() {
			missing_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&missing_prefix.expect("missing-file cat error prefix").bytes, b"cat: ");
	assert_eq!(&missing_stdout_kernel.recv().expect("missing-file cat error path").bytes, b"vol://system/dynamic-dir/dynamic-write.txt");
	assert_eq!(&missing_stdout_kernel.recv().expect("missing-file cat error suffix").bytes, b": cannot open\n");

	// LAUNCH the ET_DYN probe with a bootstrap channel. ProcessService must resolve
	// pix.lslib -> lsrt.lslib from vol://system/lib, load providers first, relocate the
	// probe's PLT call, and only then start it. Wire: op, corr, name, handle marker.
	let dynamic_name: &[u8] = b"dyn_probe";
	let (dynamic_report, dynamic_bootstrap) = Channel::create();
	let mut launch = alloc::vec::Vec::new();
	launch.extend_from_slice(&3u16.to_le_bytes());
	launch.extend_from_slice(&2u32.to_le_bytes());
	launch.extend_from_slice(&(dynamic_name.len() as u16).to_le_bytes());
	launch.extend_from_slice(dynamic_name);
	launch.extend_from_slice(&0u32.to_le_bytes());
	let dynamic_started = arch::tsc::now();
	send_cap(&process_client, &launch, dynamic_bootstrap, Rights::ALL).expect("dynamic launch request");
	sched::run_until_idle();

	let reply = process_client.recv().expect("dynamic launch reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 2, "dynamic launch echoes the correlation id");
	assert_eq!(b[4], 1, "the staged dynamic executable loaded with its providers");
	assert!(!reply.caps.is_empty(), "dynamic launch returns the Process capability");
	let dynamic_process = reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic launch capability is a Process");
	let report = dynamic_report.recv().expect("dynamic probe called its shared pix symbol");
	assert_eq!(&report.bytes, b"dynamic link ok");
	let dynamic_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(dynamic_started));
	crate::serial_println!("dynamic-start-perf: {}ns private-pages={} shared-pages={}", dynamic_ns, dynamic_process.private_image_pages(), dynamic_process.shared_image_pages());
	assert!(dynamic_ns != 0 && dynamic_process.private_image_pages() != 0 && dynamic_process.shared_image_pages() != 0);

	let (second_report, second_bootstrap) = Channel::create();
	let mut second_launch = alloc::vec::Vec::new();
	second_launch.extend_from_slice(&3u16.to_le_bytes());
	second_launch.extend_from_slice(&3u32.to_le_bytes());
	second_launch.extend_from_slice(&(dynamic_name.len() as u16).to_le_bytes());
	second_launch.extend_from_slice(dynamic_name);
	second_launch.extend_from_slice(&0u32.to_le_bytes());
	let warm_started = arch::tsc::now();
	send_cap(&process_client, &second_launch, second_bootstrap, Rights::ALL).expect("second dynamic launch request");
	sched::run_until_idle();
	let second_reply = process_client.recv().expect("second dynamic launch reply");
	assert_eq!(le_u32(&second_reply.bytes, 0), 3);
	assert_eq!(second_reply.bytes[4], 1);
	let second_process = second_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("second dynamic launch capability is a Process");
	assert_eq!(&second_report.recv().expect("second dynamic probe report").bytes, b"dynamic link ok");
	let warm_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(warm_started));
	let first_provider_frame = dynamic_process.address_space().unmap(0x2000_0000).expect("first liblsrt text page");
	let second_provider_frame = second_process.address_space().unmap(0x2000_0000).expect("second liblsrt text page");
	assert_eq!(first_provider_frame, second_provider_frame, "concurrent dynamic processes share one physical liblsrt text page");
	crate::serial_println!("dynamic-warm-perf: {}ns two-process-private-pages={} two-process-shared-refs={}", warm_ns, dynamic_process.private_image_pages() + second_process.private_image_pages(), dynamic_process.shared_image_pages() + second_process.shared_image_pages());
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(dynamic_wave_launch_metrics_are_structurally_sound, [Dynamic, Service, Process, Storage]);
fn dynamic_wave_launch_metrics_are_structurally_sound() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	// One representative per wave. No expected page counts: they used to be carried here, one set
	// per architecture, and they were maintenance with no return - see the note on
	// `measure_dynamic_wave_launch` for why an absolute count cannot distinguish a page boundary
	// landing one over from sharing having broken. The measured counts still reach the log through
	// the `dynamic-wave-perf` lines below, and the checked `docs/DYNAMIC_*.tsv` reports (which
	// `just dynamic-report-update` regenerates) remain the place where exact figures are reviewed.
	let representatives = [(1u8, b"echo" as &[u8], 100u32), (2, b"cat" as &[u8], 102), (3, b"date" as &[u8], 104), (4, b"ip" as &[u8], 106), (5, b"imgconv" as &[u8], 108)];
	for (wave, name, correlation) in representatives {
		measure_dynamic_wave_launch(&process_client, wave, name, correlation);
	}
	process_client.send(object::channel::Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(unrelated_dynamic_consumers_share_domain_and_codec_text, [Dynamic, Service, Process, Storage]);
fn unrelated_dynamic_consumers_share_domain_and_codec_text() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	assert_unrelated_dynamic_consumers_share(&process_client, b"cat", b"write", 120, 0x2400_0000, "volume-client");
	assert_unrelated_dynamic_consumers_share(&process_client, b"imgconv", b"imgview", 122, 0x2500_0000, "jpeg");
	process_client.send(object::channel::Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(dynamic_process_service_rejects_missing_provider, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_missing_provider() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	replace_dynamic_needed(&mut mutated_volume, test_program_path("echo").expect("echo destination").as_bytes(), "lsrt.lslib", "none.lslib");
	let reply = launch_from_volume(&mutated_volume, b"echo", 78);
	assert_eq!(le_u32(&reply.bytes, 0), 78);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects an absent direct provider");
	assert!(reply.caps.is_empty(), "an absent provider creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_undeclared_provider_edge, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_undeclared_provider_edge() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	replace_dynamic_needed(&mut mutated_volume, test_program_path("echo").expect("echo destination").as_bytes(), "lsrt.lslib", "wire.lslib");
	let reply = launch_from_volume(&mutated_volume, b"echo", 79);
	assert_eq!(le_u32(&reply.bytes, 0), 79);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects an undeclared provider edge");
	assert!(reply.caps.is_empty(), "an undeclared provider edge creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_duplicate_provider_edge, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_duplicate_provider_edge() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	duplicate_dynamic_needed(&mut mutated_volume, test_program_path("dyn_probe").expect("dyn_probe destination").as_bytes());
	let reply = launch_from_volume(&mutated_volume, b"dyn_probe", 80);
	assert_eq!(le_u32(&reply.bytes, 0), 80);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a duplicate provider edge");
	assert!(reply.caps.is_empty(), "a duplicate provider edge creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_malformed_dynamic_metadata, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_malformed_dynamic_metadata() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	for (correlation, mutate) in [
		(83u32, duplicate_dynamic_segment as fn(&mut [u8], &[u8])),
		(84, remove_dynamic_terminator as fn(&mut [u8], &[u8])),
		(85, duplicate_dynamic_singleton as fn(&mut [u8], &[u8])),
	] {
		let mut mutated_volume = volume.to_vec();
		mutate(&mut mutated_volume, test_program_path("dyn_probe").expect("dyn_probe destination").as_bytes());
		let reply = launch_from_volume(&mutated_volume, b"dyn_probe", correlation);
		assert_eq!(le_u32(&reply.bytes, 0), correlation);
		assert_eq!(reply.bytes[4], 0, "ProcessService rejects malformed dynamic metadata");
		assert!(reply.caps.is_empty(), "malformed dynamic metadata creates no process capability");
	}
}

tagged_test!(dynamic_process_service_rejects_malformed_symbol_and_relocation_metadata, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_malformed_symbol_and_relocation_metadata() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	for (correlation, mutate) in [
		(86u32, invalidate_dynamic_symbol_entry_size as fn(&mut [u8], &[u8])),
		(87, overflow_dynamic_symbol_count as fn(&mut [u8], &[u8])),
		(88, invalidate_plt_relocation_size as fn(&mut [u8], &[u8])),
	] {
		let mut mutated_volume = volume.to_vec();
		mutate(&mut mutated_volume, test_program_path("dyn_probe").expect("dyn_probe destination").as_bytes());
		let reply = launch_from_volume(&mutated_volume, b"dyn_probe", correlation);
		assert_eq!(le_u32(&reply.bytes, 0), correlation);
		assert_eq!(reply.bytes[4], 0, "ProcessService rejects malformed symbol or relocation metadata");
		assert!(reply.caps.is_empty(), "malformed symbol or relocation metadata creates no process capability");
	}
}

tagged_test!(dynamic_process_service_rejects_provider_cycle, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_provider_cycle() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	replace_dynamic_needed(&mut mutated_volume, b"lib/ipc/wire.lslib", "lsrt.lslib", "wire.lslib");
	let reply = launch_from_volume(&mutated_volume, b"lscpu", 89);
	assert_eq!(le_u32(&reply.bytes, 0), 89);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a provider dependency cycle");
	assert!(reply.caps.is_empty(), "a provider dependency cycle creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_substituted_or_corrupted_identity_note, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_substituted_or_corrupted_identity_note() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut substituted_provider = volume.to_vec();
	replace_volume_entry(&mut substituted_provider, b"lib/runtime/lsrt.lslib", b"lib/ipc/wire.lslib");
	let reply = launch_from_volume(&substituted_provider, b"echo", 80);
	assert_eq!(le_u32(&reply.bytes, 0), 80);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a valid provider substituted under lsrt.lslib");
	assert!(reply.caps.is_empty(), "a substituted provider creates no process capability");

	let mut corrupted_identity = volume.to_vec();
	corrupt_identity_note(&mut corrupted_identity, b"lib/runtime/lsrt.lslib", b"profile=");
	let reply = launch_from_volume(&corrupted_identity, b"echo", 81);
	assert_eq!(le_u32(&reply.bytes, 0), 81);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a provider whose embedded identity record is malformed");
	assert!(reply.caps.is_empty(), "a corrupted embedded identity record creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_duplicate_provider_export, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_duplicate_provider_export() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut duplicated_export = volume.to_vec();
	replace_provider_export(&mut duplicated_export, b"lib/image/pix.lslib", b"lib/runtime/lsrt.lslib");
	let reply = launch_from_volume(&duplicated_export, b"dyn_probe", 82);
	assert_eq!(le_u32(&reply.bytes, 0), 82);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a provider that duplicates a runtime export");
	assert!(reply.caps.is_empty(), "a duplicate provider export creates no process capability");
}

tagged_test!(dynamic_process_service_derives_provider_slots_independently_of_needed_order, [Dynamic, Service, Process, Storage]);
fn dynamic_process_service_derives_provider_slots_independently_of_needed_order() {
	use object::process::Process;

	let (volume, _) = scenario_packages().expect("scenario packages");
	let baseline_reply = launch_from_volume(volume, b"dyn_probe", 90);
	assert_eq!(baseline_reply.bytes[4], 1, "baseline dynamic graph loads");
	let baseline = baseline_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("baseline dynamic launch returns a Process");

	let mut reordered_volume = volume.to_vec();
	swap_dynamic_needed_order(&mut reordered_volume, test_program_path("dyn_probe").expect("dyn_probe destination").as_bytes());
	let reordered_reply = launch_from_volume(&reordered_volume, b"dyn_probe", 91);
	assert_eq!(reordered_reply.bytes[4], 1, "reordered DT_NEEDED graph loads");
	let reordered = reordered_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("reordered dynamic launch returns a Process");

	for provider_slot in [0x2000_0000u64, 0x2100_0000] {
		let baseline_frame = baseline.address_space().unmap(provider_slot).expect("baseline provider occupies its canonical slot");
		let reordered_frame = reordered.address_space().unmap(provider_slot).expect("reordered provider occupies its canonical slot");
		assert_eq!(reordered_frame, baseline_frame, "provider slot {provider_slot:#x} is independent of DT_NEEDED enumeration order");
	}
}

tagged_test!(a_load_that_runs_out_of_frames_anywhere_gives_back_everything, [Dynamic, Memory, Process]);
fn a_load_that_runs_out_of_frames_anywhere_gives_back_everything() {
	// A real service image, loaded over and over with the frame allocator told to refuse the
	// k-th allocation for k = 0, 1, 2, ... - so the failure walks through every allocation the
	// load makes: the address space's own top-level table, each intermediate page-table frame,
	// the first ELF data frame, the ones after it, the first stack frame, mid-stack.
	//
	// After every refusal the pool must be exactly what it was before the call. That single
	// number covers what the rollback is for: a frame allocated and not recorded, a segment
	// unmapped without its frames returned, a stack half-mapped and abandoned, page tables
	// built for a load that never finished. Any of them shows up here as a pool that shrank.
	//
	// Draining the pool cannot reach any of this - it fails the first allocation, so the load
	// gives up before it has anything to leak.
	use crate::mem::frame;
	use crate::object::rights::Rights;
	use crate::{loader, sched};

	let bytes = crate::init_package_bytes().expect("init package present");
	let package = pkg::Package::parse(bytes).expect("init package parses");
	let elf = package.lookup(b"log_service.lsexe").expect("log_service.lsexe image");

	// How many allocations a whole load takes, measured rather than assumed - the image
	// decides it and it differs per port.
	let mut refusals = 0;
	let mut succeeded_at = None;
	for budget in 0..4096 {
		let before = frame::free_count();
		let (kernel_ep, user_ep) = crate::object::channel::Channel::create();
		frame::fail_allocations_after(budget);
		let result = loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL, 0);
		frame::stop_failing_allocations();
		match result {
			Err(_) => {
				refusals += 1;
				// The channel endpoints are the test's, not the load's; dropping them here
				// keeps the comparison to what the loader did.
				drop(kernel_ep);
				// The frames come back through the QUARANTINE: anything that was MAPPED waits for a
				// shootdown that retires every core's translation of it, and those are batched
				// rather than taken one span at a time. Draining is what "wait for the shootdown"
				// looks like from here - without it this measures the queue rather than the load.
				// Settle, THEN compare.
				//
				// Two things stand between a frame being given up and the count showing it: the
				// quarantine, which holds anything that was mapped until a shootdown retires it,
				// and whatever teardown is still in flight elsewhere on the machine - a thread
				// exiting on another core retires its kernel stack whenever it gets there, which
				// can be inside this test's window.
				//
				// So the test settles rather than sampling once: run the machine down, drain, look,
				// repeat. A straggler matches on the second or third turn; a genuine leak never
				// matches, and the count is printed either way.
				// NOT LOWER than it started, rather than equal to it.
				//
				// Equality was asserting a number this test does not own. The free count is the
				// whole machine's: a thread exiting on another core returns its kernel stack
				// whenever it gets there, and that lands inside this window in either direction -
				// the first version of this settle loop reported nine frames SHORT in the full
				// suite and two hundred and eighty-seven SPARE once it let the machine run down.
				// Both were other people's work, and neither said anything about the load.
				//
				// What the load owes is that it kept nothing. A leak shows as a count that stays
				// below where it started however long the machine is given; frames returned by
				// somebody else only push it up.
				let mut settled = false;
				for _ in 0..16 {
					crate::sched::run_until_idle();
					assert!(frame::drain_quarantine_fully(64), "the shootdown never completed, so the frames could not come back");
					if frame::free_count() >= before {
						settled = true;
						break;
					}
				}
				assert!(settled, "a load refused at allocation {budget} kept {} frame(s) it took ({} still quarantined)", before as i64 - frame::free_count() as i64, frame::quarantined());
			}
			Ok(process) => {
				// The load got all the way through. Take the process down again and stop:
				// past this point the budget is larger than a whole load needs.
				process.terminate();
				drop(process);
				drop(kernel_ep);
				succeeded_at = Some(budget);
				break;
			}
		}
	}
	let succeeded_at = succeeded_at.expect("no budget up to 4096 completed a load: the injection is not reaching the loader");
	assert!(refusals > 0, "the first budget already completed a load: nothing was injected");
	crate::serial_println!("(load takes {succeeded_at} allocations; {refusals} refusals checked) ");
}

tagged_test!(a_fuzzed_elf_header_is_refused_without_leaking, [Dynamic, Memory, Process]);
fn a_fuzzed_elf_header_is_refused_without_leaking() {
	// Random damage to a well-formed image, in the two structures the loader parses before it
	// trusts anything: the ELF header and the program headers. Every one of these is a byte an
	// attacker chooses, and the loader reads sizes, counts, offsets and addresses out of them.
	//
	// Two properties, on every iteration: the call returns rather than panicking, and the frame
	// pool afterwards is exactly what it was before. A refusal that leaked what it had already
	// allocated is the failure mode that matters here - a hostile image would only have to be
	// submitted repeatedly.
	//
	// Fixed-seed xorshift, so a failure is reproducible from the iteration number alone.
	use crate::mem::frame;
	use crate::object::address_space::AddressSpace;

	fn put16(bytes: &mut [u8], offset: usize, value: u16) {
		bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	}
	fn put32(bytes: &mut [u8], offset: usize, value: u32) {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}
	fn put64(bytes: &mut [u8], offset: usize, value: u64) {
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}

	// The same minimal ET_EXEC the boundary tests use: one executable PT_LOAD at a low address.
	fn well_formed() -> Vec<u8> {
		const CODE_OFFSET: usize = 0x200;
		let mut bytes = alloc::vec![0u8; CODE_OFFSET + 0x40];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 2);
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, 0x40_0000);
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 1);
		let base = 64;
		put32(&mut bytes, base, 1);
		put32(&mut bytes, base + 4, 5);
		put64(&mut bytes, base + 8, CODE_OFFSET as u64);
		put64(&mut bytes, base + 16, 0x40_0000);
		put64(&mut bytes, base + 24, 0);
		put64(&mut bytes, base + 32, 1);
		put64(&mut bytes, base + 40, 1);
		put64(&mut bytes, base + 48, 1);
		bytes[CODE_OFFSET] = 0xc3;
		bytes
	}

	let mut state: u64 = 0x2545_F491_4F6C_DD1D;
	let mut next = move || {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		state
	};

	const ITERATIONS: usize = 400;
	// The header is 64 bytes and the one program header another 56; damage lands inside those
	// 120, which is what makes this a header fuzz rather than a payload fuzz.
	const HEADERS_END: usize = 120;
	let mut loaded = 0;
	let before = frame::free_count();
	for iteration in 0..ITERATIONS {
		let mut image = well_formed();
		// One to four damaged bytes, so single-field corruption and multi-field corruption
		// both get a turn.
		for _ in 0..(1 + next() % 4) {
			let at = (next() as usize) % HEADERS_END;
			image[at] = next() as u8;
		}
		let space = AddressSpace::create().expect("a scratch address space");
		let mut frames = Vec::new();
		let mut shared = Vec::new();
		// The call must RETURN. Reaching the next line at all is half the assertion.
		if crate::elf::load_into(&image, &space, &mut frames, &mut shared).is_ok() {
			loaded += 1;
		}
		// Whatever it did, the frames it took are the caller's to release - on the error path
		// `load_parsed` has already unmapped them, and on the success path they are ours.
		// SAFETY: every frame here came from this load and is named by nothing else now.
		unsafe { frame::free_pages(&frames) };
		drop(shared);
		drop(space);
		assert_eq!(frame::free_count(), before, "iteration {iteration} left the frame pool short");
	}
	// Some damage is harmless (padding, a reserved field), so a few must still load - otherwise
	// this is fuzzing a parser that refuses everything and proving nothing.
	assert!(loaded > 0, "every fuzzed image was refused: the fuzz is not producing loadable ones");
	assert!(loaded < ITERATIONS, "every fuzzed image loaded: the fuzz is not reaching the checks");
	crate::serial_println!("({loaded}/{ITERATIONS} fuzzed images still loaded) ");
}

tagged_test!(a_fuzzed_relocation_table_is_refused_without_leaking, [Dynamic, Memory, Process]);
fn a_fuzzed_relocation_table_is_refused_without_leaking() {
	// The other structure the loader walks on an image's say-so. A `.dynamic` array names where
	// the relocations are, how many, and how wide; each relocation names an address to patch
	// and a value to patch in. Every one of those numbers is the image's, and the loader writes
	// through them into the address space it is building.
	//
	// Same two properties per iteration as the header fuzz: the call returns, and the frame
	// pool afterwards is exactly what it was. Fixed-seed xorshift.
	use crate::mem::frame;
	use crate::object::address_space::AddressSpace;

	fn put16(bytes: &mut [u8], offset: usize, value: u16) {
		bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	}
	fn put32(bytes: &mut [u8], offset: usize, value: u32) {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}
	fn put64(bytes: &mut [u8], offset: usize, value: u64) {
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}
	fn program_header(bytes: &mut [u8], index: usize, kind: u32, flags: u32, offset: u64, address: u64, file_size: u64, memory_size: u64) {
		let at = 64 + index * 56;
		put32(bytes, at, kind);
		put32(bytes, at + 4, flags);
		put64(bytes, at + 8, offset);
		put64(bytes, at + 16, address);
		put64(bytes, at + 24, 0);
		put64(bytes, at + 32, file_size);
		put64(bytes, at + 40, memory_size);
		put64(bytes, at + 48, 1);
	}

	const CODE_OFFSET: usize = 0x200;
	const DATA_OFFSET: usize = 0x300;
	const DATA_ADDRESS: u64 = 0x2000;
	const RELA_OFFSET: usize = 0x40;
	const DYNAMIC_OFFSET: usize = 0x80;
	const DATA_LEN: usize = 0x100;

	// A relative-only ET_DYN that loads: one relocation adding the bias to a word in its data
	// segment. Damage lands in the RELA and .dynamic bytes, which is what makes this a
	// relocation fuzz rather than a header one.
	fn well_formed() -> Vec<u8> {
		let mut bytes = alloc::vec![0u8; DATA_OFFSET + DATA_LEN];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 3); // ET_DYN
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, 0);
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 3);
		program_header(&mut bytes, 0, 1, 5, CODE_OFFSET as u64, 0, 1, 1);
		program_header(&mut bytes, 1, 1, 6, DATA_OFFSET as u64, DATA_ADDRESS, DATA_LEN as u64, DATA_LEN as u64);
		program_header(&mut bytes, 2, 2, 6, (DATA_OFFSET + DYNAMIC_OFFSET) as u64, DATA_ADDRESS + DYNAMIC_OFFSET as u64, 5 * 16, 5 * 16);
		bytes[CODE_OFFSET] = 0xc3;
		let rela = DATA_OFFSET + RELA_OFFSET;
		put64(&mut bytes, rela, DATA_ADDRESS);
		put64(&mut bytes, rela + 8, relative_relocation_type() as u64);
		put64(&mut bytes, rela + 16, 0x1234);
		let dynamic = DATA_OFFSET + DYNAMIC_OFFSET;
		for (index, (tag, value)) in [(7u64, DATA_ADDRESS + RELA_OFFSET as u64), (8, 24), (9, 24), (0x6fff_fff9, 1), (0, 0)].into_iter().enumerate() {
			put64(&mut bytes, dynamic + index * 16, tag);
			put64(&mut bytes, dynamic + index * 16 + 8, value);
		}
		bytes
	}

	// Prove the undamaged one loads, so a fuzz that refuses everything is visible as a bug in
	// the fixture rather than as evidence about the loader.
	{
		let space = AddressSpace::create().expect("a scratch address space");
		let mut frames = Vec::new();
		let mut shared = Vec::new();
		crate::elf::load_into(&well_formed(), &space, &mut frames, &mut shared).expect("the undamaged fixture must load");
		// SAFETY: frames from this load, named by nothing else now that the space is going.
		drop(space);
		unsafe { frame::free_pages(&frames) };
	}

	let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
	let mut next = move || {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		state
	};

	const ITERATIONS: usize = 400;
	// The RELA entries and the .dynamic array, and nothing else: [RELA_OFFSET, DYNAMIC_OFFSET +
	// 5*16) inside the data segment.
	let damage_lo = DATA_OFFSET + RELA_OFFSET;
	let damage_hi = DATA_OFFSET + DYNAMIC_OFFSET + 5 * 16;
	let mut loaded = 0;
	let before = frame::free_count();
	for iteration in 0..ITERATIONS {
		let mut image = well_formed();
		for _ in 0..(1 + next() % 4) {
			let at = damage_lo + (next() as usize) % (damage_hi - damage_lo);
			image[at] = next() as u8;
		}
		let space = AddressSpace::create().expect("a scratch address space");
		let mut frames = Vec::new();
		let mut shared = Vec::new();
		if crate::elf::load_into(&image, &space, &mut frames, &mut shared).is_ok() {
			loaded += 1;
		}
		drop(shared);
		drop(space);
		// SAFETY: every frame here came from this load; the space that mapped them is gone.
		unsafe { frame::free_pages(&frames) };
		assert_eq!(frame::free_count(), before, "iteration {iteration} left the frame pool short");
	}
	assert!(loaded > 0, "every fuzzed relocation table was refused: the fuzz is not producing loadable ones");
	assert!(loaded < ITERATIONS, "every fuzzed relocation table loaded: the fuzz is not reaching the checks");
	crate::serial_println!("({loaded}/{ITERATIONS} fuzzed relocation tables still loaded) ");
}
