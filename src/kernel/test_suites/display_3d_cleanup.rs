//! Real demo/DisplayService cleanup after a Domain refusal and the emergency kill path.

use super::display_harness::*;
use super::*;
use object::domain::{Domain, UNLIMITED};

fn bind(harness: &Harness, process: &Arc<object::process::Process>) -> Arc<Channel> {
	let frame = request(display_admin::OP_BIND, 50, &0u32.to_le_bytes());
	send_cap(&harness.admin, &frame.bytes, process.clone(), Rights::MANAGE | Rights::WAIT | Rights::TRANSFER).expect("bind the actual demo process");
	sched::run_until_idle();
	let reply = harness.admin.recv().expect("bound display reply");
	succeeded(&reply, 50);
	reply.caps.first().expect("bound display capability").object().into_any_arc().downcast::<Channel>().expect("bound display channel")
}

fn acknowledge_present(gpu: &Channel, present: Message) {
	let mut reader = wire::Reader::new(&present.bytes);
	assert_eq!(reader.u16(), Some(display_device::display_device::OP_PRESENT));
	let corr = reader.u32().expect("present correlation");
	reader.u32().expect("backing generation");
	display_device::DamageRegion::read(&mut reader).expect("typed device damage");
	reader.finish().expect("complete device present");
	let mut reply = corr.to_le_bytes().to_vec();
	reply.push(1);
	gpu.send(Message::new(reply, alloc::vec::Vec::new())).expect("device completed present");
}

fn run_case(deny_second_image: bool) {
	const CONSOLE: u32 = 0x0011_4477;
	let harness = start(64, 48);
	let console = create_surface(&harness.console, &harness.focus, b"CONSOLE", 1, 0, 0, 2);
	let configuration = adopt(&console, 2);
	let (queue, _producer, _done) = present_queue(&console, 4);
	let image = provide_queue(&console, 5, &queue, 48);
	fill(&image, CONSOLE, 64 * 48);
	assert_eq!(acquire_next(&console, 8), display_v1::AcquiredImage::Image(0));
	send_present(&console, 9, 0, &configuration, &display_v1::DamageRegion { whole: true, rects: alloc::vec::Vec::new() });
	device_present(&harness.gpu, "the console before the demo");
	present_reply(&console, 9);
	assert_eq!(scanout_pixel(&harness.scanout), CONSOLE);
	let baseline = display_resources(&harness.stats, 10);
	assert_eq!((baseline.surfaces, baseline.present_images), (1, 2));

	let (volume, package) = scenario_packages().expect("scenario packages");
	let elf = program_elf(&package, volume, b"test3d-sw").expect("staged production demo");
	let domain = Domain::new_child(&sched::root_domain(), UNLIMITED, UNLIMITED, UNLIMITED).expect("demo Domain");
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let process = spawn_dynamic_test_process(domain.clone(), elf, child);
	let display = bind(&harness, &process);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("demo stdout");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).unwrap();
	let arguments: &[u8] = if deny_second_image { b"--width 2048 --height 2048 --no-input --workers 1" } else { b"--width 64 --height 48 --no-input --workers 1" };
	bootstrap.send(Message::new(launch_context(arguments, b"vol://system"), alloc::vec::Vec::new())).unwrap();
	// Keep this kernel reference deliberately: reclamation must follow the bound process, even
	// while another holder keeps the display endpoint alive.
	send_cap(&bootstrap, b"DISPLAY", display.clone(), Rights::ALL).expect("the process-bound grant");
	let mut output = alloc::vec::Vec::new();
	let mut took_screen = false;
	let mut restored = false;
	let mut killed = false;
	let mut presents = 0;
	let mut quota_baseline = None;
	for _ in 0..40_000 {
		sched::run_until_idle_until(arch::apic::ticks().saturating_add(1));
		while let Ok(message) = stdout.recv() {
			output.extend_from_slice(&message.bytes);
		}
		while let Ok(focus) = harness.focus.recv() {
			match focus.bytes.as_slice() {
				b"SET" => {
					assert!(!took_screen, "one foreground transition per demo");
					took_screen = true;
					if deny_second_image {
						// The runtime has initialized and the service is holding create-surface's
						// reply. One 16 MiB queue image fits, the second cannot fit. No production
						// allocator hook or artificial protocol refusal is involved.
						let used = domain.account().memory().used();
						domain.account().memory().set_limit(used + 20 * 1024 * 1024);
						quota_baseline = Some(used);
					}
				}
				b"CONSOLE" => restored = true,
				other => panic!("unexpected focus transition: {other:?}"),
			}
			harness.focus.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).unwrap();
		}
		while let Ok(present) = harness.gpu.recv() {
			acknowledge_present(&harness.gpu, present);
			presents += 1;
		}
		let rendering = output.windows(b"test3d-sw: core passes recorded by scene3d".len()).any(|bytes| bytes == b"test3d-sw: core passes recorded by scene3d");
		if !deny_second_image && !killed && rendering && presents >= 3 {
			assert!(!process.is_terminated(), "emergency kill targets the running demo");
			assert_ne!(scanout_pixel(&harness.scanout), CONSOLE, "the demo actually replaced the console");
			harness.kill.send(Message::new(b"KILL".to_vec(), alloc::vec::Vec::new())).expect("the private emergency display command");
			killed = true;
		}
		if process.is_terminated() && restored && display.is_peer_closed() && scanout_pixel(&harness.scanout) == CONSOLE {
			break;
		}
	}
	while let Ok(message) = stdout.recv() {
		output.extend_from_slice(&message.bytes);
	}
	for line in output.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
		crate::serial_println!("  {}", alloc::string::String::from_utf8_lossy(line));
	}
	assert!(took_screen && restored, "the actual demo took and released the foreground: {output:?}");
	assert!(process.is_terminated(), "the demo ended within the fixture's bound: {output:?}");
	assert!(display.is_peer_closed(), "the bound connection was reclaimed while this reference stayed live");
	assert_eq!(scanout_pixel(&harness.scanout), CONSOLE, "the already-painted console returned without redrawing");
	let held = display_resources(&harness.stats, 100);
	assert_eq!((held.surfaces, held.present_images, held.queued_presents, held.damage_entries, held.waiters), (baseline.surfaces, baseline.present_images, baseline.queued_presents, baseline.damage_entries, baseline.waiters), "the demo left no surface, imported image, queued present or wait registration behind");
	assert_eq!(domain.account().memory().used(), process.memory_bytes(), "only the held process's private image/stack pages remain; all heap and shared queue images were refunded");
	assert_eq!(domain.account().handles().used(), 0, "the exited demo holds no handle");
	if deny_second_image {
		let before = quota_baseline.expect("the Domain ceiling was installed before queue allocation");
		assert!(domain.account().memory().peak() >= before + 16 * 1024 * 1024, "one full image was really allocated before the second was refused");
		assert!(domain.account().memory().peak() <= before + 20 * 1024 * 1024, "the enforced limit was never exceeded");
		assert!(!process.is_killed(), "the allocation refusal exited cleanly rather than panicking");
		assert!(output.windows(b"test3d-sw: no surface".len()).any(|bytes| bytes == b"test3d-sw: no surface"), "the demo reported its failed queue allocation: {output:?}");
	} else {
		assert!(killed && process.is_killed(), "DisplayService killed the process bound through display-admin");
	}
	// The original console remains usable after either failure, not merely visible in one pixel.
	assert_eq!(acquire_next(&console, 101), display_v1::AcquiredImage::Image(0));
	send_present(&console, 102, 0, &configuration, &display_v1::DamageRegion { whole: true, rects: alloc::vec::Vec::new() });
	device_present(&harness.gpu, "console presentation after demo cleanup");
	present_reply(&console, 102);
	harness.service.terminate();
}

tagged_test!(the_3d_demo_releases_a_partly_allocated_queue_and_restores_the_console, [Service, Display, Process, Domain, Image], id = "kernel.services.the_3d_demo_releases_a_partly_allocated_queue_and_restores_the_console", covers = ["bin.test3d-sw", "bin.display_service", "graphics-app", "surface"]);
fn the_3d_demo_releases_a_partly_allocated_queue_and_restores_the_console() {
	run_case(true);
}

tagged_test!(the_3d_demo_emergency_kill_restores_the_console_and_refunds_its_images, [Service, Display, Process, Domain, Image], id = "kernel.services.the_3d_demo_emergency_kill_restores_the_console_and_refunds_its_images", covers = ["bin.test3d-sw", "bin.display_service", "graphics-app", "surface"]);
fn the_3d_demo_emergency_kill_restores_the_console_and_refunds_its_images() {
	run_case(false);
}
