use super::*;

fn wait_message(storage: &mut StorageHarness, channel: &object::channel::Channel, description: &str) -> object::channel::Message {
	for _ in 0..100_000 {
		storage.pump();
		if let Ok(message) = channel.recv() {
			return message;
		}
	}
	panic!("{description}");
}

fn wait_terminated(storage: &mut StorageHarness, process: &object::process::Process, description: &str) {
	for _ in 0..100_000 {
		storage.pump();
		if process.is_terminated() {
			return;
		}
	}
	panic!("{description}");
}

fn start_process_service(storage: &mut StorageHarness, package: &pkg::Package<'static>) -> alloc::sync::Arc<object::channel::Channel> {
	use object::channel::Channel;
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("ProcessService image");
	let (boot, boot_user) = Channel::create();
	let (server, client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), process_elf, boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	send_package(&boot, init).expect("ProcessService package bootstrap");
	send_cap(&boot, b"STORAGE", storage.connect(), Rights::ALL).expect("ProcessService storage bootstrap");
	// The development registry, with its far end dropped: nothing answers, so every
	// launch reads the volume. The handoff itself cannot be skipped - the bootstrap
	// consumes one message per handoff in order, so a missing one swallows the SERVE
	// channel and the service then blocks for a message that already came and went.
	let (registry_server, registry_client) = Channel::create();
	core::mem::drop(registry_server);
	send_cap(&boot, b"REGISTRY", registry_client, Rights::ALL).expect("ProcessService registry bootstrap");
	send_cap(&boot, b"SERVE", server, Rights::ALL).expect("ProcessService serve bootstrap");
	let online = wait_message(storage, &boot, "ProcessService did not report online");
	assert_eq!(&online.bytes[..], b"ProcessService: online", "ProcessService serves the fresh system volume");
	client
}

fn launch_volume_program(storage: &mut StorageHarness, process_client: &alloc::sync::Arc<object::channel::Channel>, name: &str, correlation: u32) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::process::Process>) {
	use object::channel::Channel;
	use object::process::Process;
	use object::rights::Rights;

	let (bootstrap, child) = Channel::create();
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&3u16.to_le_bytes());
	request.extend_from_slice(&correlation.to_le_bytes());
	request.extend_from_slice(&(name.len() as u16).to_le_bytes());
	request.extend_from_slice(name.as_bytes());
	request.extend_from_slice(&0u32.to_le_bytes());
	send_cap(process_client, &request, child, Rights::ALL).expect("volume program launch request");
	let reply = wait_message(storage, process_client, "ProcessService did not answer the volume launch");
	assert_eq!(le_u32(&reply.bytes, 0), correlation, "volume launch echoes its correlation id");
	assert_eq!(reply.bytes.get(4), Some(&1), "the program loads from its manifest path on the fresh volume");
	let process = reply.caps.first().expect("volume launch returns a Process").object().into_any_arc().downcast::<Process>().expect("volume launch capability is a Process");
	(bootstrap, process)
}

fn start_config_service(storage: &mut StorageHarness, process_client: &alloc::sync::Arc<object::channel::Channel>, correlation: u32) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::process::Process>) {
	use object::channel::Channel;
	use object::rights::Rights;

	let (boot, process) = launch_volume_program(storage, process_client, "config_service", correlation);
	let (server, client) = Channel::create();
	let scope = storage.open_directory(b"vol://system/libexec/config_service");
	send_cap(&boot, b"STORAGE", scope, Rights::ALL).expect("ConfigService directory scope bootstrap");
	send_cap(&boot, b"SERVE", server, Rights::ALL).expect("ConfigService serve bootstrap");
	let online = wait_message(storage, &boot, "ConfigService did not report online");
	assert_eq!(&online.bytes[..], b"ConfigService: online", "ConfigService starts from libexec on the fresh volume");
	(client, process)
}

fn start_log_service(storage: &mut StorageHarness, package: &pkg::Package<'static>) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>) {
	use object::channel::Channel;
	use object::rights::Rights;

	let log_elf = package.lookup(b"log_service.lsexe").expect("LogService image");
	let (boot, boot_user) = Channel::create();
	let (server, client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), log_elf, boot_user, Rights::ALL, 0).expect("spawn LogService");
	send_cap(&boot, b"SERVE", server, Rights::ALL).expect("LogService serve bootstrap");
	let online = wait_message(storage, &boot, "LogService did not report online");
	assert_eq!(&online.bytes[..], b"LogService: online", "LogService reports in");
	send_cap(&boot, b"STORAGE", storage.open_directory(b"vol://system/log"), Rights::ALL).expect("LogService journal scope bootstrap");
	for _ in 0..16 {
		storage.pump();
	}
	(boot, client)
}

fn emit_persistent_log_entry(storage: &mut StorageHarness, log_client: &object::channel::Channel, correlation: u32) {
	use abi::log::{self, Severity};
	use object::channel::Message;

	let mut entry = [0u8; 256];
	let len = log::encode(1, Severity::Error, b"volume-layout", &[(b"event" as &[u8], b"persisted" as &[u8])], &mut entry).expect("encode journal entry");
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&1u16.to_le_bytes());
	request.extend_from_slice(&correlation.to_le_bytes());
	request.extend_from_slice(&entry[..len]);
	log_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("LogService emit request");
	let reply = wait_message(storage, log_client, "LogService did not acknowledge the persistent entry");
	assert_eq!(le_u32(&reply.bytes, 0), correlation, "LogService emit echoes its correlation id");
	assert_eq!(reply.bytes.get(4), Some(&1), "an error entry flushes to the system journal");
}

fn query_previous_log_boot(storage: &mut StorageHarness, log_client: &object::channel::Channel, correlation: u32, boot: u32) -> object::channel::Message {
	use object::channel::Message;

	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&2u16.to_le_bytes());
	request.extend_from_slice(&correlation.to_le_bytes());
	request.extend_from_slice(&[0, 0, 0, 1]);
	request.extend_from_slice(&boot.to_le_bytes());
	request.extend_from_slice(&0u32.to_le_bytes());
	log_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("LogService boot query request");
	wait_message(storage, log_client, "LogService did not answer the previous-boot query")
}

tagged_test!(directory_scoped_storage_clients_cannot_escape_their_grant, [Filesystem, Storage, VolumeLayout, VolumeScope]);
fn directory_scoped_storage_clients_cannot_escape_their_grant() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let component = test_factory_path("liber-component").expect("component factory path");
	let component_directory = component.rsplit_once('/').expect("component path has an owner directory").0;
	let component_uri = alloc::format!("vol://system/{component}");
	let hello = test_factory_path("hello").expect("hello factory path");
	let hello_uri = alloc::format!("vol://system/{hello}");
	let config = test_runtime_path("config-tree").expect("config runtime path");
	let config_uri = alloc::format!("vol://system/{config}");
	let mut storage = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	let full = storage.connect();
	let expected = volume_file(volume, component.as_bytes()).expect("component payload in the archive");
	assert_eq!(storage.open_from(&full, component_uri.as_bytes(), 0xd100), Some(expected), "a full client reaches the seeded component payload");

	let scope_uri = alloc::format!("vol://system/{component_directory}");
	let scope = storage.open_directory(scope_uri.as_bytes());
	assert!(storage.open_from(&scope, component_uri.as_bytes(), 0xd101).is_some(), "the directory scope reads its own component payload");
	assert!(storage.open_from(&scope, hello_uri.as_bytes(), 0xd102).is_none(), "the directory scope cannot read a factory file outside its grant");
	assert!(storage.open_from(&scope, config_uri.as_bytes(), 0xd103).is_none(), "the directory scope cannot read ConfigService state outside its grant");

	let child = storage.connect_from(&scope);
	assert!(storage.open_from(&child, hello_uri.as_bytes(), 0xd104).is_none(), "a child minted by a directory scope cannot widen to the volume root");
}

tagged_test!(existing_system_volume_preserves_owned_state_across_a_restart, [Filesystem, Storage, VolumeLayout, VolumeScope]);
// This used to corrupt the factory archive between the two runs and assert that an existing
// volume mounted as-is rather than being reformatted from the changed seed. There is no seed any
// more (M0138) - and the bytes it used to corrupt are the superblock now - so what remains
// testable, and still worth testing, is that an existing volume survives a restart with its
// owner state intact.
fn existing_system_volume_preserves_owned_state_across_a_restart() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let config_tree = test_runtime_path("config-tree").expect("config runtime path");
	let config_uri = alloc::format!("vol://system/{config_tree}");
	let hello = test_factory_path("hello").expect("hello factory path");
	let hello_uri = alloc::format!("vol://system/{hello}");
	let marker = b"preserved existing system volume state";
	let expected_hello = volume_file(volume, hello.as_bytes()).expect("hello in factory archive");
	let mut storage = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	assert!(storage.write(config_uri.as_bytes(), marker, 0xd200), "the fresh writable volume records owner state");
	assert_eq!(storage.open(config_uri.as_bytes(), 0xd201), Some(marker.to_vec()), "the owner state reads before restart");
	let mut storage = storage.restart(storage_elf);
	assert_eq!(storage.open(config_uri.as_bytes(), 0xd202), Some(marker.to_vec()), "an existing LiberFS mounts as-is rather than being reformatted");
	assert_eq!(storage.open(hello_uri.as_bytes(), 0xd203), Some(expected_hello), "the volume's own files remain available after the restart");
}

tagged_test!(fresh_seeded_system_volume_runs_each_layout_class_and_reopens_owned_state, [Component, Config, Drivers, Filesystem, Process, ProcessService, Service, Storage, VolumeLayout, VolumeScope]);
fn fresh_seeded_system_volume_runs_each_layout_class_and_reopens_owned_state() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let hello = test_factory_path("hello").expect("hello factory path");
	let motd = test_factory_path("motd").expect("motd factory path");
	let audio = test_factory_path("audio-demo").expect("audio factory path");
	let wallpaper = test_factory_path("wallpaper-logo").expect("wallpaper factory path");
	let component = test_factory_path("liber-component").expect("component factory path");
	let component_output = test_runtime_path("liber-component-output").expect("component output path");
	let config_tree = test_runtime_path("config-tree").expect("config runtime path");
	let journal_root = test_runtime_path("system-journal").expect("journal runtime path");
	let mut storage = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	for (name, path) in [("hello", hello), ("motd", motd), ("audio", audio), ("wallpaper", wallpaper)] {
		let uri = alloc::format!("vol://system/{path}");
		let expected = volume_file(volume, path.as_bytes()).expect("declared factory file in the archive");
		assert_eq!(storage.open(uri.as_bytes(), 0xd300 + name.len() as u32), Some(expected), "the fresh writable volume preserves the declared {name} factory file");
	}

	let driver = test_program_path("xhci").expect("xhci driver path");
	let driver_uri = alloc::format!("vol://system/{driver}");
	let driver_bytes = storage.open(driver_uri.as_bytes(), 0xd310).expect("the fresh volume stages xhci in drivers");
	assert!(bootproto::elf::Elf::parse(&driver_bytes).is_some(), "the staged driver remains a valid ELF; the paired boot-chain contract launches it on QEMU hardware");

	let process_client = start_process_service(&mut storage, &package);
	let (echo_boot, echo_process) = launch_volume_program(&mut storage, &process_client, "echo", 0xd320);
	let (echo_stdout, echo_stdout_child) = Channel::create();
	send_cap(&echo_boot, b"STDOUT", echo_stdout_child, Rights::ALL).expect("echo stdout bootstrap");
	echo_boot.send(Message::new(crate::tests::launch_context(b"fresh volume command", b""), alloc::vec::Vec::new(), 0)).expect("echo argument bootstrap");
	assert_eq!(&wait_message(&mut storage, &echo_stdout, "echo did not print its command result").bytes[..], b"fresh volume command", "a tool launches from bin through ProcessService");
	assert_eq!(&wait_message(&mut storage, &echo_stdout, "echo did not print its newline").bytes[..], b"\n");
	wait_terminated(&mut storage, &echo_process, "echo did not exit");

	let (config_client, config_process) = start_config_service(&mut storage, &process_client, 0xd330);
	let key = b"volume.layout.persist";
	let value = b"survives";
	let mut set = alloc::vec::Vec::new();
	set.extend_from_slice(&3u16.to_le_bytes());
	set.extend_from_slice(&1u32.to_le_bytes());
	set.extend_from_slice(&(key.len() as u16).to_le_bytes());
	set.extend_from_slice(key);
	set.extend_from_slice(&(value.len() as u16).to_le_bytes());
	set.extend_from_slice(value);
	config_client.send(Message::new(set, alloc::vec::Vec::new(), 0)).expect("ConfigService set request");
	let set_reply = wait_message(&mut storage, &config_client, "ConfigService did not acknowledge the set");
	assert_eq!(le_u32(&set_reply.bytes, 0), 1);
	assert_eq!(set_reply.bytes.get(4), Some(&1), "ConfigService persists through its directory scope");
	let config_uri = alloc::format!("vol://system/{config_tree}");
	assert!(storage.open(config_uri.as_bytes(), 0xd331).is_some(), "ConfigService writes its tree beside its libexec artifact");
	config_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("ConfigService quit sentinel");
	wait_terminated(&mut storage, &config_process, "the first ConfigService did not exit");

	let (config_client, config_process) = start_config_service(&mut storage, &process_client, 0xd332);
	let mut get = alloc::vec::Vec::new();
	get.extend_from_slice(&1u16.to_le_bytes());
	get.extend_from_slice(&2u32.to_le_bytes());
	get.extend_from_slice(&(key.len() as u16).to_le_bytes());
	get.extend_from_slice(key);
	config_client.send(Message::new(get, alloc::vec::Vec::new(), 0)).expect("ConfigService persisted get request");
	let get_reply = wait_message(&mut storage, &config_client, "the replacement ConfigService did not answer");
	assert_eq!(le_u32(&get_reply.bytes, 0), 2);
	assert_eq!(get_reply.bytes.get(4), Some(&1), "the replacement ConfigService found the persisted key");
	let value_len = le_u16(&get_reply.bytes, 5) as usize;
	assert_eq!(&get_reply.bytes[7..7 + value_len], value, "ConfigService reloads the exact persisted value");
	config_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("replacement ConfigService quit sentinel");
	wait_terminated(&mut storage, &config_process, "the replacement ConfigService did not exit");

	let (log_boot, log_client) = start_log_service(&mut storage, &package);
	let (component_boot, component_process) = launch_volume_program(&mut storage, &process_client, "component_host", 0xd340);
	send_cap(&component_boot, b"STORAGE", storage.connect(), Rights::ALL).expect("ComponentHost storage bootstrap");
	send_cap(&component_boot, b"LOG", log_client.clone(), Rights::ALL).expect("ComponentHost log bootstrap");
	let report = wait_message(&mut storage, &component_boot, "ComponentHost did not report its component result");
	assert!(report.bytes.len() >= 5, "ComponentHost result has its log flag and score");
	assert_ne!(report.bytes[0], 0, "the component reaches its LogService grant");
	assert_eq!(i32::from_le_bytes([report.bytes[1], report.bytes[2], report.bytes[3], report.bytes[4]]), 17, "the staged component runs through ComponentHost");
	let expected_component_output: alloc::vec::Vec<u8> = volume_file(volume, hello.as_bytes()).expect("hello factory file").iter().map(|byte| byte.to_ascii_uppercase()).collect();
	assert_eq!(&report.bytes[5..], expected_component_output.as_slice(), "the component transforms its granted factory input");
	wait_terminated(&mut storage, &component_process, "ComponentHost did not exit");
	let component_output_uri = alloc::format!("vol://system/{component_output}");
	assert_eq!(storage.open(component_output_uri.as_bytes(), 0xd341), Some(expected_component_output.clone()), "ComponentHost writes its runtime output beside the component payload");

	emit_persistent_log_entry(&mut storage, &log_client, 0xd350);
	let journal_uri = alloc::format!("vol://system/{journal_root}/boot-1");
	assert!(storage.open(journal_uri.as_bytes(), 0xd351).is_some(), "LogService writes the system journal below log");
	let component_directory = component.rsplit_once('/').expect("component path has an owner directory").0;
	let component_scope = storage.open_directory(alloc::format!("vol://system/{component_directory}").as_bytes());
	assert_eq!(storage.open_from(&component_scope, component_output_uri.as_bytes(), 0xd352), Some(expected_component_output), "a component-owned scope opens its own runtime output");
	assert!(storage.open_from(&component_scope, config_uri.as_bytes(), 0xd353).is_none(), "a component-owned scope cannot open ConfigService state");
	assert!(storage.open_from(&component_scope, journal_uri.as_bytes(), 0xd354).is_none(), "a component-owned scope cannot open the system journal");

	log_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("first LogService quit sentinel");
	for _ in 0..100_000 {
		storage.pump();
		if log_client.is_peer_closed() {
			break;
		}
	}
	assert!(log_client.is_peer_closed(), "the first LogService exits before its replacement starts");
	let (_log_boot, replacement_log_client) = start_log_service(&mut storage, &package);
	let reply = query_previous_log_boot(&mut storage, &replacement_log_client, 0xd355, 1);
	assert_eq!(le_u32(&reply.bytes, 0), 0xd355, "previous-boot journal query echoes its correlation id");
	assert_eq!(reply.bytes.get(4), Some(&1), "the replacement LogService reopens boot 1");
	assert!(le_u16(&reply.bytes, 5) >= 1, "the reopened journal has at least the flushed error entry");
	assert!(reply.bytes.windows(b"volume-layout".len()).any(|window| window == b"volume-layout"), "the reopened journal preserves the structured error entry");

	replacement_log_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("replacement LogService quit sentinel");
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("ProcessService quit sentinel");
	let _ = log_boot;
}
