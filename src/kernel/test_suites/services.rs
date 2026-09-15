use super::*;
use display_device_proto::codec as wire;
use display_device_proto::generated::liber::display_device::v1 as display_device;

// A managed service that cannot complete a required bootstrap step reports the failing
// step and the reason over its bootstrap channel before exiting, so the supervisor
// records why it went down instead of seeing an unexplained peer-close. DeviceManager
// needs the init package before it reports in; hand it a plain message where the package
// should be and it reports the failure honestly rather than dying silently.
tagged_test!(a_service_reports_a_bootstrap_failure, [Service, Boot], id = "kernel.services.a_service_reports_a_bootstrap_failure", covers = ["kernel", "services"]);
fn a_service_reports_a_bootstrap_failure() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let device_elf = package.lookup(b"device_manager.lsexe").expect("device_manager.lsexe in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), device_elf, boot_user, Rights::ALL).expect("spawn device_manager");
	// Where the "PACKAGE" grant should be, hand it a plain message with no transferred
	// object: recv_package rejects it and the service reports the failing step.
	boot_kernel.send(Message::new(b"NOTAPACKAGE".to_vec(), alloc::vec::Vec::new())).expect("bogus bootstrap");

	sched::run_until_idle();

	let report = boot_kernel.recv().expect("a bootstrap failure report");
	assert!(report.bytes.starts_with(b"BOOTFAIL"), "reports the failing step, not a silent exit");
	assert!(report.bytes.windows(7).any(|w| w == b"package"), "the report names the failing step");
}

tagged_test!(log_service_speaks_generated_bindings, [Service], id = "kernel.services.log_service_speaks_generated_bindings", covers = ["kernel", "bin.log_service"]);
fn log_service_speaks_generated_bindings() {
	use abi::log::{self, Severity};
	use object::channel::Message;

	// Drive the real userspace LogService as a client over its generated Log
	// bindings: spawn it from the init package, hand it a serve channel, EMIT two
	// records and QUERY them back. The wire is the proto framing - request
	// [op u16][corr u32][args], reply [corr u32][result] - and the proto Entry
	// encoding is byte-for-byte the abi::log record, so we build entries with
	// log::encode and frame them by hand. Everything is pre-queued so the
	// cooperative service drains it in one pass and exits, after which we read its
	// replies (the kernel-as-client pattern).
	let (_boot_kernel, service_client) = spawn_service(b"log_service");

	// EMIT one record: [op = 1 (emit) u16][corr u32][entry bytes].
	let emit = |corr: u32, ts: u64, severity: Severity, source: &[u8], fields: &[(&[u8], &[u8])]| {
		let mut wire = [0u8; 128];
		let n = log::encode(ts, severity, source, fields, &mut wire).expect("encode entry");
		let mut msg = alloc::vec::Vec::new();
		msg.extend_from_slice(&1u16.to_le_bytes());
		msg.extend_from_slice(&corr.to_le_bytes());
		msg.extend_from_slice(&wire[..n]);
		service_client.send(Message::new(msg, alloc::vec::Vec::new())).expect("emit");
	};
	emit(1, 10, Severity::Info, b"storage_service", &[(b"event" as &[u8], b"online" as &[u8])]);
	emit(2, 11, Severity::Error, b"device_manager", &[(b"code" as &[u8], b"5" as &[u8])]);

	// QUERY all severities: [op = 2 (query) u16][corr u32][query bytes]. The query
	// record is since:option<u64> min-severity:option<severity> source:option<string>
	// boot:option<u32> limit:u32; all-absent with limit 0 is eight zero bytes.
	let mut q = alloc::vec::Vec::new();
	q.extend_from_slice(&2u16.to_le_bytes());
	q.extend_from_slice(&7u32.to_le_bytes());
	q.extend_from_slice(&[0u8; 8]);
	service_client.send(Message::new(q, alloc::vec::Vec::new())).expect("query");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("quit sentinel");

	sched::run_until_idle();

	// Each emit is a round-trip replying result<unit, error> = [corr u32][ok u8 = 1].
	for corr in [1u32, 2] {
		let reply = service_client.recv().expect("emit reply");
		assert_eq!(reply.bytes.len(), 5, "emit reply is corr + ok");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "emit reply echoes the correlation id");
		assert_eq!(reply.bytes[4], 1, "emit succeeded");
	}

	// The query reply is [corr u32 = 7][ok u8 = 1][count u16 = 2][entry][entry].
	let reply = service_client.recv().expect("query reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 7, "query reply echoes the correlation id");
	assert_eq!(b[4], 1, "query succeeded");
	assert_eq!(le_u16(b, 5), 2, "both records came back");
	// spot-check both entries are present in the structured reply
	assert!(b.windows(b"storage_service".len()).any(|w: &[u8]| w == b"storage_service"), "first entry present");
	assert!(b.windows(b"device_manager".len()).any(|w: &[u8]| w == b"device_manager"), "second entry present");
}

tagged_test!(input_service_streams_pointer_events, [Service, Input, Mouse, Console], id = "kernel.services.input_service_streams_pointer_events", covers = ["kernel", "services", "bin.input_service"]);
fn input_service_streams_pointer_events() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Drive the real userspace InputService end to end over its generated Input
	// bindings: spawn it from the init package, hand it a SERVE channel, an INPUT
	// raw channel (the one the virtio_input pointer driver would feed), and a FORWARD
	// channel (ConsoleService's pointer sink, which it mirrors raw events to), inject a
	// couple of normalized [x u16][y u16][buttons u8] pointer events the way the
	// driver does, then SUBSCRIBE and read the mapped text-cell events back off the
	// stream. The pointer device is interactive-only, so here the test plays the
	// driver itself by sending raw events on the producer end it keeps.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"input_service").expect("input_service in the package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	let (raw_producer, raw_consumer) = Channel::create();
	let (_key_producer, key_consumer) = Channel::create();
	let (_focus_display, focus_input) = Channel::create();
	// ConsoleService's pointer sink: the test keeps the consumer end alive so the forward
	// channel stays open (InputService mirrors each raw event to it), but does not assert
	// on it here - the forwarding path is exercised by the live console.
	let (_forward_drain, forward_input) = Channel::create();
	let _input_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	send_cap(&boot_kernel, b"FORWARD", forward_input, Rights::ALL).expect("forward raw bootstrap");
	send_cap(&boot_kernel, b"KEYS", key_consumer, Rights::ALL).expect("key raw bootstrap");
	send_cap(&boot_kernel, b"FOCUS", focus_input, Rights::ALL).expect("focus bootstrap");
	boot_kernel.send(Message::new(b"KILL".to_vec(), alloc::vec::Vec::new())).expect("kill bootstrap");
	let (_admin_peer, admin) = Channel::create();
	send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("input admin bootstrap");
	// THE PROVIDER CATALOGUE, LAST, AND IT IS HOW THIS SERVICE FINDS ITS POINTERS. It is not handed
	// `INPUT`/`INPUT2` channels any more - it subscribes to the input and pointer kinds - so this
	// harness answers both conversations: one with the raw pointer this scenario has, and one empty,
	// because it has no USB pointing device.
	let (catalogue_server, catalogue_client) = Channel::create();
	send_cap(&boot_kernel, b"CATALOGUE", catalogue_client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the catalogue channel");
	sched::run_until_idle();
	crate::tests::serve_provider_catalogue(&catalogue_server, device_proto::generated::liber::device::v1::ProviderKind::Input, raw_consumer).expect("the catalogue answered the pointer subscription");
	crate::tests::serve_provider_catalogue_empty(&catalogue_server).expect("the catalogue answered the usb-pointer subscription with nothing");

	// Inject two normalized pointer events as the driver would. The grid is COLS = 80
	// x ROWS = 50 over the 0..0x10000 normalized span, so col = (x * 80) / 0x10000 and
	// row = (y * 50) / 0x10000. x = y = 0x8000 (half span) lands on col 40 / row 25
	// with the left button held; the second event is the top-left corner, no buttons.
	let raw_event = |x: u16, y: u16, buttons: u8| -> Message {
		let mut bytes = alloc::vec::Vec::new();
		bytes.extend_from_slice(&x.to_le_bytes());
		bytes.extend_from_slice(&y.to_le_bytes());
		bytes.push(buttons);
		Message::new(bytes, alloc::vec::Vec::new())
	};
	raw_producer.send(raw_event(0x8000, 0x8000, 1)).expect("first pointer event");
	raw_producer.send(raw_event(0, 0, 0)).expect("second pointer event");

	// SUBSCRIBE: [op = 1 (subscribe) u16][corr u32], no args.
	let corr: u32 = 7;
	let mut req = alloc::vec::Vec::new();
	req.extend_from_slice(&1u16.to_le_bytes());
	req.extend_from_slice(&corr.to_le_bytes());
	service_client.send(Message::new(req, alloc::vec::Vec::new())).expect("subscribe request");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("InputService online report");
	assert_eq!(&online.bytes[..], b"InputService: online", "InputService reports in");

	// the subscribe reply is [corr u32] with the stream consumer transferred out of band
	let reply = service_client.recv().expect("subscribe reply");
	assert_eq!(le_u32(&reply.bytes, 0), corr, "subscribe reply echoes the correlation id");
	let cap = reply.caps.first().expect("the stream consumer is transferred");
	let consumer = cap.object().into_any_arc().downcast::<Channel>().expect("the consumer is a channel");

	// each event rides its own framed message [seq u32][col u16][row u16][buttons u8];
	// closing the producer ends the stream, so recv drains to a clean close.
	let mut events = alloc::vec::Vec::new();
	while let Ok(frame) = consumer.recv() {
		let f = &frame.bytes;
		events.push((le_u16(f, 4), le_u16(f, 6), f[8]));
	}
	assert_eq!(events.len(), 2, "both injected pointer events stream back");
	assert_eq!(events[0], (40, 25, 1), "the half-span event maps to the middle cell with the left button");
	assert_eq!(events[1], (0, 0, 0), "the corner event maps to column 0, row 0, no buttons");
}

tagged_test!(input_service_streams_keys_only_with_display_focus, [Service, Input, Display], id = "kernel.services.input_service_streams_keys_only_with_display_focus", covers = ["kernel"]);
fn input_service_streams_keys_only_with_display_focus() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	fn subscribe(client: &Channel, corr: u32, proof: alloc::sync::Arc<Channel>) -> Option<alloc::sync::Arc<Channel>> {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(client, &request, proof, Rights::ALL).expect("key subscription request");
		sched::run_until_idle();
		let reply = client.recv().expect("key subscription reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "subscription echoes correlation id");
		reply.caps.first().map(|cap| cap.object().into_any_arc().downcast::<Channel>().expect("key stream is a channel"))
	}

	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"input_service").expect("input_service in the package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, _service_client) = Channel::create();
	let (_pointer_a, pointer_b) = Channel::create();
	let (console_focus, forward_b) = Channel::create();
	let (keys_driver, keys_input) = Channel::create();
	let (focus_display, focus_input) = Channel::create();
	let (kill_display, kill_input) = Channel::create();
	let _input_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	send_cap(&boot_kernel, b"FORWARD", forward_b, Rights::ALL).expect("forward bootstrap");
	send_cap(&boot_kernel, b"KEYS", keys_input, Rights::ALL).expect("keys bootstrap");
	send_cap(&boot_kernel, b"FOCUS", focus_input, Rights::ALL).expect("focus bootstrap");
	send_cap(&boot_kernel, b"KILL", kill_input, Rights::ALL).expect("kill bootstrap");
	let (input_admin, admin) = Channel::create();
	send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("input admin bootstrap");
	// THE PROVIDER CATALOGUE, LAST - see the other InputService harness in this file.
	let (pointer_catalogue_server, pointer_catalogue_client) = Channel::create();
	send_cap(&boot_kernel, b"CATALOGUE", pointer_catalogue_client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the catalogue channel");
	sched::run_until_idle();
	crate::tests::serve_provider_catalogue(&pointer_catalogue_server, device_proto::generated::liber::device::v1::ProviderKind::Input, pointer_b).expect("the catalogue answered the pointer subscription");
	crate::tests::serve_provider_catalogue_empty(&pointer_catalogue_server).expect("the catalogue answered the usb-pointer subscription with nothing");
	sched::run_until_idle();
	let online = boot_kernel.recv().expect("InputService online report");
	assert_eq!(&online.bytes[..], b"InputService: online");
	let mut open_keys = alloc::vec::Vec::new();
	open_keys.extend_from_slice(&1u16.to_le_bytes());
	open_keys.extend_from_slice(&40u32.to_le_bytes());
	input_admin.send(Message::new(open_keys, alloc::vec::Vec::new())).expect("open key-only connection");
	sched::run_until_idle();
	let reply = input_admin.recv().expect("key-only connection reply");
	let scoped = reply.caps.first().expect("key-only connection").object().into_any_arc().downcast::<Channel>().expect("key-only grant is a channel");
	let mut pointer_request = alloc::vec::Vec::new();
	pointer_request.extend_from_slice(&1u16.to_le_bytes());
	pointer_request.extend_from_slice(&41u32.to_le_bytes());
	scoped.send(Message::new(pointer_request, alloc::vec::Vec::new())).expect("forbidden pointer snapshot");
	sched::run_until_idle();
	let denied = scoped.recv().expect("pointer scope denial");
	assert!(denied.caps.is_empty(), "key-only connection cannot open a pointer stream");

	// An unrelated channel is not a display-minted peer and cannot open the stream.
	let (forged, _forged_peer) = Channel::create();
	assert!(subscribe(&scoped, 1, forged).is_none(), "a forged focus proof must be refused");

	// DisplayService registers one peer and transfers its counterpart to the client.
	let (proof, registered) = Channel::create();
	send_cap(&focus_display, b"SET", registered, Rights::ALL).expect("register focus peer");
	let stream = subscribe(&scoped, 2, proof).expect("active display proof opens the key stream");
	let focus_ack = focus_display.recv().expect("focus acknowledgement");
	assert_eq!(&focus_ack.bytes[..], b"OK");
	let suppressed = console_focus.recv().expect("console focus suppression");
	assert_eq!(&suppressed.bytes[..], b"KEYFOCUS\0");
	keys_driver.send(Message::new(alloc::vec![0x04, 0, 1], alloc::vec::Vec::new())).expect("A down");
	keys_driver.send(Message::new(alloc::vec![0x04, 0, 1], alloc::vec::Vec::new())).expect("duplicate A down");
	sched::run_until_idle();
	focus_display.send(Message::new(b"CLEAR".to_vec(), alloc::vec::Vec::new())).expect("revoke focus");
	sched::run_until_idle();
	let clear_ack = focus_display.recv().expect("clear acknowledgement");
	assert_eq!(&clear_ack.bytes[..], b"OK");
	let cleared = console_focus.recv().expect("console focus clear");
	assert_eq!(&cleared.bytes[..], b"KEYFOCUS\0");
	let down = stream.recv().expect("A down frame");
	let up = stream.recv().expect("synthetic A up frame");
	assert_eq!((le_u16(&down.bytes, 4), down.bytes[6]), (0x04, 1), "canonical HID A down");
	assert_eq!((le_u16(&up.bytes, 4), up.bytes[6]), (0x04, 0), "focus loss releases held A");
	assert!(stream.recv().is_err(), "focus loss closes the key stream");
	keys_driver.send(Message::new(alloc::vec![0x04, 0, 0], alloc::vec::Vec::new())).expect("physical A up");
	sched::run_until_idle();
	focus_display.send(Message::new(b"CONSOLE".to_vec(), alloc::vec::Vec::new())).expect("restore console focus");
	sched::run_until_idle();
	let console_ack = focus_display.recv().expect("console focus acknowledgement");
	assert_eq!(&console_ack.bytes[..], b"OK");
	let restored = console_focus.recv().expect("console focus restoration");
	assert_eq!(&restored.bytes[..], b"KEYFOCUS\x01");

	// Ctrl+Alt+Esc is consumed as the emergency display-revocation chord.
	let (proof2, registered2) = Channel::create();
	send_cap(&focus_display, b"SET", registered2, Rights::ALL).expect("register second focus peer");
	let stream2 = subscribe(&scoped, 3, proof2).expect("second active proof opens a stream");
	let second_ack = focus_display.recv().expect("second focus acknowledgement");
	assert_eq!(&second_ack.bytes[..], b"OK");
	for event in [[0xe0, 0, 1], [0xe2, 0, 1], [0x29, 0, 1]] {
		keys_driver.send(Message::new(event.to_vec(), alloc::vec::Vec::new())).expect("kill chord key");
	}
	sched::run_until_idle();
	let kill = kill_display.recv().expect("kill chord reaches DisplayService");
	assert_eq!(&kill.bytes[..], b"KILL");
	let mut frames: usize = 0;
	while stream2.recv().is_ok() {
		frames += 1;
	}
	assert_eq!(frames, 6, "three key-down frames are followed by three synthetic releases");
}

// THE DISPLAY HARNESS, SHARED BY EVERY TEST THAT SPEAKS THE PRESENT-QUEUE CONTRACT.
//
// It lived inside one test and a second test needed all of it. Copying it would have been a second
// encoder of a wire this tree already generates one for, which is the mistake every comment in here
// is about - so it is a module, and the tests that drive a display use it rather than restate it.
mod display_harness {
	use super::*;
	pub(super) use alloc::sync::Arc;
	pub(super) use display_proto::generated::liber::display::v1 as display_v1;
	pub(super) use display_v1::{display, display_admin, surface};
	pub(super) use graphics_proto::generated::liber::graphics::v1::{Offset2d, Rect as DamageRect};
	pub(super) use object::channel::{Channel, Message};
	pub(super) use object::dma_buffer::DmaBuffer;
	pub(super) use object::memory_object::MemoryObject;
	pub(super) use object::rights::Rights;
	use wire::Sink as _;

	// EVERY REQUEST IS THE OP, THE CORRELATION AND THEN THE ARGUMENTS THE GENERATED WRITERS LAY OUT,
	// and every record below is decoded by the generated READER. A harness that built a field at a
	// hand-counted byte offset would be a second encoder of a wire this tree already generates one
	// for - which is how a moved field becomes a test that waits for a frame nobody is going to
	// send, and is how this one spent three minutes waiting for one.
	pub(super) fn request(op: u16, corr: u32, body: &[u8]) -> Message {
		let mut bytes = op.to_le_bytes().to_vec();
		bytes.extend_from_slice(&corr.to_le_bytes());
		bytes.extend_from_slice(body);
		Message::new(bytes, alloc::vec::Vec::new())
	}

	// The payload of a `result<T, error>` reply: the correlation, the success tag, then `T`.
	pub(super) fn succeeded<'a>(reply: &'a Message, corr: u32) -> &'a [u8] {
		assert_eq!(le_u32(&reply.bytes, 0), corr, "the reply echoes its correlation id");
		assert_eq!(reply.bytes[4], 1, "the call succeeded");
		&reply.bytes[5..]
	}

	// A TYPED REFUSAL IS STILL A REPLY, which is the half a client hangs on when it is missing.
	pub(super) fn refused(reply: &Message, corr: u32) {
		assert_eq!(le_u32(&reply.bytes, 0), corr, "a refusal echoes its correlation id");
		assert_eq!(reply.bytes[4], 0, "the call was refused");
	}

	// One call that the service answers without talking to anything else first.
	pub(super) fn call(channel: &Channel, op: u16, corr: u32, body: &[u8]) -> Message {
		channel.send(request(op, corr, body)).expect("a display request");
		sched::run_until_idle();
		channel.recv().expect("a display reply")
	}

	pub(super) fn damage(rects: &[(i32, i32, u32, u32)]) -> display_v1::DamageRegion {
		display_v1::DamageRegion { whole: false, rects: rects.iter().map(|(x, y, width, height)| DamageRect { origin: Offset2d { x: *x, y: *y }, size: display_v1::Extent2d { width: *width, height: *height } }).collect() }
	}

	// A present names its image, the configuration serial it was drawn for, its generation and its
	// damage. THE SERIAL AND THE GENERATION ARE NOT DECORATION: both must match the acknowledged
	// current configuration at acceptance, which is what stops a frame drawn for one size from being
	// shown at another.
	pub(super) fn present_body(image: u32, configuration: &display_v1::SurfaceConfiguration, damage: &display_v1::DamageRegion) -> alloc::vec::Vec<u8> {
		let mut writer = wire::VecWriter::new();
		writer.u32(image).expect("a present names its image");
		writer.u64(configuration.serial).expect("and the serial it was drawn for");
		writer.u64(configuration.generation).expect("and the generation it belongs to");
		damage.write(&mut writer).expect("a damage region encodes");
		writer.into_inner().expect("a present carries no capability")
	}

	pub(super) fn send_present(channel: &Channel, corr: u32, image: u32, configuration: &display_v1::SurfaceConfiguration, damage: &display_v1::DamageRegion) {
		channel.send(request(surface::OP_PRESENT, corr, &present_body(image, configuration, damage))).expect("a present request");
		sched::run_until_idle();
	}

	// Answer one typed present on the DEVICE wire, the way a driver does, and report the rectangles
	// it carried. The service is inside its own `flush` while the client waits for the present's
	// answer, which is exactly the shape a blocking completion would deadlock.
	pub(super) fn device_present(gpu: &Channel, what: &str) -> alloc::vec::Vec<(u32, u32, u32, u32)> {
		sched::run_until_idle();
		let Ok(present) = gpu.recv() else { panic!("no present reached the gpu at {what}") };
		let mut reader = wire::Reader::new(&present.bytes);
		assert_eq!(reader.u16().expect("a device present names its op"), display_device::display_device::OP_PRESENT, "DisplayService uses the acknowledged present path");
		let corr: u32 = reader.u32().expect("a device present carries a correlation id");
		let _generation: u32 = reader.u32().expect("a device present names the backing generation it was drawn against");
		let rects = display_device::DamageRegion::read(&mut reader).expect("a present carries a damage region");
		reader.finish().expect("and the damage is the whole request");
		let mut reply = corr.to_le_bytes().to_vec();
		reply.push(1);
		gpu.send(Message::new(reply, alloc::vec::Vec::new())).expect("present acknowledgement");
		sched::run_until_idle();
		rects.rects.iter().map(|rect| (rect.origin.x as u32, rect.origin.y as u32, rect.size.width, rect.size.height)).collect()
	}

	// A present answers with the serial it was accepted under, which is what a completion names.
	pub(super) fn present_reply(channel: &Channel, corr: u32) -> u64 {
		let reply = channel.recv().expect("a present is answered");
		let body = succeeded(&reply, corr);
		let mut reader = wire::Reader::new(body);
		let serial: u64 = reader.u64().expect("a present answers with its serial");
		reader.finish().expect("and the serial is the whole answer");
		serial
	}

	pub(super) fn surface_event(events: &Channel) -> display_v1::SurfaceEvent {
		let frame = events.recv().expect("a surface event");
		let mut reader = wire::Reader::new(&frame.bytes);
		let _seq: u32 = reader.u32().expect("an event frame is sequenced");
		let event = display_v1::SurfaceEvent::read(&mut reader).expect("a surface event decodes");
		reader.finish().expect("and the event is the whole frame");
		event
	}

	// THE SCREEN CHANGING HANDS IS TWO EVENTS AND NOT ONE. Visibility says whether to keep drawing
	// and focus says where input goes, and FOCUS BELONGS TO THE SURFACE rather than to the
	// connection - which is why the second event exists at all.
	pub(super) fn screen_changed_hands(events: &Channel, mine: bool) {
		match surface_event(events) {
			display_v1::SurfaceEvent::VisibilityChanged(visible) => assert_eq!(visible, mine, "a surface is told when it stops being the visible one"),
			other => panic!("expected a visibility change, got {other:?}"),
		}
		match surface_event(events) {
			display_v1::SurfaceEvent::FocusChanged(focused) => assert_eq!(focused, mine, "and focus is its own event"),
			other => panic!("expected a focus change, got {other:?}"),
		}
	}

	pub(super) fn configure_event(events: &Channel) -> display_v1::SurfaceConfiguration {
		match surface_event(events) {
			display_v1::SurfaceEvent::Configure(configuration) => configuration,
			other => panic!("expected a configuration snapshot, got {other:?}"),
		}
	}

	pub(super) fn connect(root: &Channel) -> Arc<Channel> {
		root.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new())).expect("connect request");
		sched::run_until_idle();
		let reply = root.recv().expect("connect reply");
		let cap = reply.caps.first().expect("connected display channel");
		cap.object().into_any_arc().downcast::<Channel>().expect("display connection is a channel")
	}

	pub(super) fn acknowledge_focus(focus: &Channel, expected: &[u8]) {
		sched::run_until_idle();
		let command = focus.recv().expect("focus command");
		assert_eq!(&command.bytes[..], expected, "expected focus transition");
		focus.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("focus acknowledgement");
	}

	// Create one surface. A surface becoming the visible one is a focus transition, and the service
	// is blocked inside its own handler waiting for the acknowledgement - so the harness answers
	// that in the MIDDLE of the call rather than after it.
	pub(super) fn create_surface(connection: &Channel, focus: &Channel, expected_focus: &[u8], corr: u32, width: u32, height: u32, images: u32) -> Arc<Channel> {
		let mut writer = wire::VecWriter::new();
		display_v1::SurfaceRequest { logical_extent: display_v1::Extent2d { width, height }, images }.write(&mut writer).expect("a surface request encodes");
		let body = writer.into_inner().expect("a surface request carries no capability");
		connection.send(request(display::OP_CREATE_SURFACE, corr, &body)).expect("a create-surface request");
		acknowledge_focus(focus, expected_focus);
		sched::run_until_idle();
		let reply = connection.recv().expect("a create-surface reply");
		succeeded(&reply, corr);
		let capability = reply.caps.first().expect("the surface capability");
		// THE SURFACE STAYS WHERE IT WAS CREATED, and that lives in the RIGHTS rather than in a
		// check: without `transfer` the endpoint cannot be moved to another process at all - the
		// move fails at the syscall, which is the only place the rule CAN live. A service cannot
		// detect a live transfer however it is written, because a message names the endpoint it
		// arrived on and neither it nor `ObjectInfo` reports the holder. One process identity owns a
		// surface's images, its resize authority and its cleanup.
		let rights = capability.rights();
		assert!(rights.contains(Rights::SEND | Rights::RECEIVE | Rights::WAIT), "a surface can be called on and waited on");
		assert!(!rights.contains(Rights::TRANSFER), "and cannot be moved to another process");
		assert!(!rights.contains(Rights::DUPLICATE), "nor copied, so a client cannot keep one while giving one away");
		capability.object().into_any_arc().downcast::<Channel>().expect("a surface is a channel")
	}

	// `configure -> rebuild -> ack-configure -> first present`, which is the lifecycle the whole
	// contract turns on: a surface that skipped the acknowledgement finds every acquire answering
	// `out-of-date` rather than drawing a frame for a size nothing has.
	pub(super) fn adopt(channel: &Channel, corr: u32) -> display_v1::SurfaceConfiguration {
		let reply = call(channel, surface::OP_CONFIGURATION, corr, &[]);
		let configuration = display_v1::SurfaceConfiguration::decode(succeeded(&reply, corr)).expect("a configuration snapshot decodes");
		let acknowledged = call(channel, surface::OP_ACK_CONFIGURE, corr + 1, &configuration.serial.to_le_bytes());
		succeeded(&acknowledged, corr + 1);
		configuration
	}

	// The present queue and its two completion endpoints.
	pub(super) fn present_queue(channel: &Channel, corr: u32) -> (display_v1::PresentQueue, Arc<Channel>, Arc<Channel>) {
		let reply = call(channel, surface::OP_QUEUE, corr, &[]);
		let body = succeeded(&reply, corr);
		// The record's own reader, over the bytes and a STAND-IN handle list: a kernel channel end
		// arrives here as a capability rather than as a process handle, so what the reader proves is
		// the field ORDER - and the two capabilities are then taken in that same order.
		let mut placeholders = wire::Handles::new();
		placeholders.push(1).expect("a stand-in producer handle");
		placeholders.push(2).expect("a stand-in completion handle");
		let mut reader = wire::Reader::with_handle_list(body, &placeholders);
		let queue = display_v1::PresentQueue::read(&mut reader).expect("a present queue decodes");
		reader.finish().expect("and the queue is the whole reply");
		assert_eq!((queue.producer, queue.done), (1, 2), "the producer endpoint precedes the completion endpoint");
		assert_eq!(reply.caps.len(), 2, "both completion endpoints travel with the queue");
		// THE TWO HALVES OF A COMPLETION PAIR ARE NOT THE SAME AUTHORITY, and each arrives with only
		// its own: neither can be used as the other, and neither can leave this process.
		let producer_rights = reply.caps[0].rights();
		let done_rights = reply.caps[1].rights();
		assert!(producer_rights.contains(Rights::SEND) && !producer_rights.contains(Rights::RECEIVE), "the producer end may only send");
		assert!(done_rights.contains(Rights::RECEIVE | Rights::WAIT) && !done_rights.contains(Rights::SEND), "and the completion end may only receive and wait");
		assert!(!producer_rights.contains(Rights::TRANSFER) && !done_rights.contains(Rights::TRANSFER), "and neither may be handed on");
		let producer = reply.caps[0].object().into_any_arc().downcast::<Channel>().expect("PRODUCER_READY is a channel");
		let done = reply.caps[1].object().into_any_arc().downcast::<Channel>().expect("PRESENT_DONE is a channel");
		(queue, producer, done)
	}

	// SUPPLY one image of the queue: a MemoryObject THIS SIDE created, because the supplier of an
	// image is the one charged for it. ONE PER CALL, because a message carrying a LIST of
	// capabilities has a count no schema can bound - and a decode that stopped part way could not
	// hand back the handles it had already taken.
	//
	// `read` AND `map` AND NOT `write`: the service composes FROM these pixels and never into them,
	// which is what the schema's `@rights` guard refuses a handle for lacking.
	pub(super) fn provide_image(channel: &Channel, corr: u32, index: u32, len: u64) -> Arc<MemoryObject> {
		let object = MemoryObject::create(len as usize).expect("a presentable image");
		let mut body = index.to_le_bytes().to_vec();
		body.extend_from_slice(&0u32.to_le_bytes());
		send_cap(channel, &request(surface::OP_PROVIDE_IMAGE, corr, &body).bytes, object.clone(), Rights::READ | Rights::MAP | Rights::TRANSFER).expect("a provide-image request");
		sched::run_until_idle();
		let reply = channel.recv().expect("a provide-image reply");
		succeeded(&reply, corr);
		object
	}

	// The same supply, REFUSED: the answer is a typed refusal and the capability is not kept.
	pub(super) fn provide_image_refused(channel: &Channel, corr: u32, index: u32, len: u64, rights: Rights) {
		let object = MemoryObject::create(len.max(1) as usize).expect("a presentable image");
		let mut body = index.to_le_bytes().to_vec();
		body.extend_from_slice(&0u32.to_le_bytes());
		send_cap(channel, &request(surface::OP_PROVIDE_IMAGE, corr, &body).bytes, object, rights).expect("a provide-image request");
		sched::run_until_idle();
		let reply = channel.recv().expect("a refusal is still a reply");
		refused(&reply, corr);
	}

	// Supply every slot of a queue and hand back the first - which is the one every acquire below
	// takes, because a present completes as soon as it is accepted and gives its image straight back.
	pub(super) fn provide_queue(channel: &Channel, corr: u32, queue: &display_v1::PresentQueue, height: u32) -> Arc<MemoryObject> {
		let len: u64 = u64::from(queue.pitch) * u64::from(height);
		let mut first: Option<Arc<MemoryObject>> = None;
		for index in 0..queue.images {
			let object = provide_image(channel, corr + index, index, len);
			if index == 0 {
				first = Some(object);
			}
		}
		first.expect("a queue has at least one image")
	}

	pub(super) fn acquire_next(channel: &Channel, corr: u32) -> display_v1::AcquiredImage {
		let reply = call(channel, surface::OP_ACQUIRE_NEXT, corr, &[]);
		display_v1::AcquiredImage::decode(succeeded(&reply, corr)).expect("an acquire answers one of the four")
	}

	// WHAT THE DISPLAY PATH HOLDS, read through the OBSERVATION root - which is NOT the admin one.
	// Nothing reachable from this endpoint can bind a process to a display or take the screen, which
	// is what makes "observation, never enforcement" a property of the capability rather than a
	// promise about the caller.
	pub(super) fn display_resources(stats: &Channel, corr: u32) -> display_v1::DisplayResources {
		stats.send(request(display_v1::display_stats::OP_RESOURCES, corr, &[])).expect("a display resources request");
		sched::run_until_idle();
		let reply = stats.recv().expect("a display resources reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "the resources echo their correlation id");
		display_v1::DisplayResources::decode(&reply.bytes[4..]).expect("the resources decode")
	}

	pub(super) fn display_stats(admin: &Channel, corr: u32) -> display_v1::PresentationStats {
		admin.send(request(display_admin::OP_STATS, corr, &[])).expect("display stats request");
		sched::run_until_idle();
		let reply = admin.recv().expect("display stats reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "the statistics echo their correlation id");
		display_v1::PresentationStats::decode(&reply.bytes[4..]).expect("the statistics decode")
	}

	// Paint `pixels` words across the object's frames, one frame at a time.
	//
	// It used to take `frames()[0]` and write the whole run contiguously from there, which is
	// only correct while the run fits in one frame. A MemoryObject's frames come from the
	// frame allocator and are not physically contiguous, so a 320x200 surface - 256 kB, 62.5
	// pages - wrote past its first frame and over 61 unrelated ones. That is how a benchmark
	// surface came to overwrite a live PML4: the kernel half of an address space became
	// `0x00336699` repeated, the next switch into it could not fetch the next instruction, and
	// the machine triple-faulted with nothing in the log. Every small surface here is 4x4 and
	// fits in one frame, which is why only the one large fill ever did damage.
	pub(super) fn fill(object: &MemoryObject, pixel: u32, pixels: usize) {
		const PER_FRAME: usize = crate::mem::frame::PAGE_SIZE as usize / core::mem::size_of::<u32>();
		let mut left = pixels;
		for frame in object.frames() {
			if left == 0 {
				break;
			}
			let take = left.min(PER_FRAME);
			let base = mem::hhdm_offset() + frame;
			let words = unsafe { core::slice::from_raw_parts_mut(base as *mut u32, take) };
			words.fill(pixel);
			left -= take;
		}
		assert_eq!(left, 0, "the surface has fewer frames than the fill needs");
	}

	pub(super) fn set_surface_pixel(object: &MemoryObject, index: usize, pixel: u32) {
		let base = mem::hhdm_offset() + object.frames()[0];
		unsafe { ((base as *mut u32).add(index)).write_unaligned(pixel) };
	}

	pub(super) fn scanout_pixel_at(scanout: &DmaBuffer, x: usize, y: usize) -> u32 {
		unsafe { (((mem::hhdm_offset() + scanout.frames()[0]) as *const u32).add(y * 4 + x)).read_unaligned() }
	}

	pub(super) fn scanout_pixel(scanout: &DmaBuffer) -> u32 {
		scanout_pixel_at(scanout, 0, 0)
	}

	// A create that is REFUSED, which is a different shape from one that succeeds: every bound and
	// every argument is checked BEFORE anything is taken, so there is no focus transition to answer
	// on the way and no capability comes back.
	pub(super) fn create_surface_refused(connection: &Channel, corr: u32, width: u32, height: u32, images: u32) {
		let mut writer = wire::VecWriter::new();
		display_v1::SurfaceRequest { logical_extent: display_v1::Extent2d { width, height }, images }.write(&mut writer).expect("a surface request encodes");
		let body = writer.into_inner().expect("a surface request carries no capability");
		connection.send(request(display::OP_CREATE_SURFACE, corr, &body)).expect("a create-surface request");
		sched::run_until_idle();
		let reply = connection.recv().expect("a refusal is still a reply");
		refused(&reply, corr);
		assert!(reply.caps.is_empty(), "and a refused create hands over nothing");
	}

	/// A LIVE DISPLAYSERVICE OVER A STAND-IN DEVICE, and the ends a test drives it through.
	///
	/// The bootstrap is read POSITIONALLY at every hop, so the order below is the service's own and
	/// not a preference: a harness that reordered one message would wedge bring-up somewhere later
	/// and look like the thing it was testing.
	pub(super) struct Harness {
		/// The root display connection, which is what the console holds.
		pub(super) console: Arc<Channel>,
		pub(super) focus: Arc<Channel>,
		pub(super) kill: Arc<Channel>,
		pub(super) admin: Arc<Channel>,
		/// The observation root, which can answer what the service holds and change nothing.
		pub(super) stats: Arc<Channel>,
		pub(super) gpu: Arc<Channel>,
		pub(super) device_events: Option<Arc<Channel>>,
		pub(super) scanout: Arc<DmaBuffer>,
		/// The service's own process, held because dropping it would end the thing under test.
		pub(super) service: Arc<object::process::Process>,
		pub(super) boot: Arc<Channel>,
	}

	pub(super) fn start(width: u32, height: u32) -> Harness {
		let init = init_package_bytes().expect("init package module not found");
		let volume = volume_package_bytes().expect("volume package module not found");
		let package = pkg::Package::parse(init).expect("init package parses");
		let service_elf = program_elf(&package, volume, b"display_service").expect("display_service in the package or volume");
		let (boot_kernel, boot_user) = Channel::create();
		let (service_server, console_client) = Channel::create();
		let (gpu_kernel, gpu_user) = Channel::create();
		let (focus_input, focus_display) = Channel::create();
		let (kill_input, kill_display) = Channel::create();
		let service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
		send_cap(&boot_kernel, b"FOCUS", focus_display, Rights::ALL).expect("focus bootstrap");
		send_cap(&boot_kernel, b"KILL", kill_display, Rights::ALL).expect("kill bootstrap");
		let (display_admin_channel, admin) = Channel::create();
		send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("display admin bootstrap");
		send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
		// The DisplayController capability. DisplayService tolerates handle 0 (it takes no boot
		// framebuffer and relies on the GPU scanout, which is what this test gives it) but it BLOCKS for
		// the message, so a launcher that omits it wedges bring-up before the FB handshake below - which
		// is what a positional bootstrap costs.
		boot_kernel.send(Message::new(b"DISPLAYCTL".to_vec(), alloc::vec::Vec::new())).expect("display capability bootstrap");
		// AND THE PROVIDER CATALOGUE LAST, WHICH IS HOW THIS SERVICE FINDS ITS SCANOUT. It is not handed
		// a `GPU` channel any more - it subscribes to the display kind and opens a connection to what it
		// finds - so this harness answers that conversation before the framebuffer handshake below.
		let (catalogue_server, catalogue_client) = Channel::create();
		send_cap(&boot_kernel, b"CATALOGUE", catalogue_client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the catalogue channel");
		// AND THE OBSERVATION ROOT, WHICH IS NOT THE ADMIN ONE. It is read BEFORE the subscription is
		// answered because the bootstrap is read POSITIONALLY and in order: a harness that answered the
		// catalogue first would be waiting for a subscribe the service cannot send until this arrives.
		let (stats_root, stats_service) = Channel::create();
		send_cap(&boot_kernel, b"STATS", stats_service, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("display stats bootstrap");
		sched::run_until_idle();
		crate::tests::serve_provider_catalogue(&catalogue_server, device_proto::generated::liber::device::v1::ProviderKind::Display, gpu_user).expect("the catalogue answered the subscription and the connection");

		// Answer the driver's FB handshake with a 4x4 B8G8R8X8 DMA scanout.
		sched::run_until_idle();
		let fb_request = gpu_kernel.recv().expect("framebuffer request");
		assert_eq!(le_u16(&fb_request.bytes, 0), display_device::display_device::OP_SCANOUT, "DisplayService asks the device for its scanout");
		let scanout = match DmaBuffer::create_in(&sched::root_domain(), (width * height * 4) as usize) {
			Ok(scanout) => scanout,
			Err(_) => panic!("stand-in scanout"),
		};
		let fb_reply = crate::tests::scanout_reply(le_u32(&fb_request.bytes, 2), width, height, (width * height * 4) as u64);
		// `WRITE` EXPLICITLY: DisplayService COMPOSITES into this scanout. It was handed over with `MAP`
		// alone, which worked only while every mapping was writable regardless of what the capability
		// said - `sys_memory_map` and `sys_dma_buffer_map` now set the writable bit from `Rights::WRITE`,
		// so a surface to be drawn into has to say it is one.
		send_cap(&gpu_kernel, &fb_reply, scanout.clone(), Rights::READ | Rights::WRITE | Rights::MAP | Rights::TRANSFER).expect("framebuffer response");
		sched::run_until_idle();
		// AND THE DEVICE'S EVENT STREAM, which the service opens as soon as it has adopted the device: a
		// resize and a replacement arrive there, and not on the channel a present is answered on.
		let device_stream = crate::tests::answer_device_events(&gpu_kernel);
		sched::run_until_idle();
		let online = boot_kernel.recv().expect("DisplayService online report");
		assert_eq!(&online.bytes[..], b"DisplayService: online", "DisplayService reports in");
		Harness { console: console_client, focus: focus_input, kill: kill_input, stats: stats_root, admin: display_admin_channel, gpu: gpu_kernel, device_events: device_stream, scanout, service, boot: boot_kernel }
	}
}

// `Memory` GOES FOR THE REASON THE IMAGE SUITE'S DOES: this test is about a display service
// restoring a console surface, and it allocates on the way. What it CATCHES is already recorded in
// `covers` below and is unchanged by this; what moves is only which changes select it.
tagged_test!(display_service_restores_the_console_surface, [Service, Console, Display], id = "kernel.services.display_service_restores_the_console_surface", covers = ["kernel", "term", "bin.display_service"]);
fn display_service_restores_the_console_surface() {
	use display_harness::*;
	use object::address_space::AddressSpace;
	use object::process::Process;

	// A LIVE DISPLAYSERVICE OVER A 4x4 STAND-IN SCANOUT, with every end this test drives it through.
	let harness = display_harness::start(4, 4);
	let console_client = harness.console;
	let focus_input = harness.focus;
	let kill_input = harness.kill;
	let display_admin_channel = harness.admin;
	let stats_root = harness.stats;
	let gpu_kernel = harness.gpu;
	let device_stream = harness.device_events;
	let scanout = harness.scanout;
	let _display_service = harness.service;
	let _boot_kernel = harness.boot;

	// THE CONSOLE'S SURFACE: native size, and two images - the fewest that can double-buffer.
	let console_surface = create_surface(&console_client, &focus_input, b"CONSOLE", 1, 0, 0, 2);
	let limits_reply = call(&console_client, display::OP_IMAGE_LIMITS, 2, &[]);
	let limits = display_v1::ImageLimits::decode(succeeded(&limits_reply, 2)).expect("the negotiable range decodes");
	assert!(limits.minimum >= 2 && limits.maximum >= limits.minimum, "two images is the fewest that can double-buffer");

	// THE SURFACE'S OWN EVENT STREAM, opened before the first frame. The first thing it carries is
	// the configuration, because a client that opened its stream after the surface was made would
	// otherwise wait for a change that may never come.
	let events_reply = call(&console_surface, surface::OP_EVENTS, 3, &[]);
	assert_eq!(le_u32(&events_reply.bytes, 0), 3, "the events reply echoes its correlation id");
	let events_capability = events_reply.caps.first().expect("the surface event stream");
	// A CLIENT ONLY RECEIVES ON ITS EVENT STREAM. `send` would let it forge its own configuration
	// events; `transfer` would let the stream outlive the process the surface belongs to.
	let events_rights = events_capability.rights();
	assert!(events_rights.contains(Rights::RECEIVE | Rights::WAIT), "an event stream can be read and waited on");
	assert!(!events_rights.contains(Rights::SEND) && !events_rights.contains(Rights::TRANSFER), "and cannot be written to or handed on");
	let console_events = events_capability.object().into_any_arc().downcast::<Channel>().expect("an event stream is a channel");
	let opening = configure_event(&console_events);
	assert!(opening.visible && opening.focused, "the first surface takes the screen");

	let console_configuration = adopt(&console_surface, 4);
	assert_eq!((console_configuration.logical_extent.width, console_configuration.logical_extent.height), (4, 4), "a native surface is the scanout's size");
	assert_eq!((console_configuration.physical_extent.width, console_configuration.physical_extent.height), (4, 4), "and the physical extent is the authoritative one");
	assert_eq!((console_configuration.scale.numerator, console_configuration.scale.denominator), (1, 1), "the scale is an exact ratio and never a float on the wire");
	let (console_queue, _console_producer, console_done) = present_queue(&console_surface, 6);
	assert_eq!(console_queue.images, 2, "the service answers with the count it gave");
	assert_eq!(console_queue.pitch, 4 * 4, "four bytes a pixel across four pixels");
	assert_eq!(console_queue.generation, console_configuration.generation, "the queue belongs to the configuration's generation");
	// THE IMAGES ARE THE CLIENT'S: it creates them and pays for them, and this service is charged
	// for none of its clients' pixels. A slot the client has not filled yet is a slot with no
	// memory behind it, so a PARTIALLY supplied queue answers `again` rather than handing out an
	// index that names nothing.
	let console_len: u64 = u64::from(console_queue.pitch) * 4;
	let console_image = provide_image(&console_surface, 7, 0, console_len);
	assert_eq!(acquire_next(&console_surface, 8), display_v1::AcquiredImage::Again, "a partially supplied queue is a queue that cannot be presented from");
	provide_image(&console_surface, 9, 1, console_len);
	// A QUEUE THAT HAS JUST BECOME COMPLETE HAS AN IMAGE TO GIVE, and a client that waits on the
	// event rather than polling `acquire-next` has to be told so - otherwise the `again` above is an
	// answer nothing ever follows.
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::ImageAvailable), "a queue that has just become complete has an image to give");
	// EVERY REFUSAL IS TYPED AND EVERY REFUSAL IS THE WHOLE CALL. An index past the negotiated
	// count, a slot already filled for this generation, an object shorter than the layout needs,
	// and a handle that does not carry what the schema declares are four different ways of not
	// being a presentable image, and none of them is a clamp.
	provide_image_refused(&console_surface, 10, console_queue.images, console_len, Rights::READ | Rights::MAP | Rights::TRANSFER);
	provide_image_refused(&console_surface, 11, 0, console_len, Rights::READ | Rights::MAP | Rights::TRANSFER);
	provide_image_refused(&console_surface, 12, 0, 4, Rights::READ | Rights::MAP | Rights::TRANSFER);
	provide_image_refused(&console_surface, 13, 0, console_len, Rights::READ | Rights::TRANSFER);
	assert_eq!(acquire_next(&console_surface, 14), display_v1::AcquiredImage::Image(0), "a complete queue gives out its first image");
	// BOUNDED COUNTS OF WHAT THE KERNEL CHARGES NOBODY FOR. The images themselves are MemoryObjects
	// charged to the Domain that created them - the client's - and everything reported here is
	// service heap and a service wait set, which is exactly what an adversarial client multiplies.
	let held = display_resources(&stats_root, 16);
	assert_eq!((held.surfaces, held.surface_bound), (1, 64), "one surface, against the service-wide ceiling");
	assert_eq!((held.present_images, held.image_bound), (2, 3), "its two slots, against the most one surface may negotiate");
	assert_eq!((held.queued_presents, held.present_bound), (0, 3), "nothing accepted and not yet settled");
	assert_eq!((held.damage_entries, held.damage_bound), (0, 16), "and no damage retained at all, which is what makes it unmultipliable");
	assert!(held.waiters > 0 && held.waiters <= held.waiter_bound, "the loop waits on something, and inside the kernel's ceiling");
	assert!(!held.faulted, "a service with a scanout is not in the fault state");
	fill(&console_image, 0x0011_2233, 16);
	send_present(&console_surface, 15, 0, &console_configuration, &damage(&[(0, 0, 4, 4)]));
	assert_eq!(device_present(&gpu_kernel, "the console's first present"), alloc::vec![(0, 0, 4, 4)], "a first frame initialises the whole scanout");
	let first_present = present_reply(&console_surface, 15);
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "console pixels reach the scanout");

	// THE COMPLETION GOES BOTH WAYS, which is why there are two completion pairs and not one. The
	// event carries the outcome and the evidence for a client that dispatches; PRESENT_DONE carries
	// the release a frame loop waits on WITHOUT one.
	match surface_event(&console_events) {
		display_v1::SurfaceEvent::PresentComplete(complete) => {
			assert_eq!(complete.serial, first_present, "the completion names the present it settles");
			assert_eq!(complete.outcome, display_v1::PresentOutcome::Displayed, "a driver-completed transfer is `displayed`");
			// A BACKEND THAT CANNOT OBSERVE SCANOUT SAYS SO rather than fabricating a timestamp, and
			// that is what the evidence field is for: this path acknowledges a transfer and a flush,
			// and there is no vblank, no page-flip timing and no proof a scanout happened.
			assert_eq!(complete.evidence, display_v1::TimestampEvidence::Unavailable, "and the evidence says the backend observed nothing");
		}
		other => panic!("expected a present completion, got {other:?}"),
	}
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::ImageAvailable), "a settled present is an image a client may take again");
	let release = console_done.recv().expect("PRESENT_DONE carries the release a frame loop waits on");
	assert_eq!(le_u64(&release.bytes, 0), first_present, "the release names the present it settles");
	assert_eq!(le_u32(&release.bytes, 8), 0, "and the image it gives back");

	// THE DEVICE'S OWN EVENT STREAM, which the service opened when it adopted this device: a resize
	// arrives there rather than on the channel a present is answered on.
	let device_events = device_stream.expect("DisplayService opened the device's event stream");
	let mut resize_frame = [0u8; 64];
	let mut resize_handles = wire::Handles::new();
	let resized = display_device::DeviceEvent::Resized(display_device::Extent2d { width: 4, height: 4 });
	let resize_len = display_device::display_device::events_frame(0, &resized, &mut resize_frame, &mut resize_handles).expect("a device event encodes");
	device_events.send(Message::new(resize_frame[..resize_len].to_vec(), alloc::vec::Vec::new())).expect("gpu resize event");
	device_present(&gpu_kernel, "the repaint after a device resize");
	// A NEW CONFIGURATION IS A NEW SERIAL, AND THE ACKNOWLEDGEMENT IS SPENT WITH IT. A service that
	// carried the old acknowledgement forward would let a client present frames it drew before it
	// was told anything had changed.
	let reconfigured = configure_event(&console_events);
	assert_eq!((reconfigured.logical_extent.width, reconfigured.logical_extent.height), (4, 4), "the native surface follows the scanout");
	assert!(reconfigured.serial > console_configuration.serial, "a reconfiguration is a new serial");
	assert_eq!(acquire_next(&console_surface, 12), display_v1::AcquiredImage::OutOfDate, "an unacknowledged configuration is exactly what `out-of-date` means");
	let stale = call(&console_surface, surface::OP_ACK_CONFIGURE, 13, &console_configuration.serial.to_le_bytes());
	refused(&stale, 13);
	let console_configuration = adopt(&console_surface, 14);
	assert_eq!(console_configuration.serial, reconfigured.serial, "and the acknowledgement names the serial the event carried");

	// A LATER CLIENT TAKES THE SCREEN, and closing its surface gives it back.
	let app = connect(&console_client);
	let app_surface = create_surface(&app, &focus_input, b"SET", 20, 2, 2, 2);
	screen_changed_hands(&console_events, false);
	let app_configuration = adopt(&app_surface, 22);
	assert!(app_configuration.visible && app_configuration.focused, "the newest surface is the visible one");
	assert_eq!((app_configuration.logical_extent.width, app_configuration.logical_extent.height), (2, 2), "a fixed-size surface keeps the size it asked for");

	// THE FOCUS PROOF IS ONE-SHOT AND BELONGS TO THE SURFACE. A client transfers it to
	// `input.subscribe-keys` once; a second ask has nothing left to give.
	let proof = call(&app_surface, surface::OP_INPUT_FOCUS, 24, &[]);
	succeeded(&proof, 24);
	assert_eq!(proof.caps.len(), 1, "the focus proof is transferred out of band");
	// THE ONE CAPABILITY HERE THAT IS MEANT TO TRAVEL, and it carries exactly what travelling needs:
	// its whole purpose is to be handed to `input.subscribe-keys`, so it keeps `transfer` and
	// nothing beyond what a one-shot proof needs to be sent once.
	let proof_rights = proof.caps[0].rights();
	assert!(proof_rights.contains(Rights::SEND | Rights::TRANSFER), "the focus proof can be sent and handed on");
	assert!(!proof_rights.contains(Rights::DUPLICATE), "and never copied, which is what makes it one-shot at all");
	let replayed = call(&app_surface, surface::OP_INPUT_FOCUS, 25, &[]);
	refused(&replayed, 25);
	// AND A SURFACE WITHOUT FOCUS HAS NO PROOF TO ASK FOR, which is the half that makes the proof
	// worth anything: a background client that could mint one could read the foreground's keys.
	let background = call(&console_surface, surface::OP_INPUT_FOCUS, 26, &[]);
	refused(&background, 26);

	let (app_queue, _app_producer, _app_done) = present_queue(&app_surface, 27);
	let app_image = provide_queue(&app_surface, 28, &app_queue, 2);
	assert_eq!(acquire_next(&app_surface, 29), display_v1::AcquiredImage::Image(0), "a visible acknowledged surface has an image to give");
	fill(&app_image, 0x00aa_bbcc, 4);
	send_present(&app_surface, 30, 0, &app_configuration, &damage(&[(0, 0, 1, 1)]));
	let first_scaled = device_present(&gpu_kernel, "the app's first scaled present");
	assert_eq!(first_scaled, alloc::vec![(0, 0, 4, 4)], "a first present initialises the whole scanout");
	present_reply(&app_surface, 30);
	assert_eq!(scanout_pixel(&scanout), 0x00aa_bbcc, "the foreground app replaces the console");
	assert_eq!(scanout_pixel_at(&scanout, 3, 3), 0x00aa_bbcc, "first small damage cannot leak the previous console outside its rectangle");

	let before_damage = display_stats(&display_admin_channel, 31);
	assert_eq!(acquire_next(&app_surface, 32), display_v1::AcquiredImage::Image(0), "a settled present gives its image back");
	set_surface_pixel(&app_image, 0, 0x0055_6677);
	send_present(&app_surface, 33, 0, &app_configuration, &damage(&[(0, 0, 1, 1)]));
	assert_eq!(device_present(&gpu_kernel, "the incremental scaled damage"), alloc::vec![(0, 0, 2, 2)], "scaled damage maps to its conservative output rectangle");
	present_reply(&app_surface, 33);
	assert_eq!(scanout_pixel_at(&scanout, 0, 0), 0x0055_6677);
	assert_eq!(scanout_pixel_at(&scanout, 1, 1), 0x0055_6677);
	assert_eq!(scanout_pixel_at(&scanout, 2, 2), 0x00aa_bbcc, "scaled damage leaves unaffected output pixels unchanged");
	let after_damage = display_stats(&display_admin_channel, 34);
	assert_eq!(after_damage.scaled_presents - before_damage.scaled_presents, 1, "one additional scaled present");
	assert_eq!(after_damage.source_pixels - before_damage.source_pixels, 1, "one source damage pixel");
	assert_eq!(after_damage.output_pixels - before_damage.output_pixels, 4, "only four scaled output pixels written");
	assert!(after_damage.max_present_ns != 0, "present latency is measured in nanoseconds");
	// ONE COUNTER PER OUTCOME, so a client's own report and the service's can be COMPARED rather
	// than argued about: a single `presents` counter cannot say which frames reached a screen.
	assert_eq!(after_damage.displayed - before_damage.displayed, 1, "and the outcome is counted per present");

	// A SECOND SURFACE IS A SECOND ROW, on a connection of its own.
	let held = display_resources(&stats_root, 39);
	assert_eq!(held.surfaces, 2, "the console's surface and the app's");
	assert_eq!(held.present_images, 4, "two slots each");

	// TWO CORNERS ARE TWO TRANSFERS AND NOT THEIR BOUNDING BOX, which is the whole reason damage is a
	// list. The client's surface is 2x2 scaled onto a 4x4 scanout, so the two opposite corners have a
	// bounding box of the entire surface: a service that unioned them would send one transfer of four
	// source pixels, and the assertions below would both fail.
	let before_list = display_stats(&display_admin_channel, 35);
	assert_eq!(acquire_next(&app_surface, 36), display_v1::AcquiredImage::Image(0), "a settled present gives its image back");
	set_surface_pixel(&app_image, 0, 0x0011_2233);
	set_surface_pixel(&app_image, 3, 0x0044_5566);
	send_present(&app_surface, 37, 0, &app_configuration, &damage(&[(0, 0, 1, 1), (1, 1, 1, 1)]));
	// ONE PRESENT ON THE DEVICE WIRE CARRYING TWO RECTANGLES, and their union is the whole scanout: a
	// service that merged them would send `(0, 0, 4, 4)` here and four source pixels below.
	assert_eq!(device_present(&gpu_kernel, "the two corners"), alloc::vec![(0, 0, 2, 2), (2, 2, 2, 2)], "both corners are transferred, each on its own");
	present_reply(&app_surface, 37);
	let after_list = display_stats(&display_admin_channel, 38);
	assert_eq!(after_list.presents - before_list.presents, 1, "two rectangles are ONE present");
	assert_eq!(after_list.source_pixels - before_list.source_pixels, 2, "and two source pixels, not the four of their bounding box");
	assert_eq!(scanout_pixel_at(&scanout, 0, 0), 0x0011_2233);
	assert_eq!(scanout_pixel_at(&scanout, 2, 2), 0x0044_5566);
	assert_eq!(scanout_pixel_at(&scanout, 2, 0), 0x00aa_bbcc, "the pixels between the two corners are not touched");

	// AN EMPTY LIST IS "NOTHING CHANGED": ordered, completed, and nothing copied. A service that
	// answered an error would make the profile's own answer a failure every client has to work
	// around, and one that transferred the surface would make it the most expensive present there is.
	assert_eq!(acquire_next(&app_surface, 39), display_v1::AcquiredImage::Image(0), "a settled present gives its image back");
	send_present(&app_surface, 40, 0, &app_configuration, &damage(&[]));
	let empty = app_surface.recv().expect("an empty present still completes");
	succeeded(&empty, 40);
	assert!(gpu_kernel.recv().is_err(), "and nothing was transferred");
	let after_empty = display_stats(&display_admin_channel, 41);
	assert_eq!(after_empty.presents - after_list.presents, 1, "the present is still counted");
	assert_eq!(after_empty.source_pixels - after_list.source_pixels, 0, "and it carried no pixels");

	// A RECTANGLE OUTSIDE THE EXTENT IS A TYPED REFUSAL AND NEVER A CLAMP, and a frame whose second
	// rectangle is out of bounds presents NONE of it rather than half.
	assert_eq!(acquire_next(&app_surface, 42), display_v1::AcquiredImage::Image(0), "a settled present gives its image back");
	send_present(&app_surface, 43, 0, &app_configuration, &damage(&[(0, 0, 1, 1), (1, 1, 2, 2)]));
	let out_of_bounds = app_surface.recv().expect("a refusal is still a reply");
	refused(&out_of_bounds, 43);
	assert!(gpu_kernel.recv().is_err(), "and the rectangle that WAS valid was not presented either");
	// A FRAME DRAWN FOR A GENERATION THAT HAS MOVED ON IS REFUSED FOR THE SAME REASON.
	let wrong_generation = display_v1::SurfaceConfiguration { generation: app_configuration.generation + 1, ..app_configuration.clone() };
	send_present(&app_surface, 44, 0, &wrong_generation, &damage(&[(0, 0, 1, 1)]));
	refused(&app_surface.recv().expect("a refusal is still a reply"), 44);
	// AND THE REFUSED FRAME'S IMAGE IS STILL THE CLIENT'S, which is what makes a refusal
	// recoverable: `abandon` is the way back that is not a present, and a resize without it leaks an
	// image per resize.
	let abandoned = call(&app_surface, surface::OP_ABANDON, 45, &0u32.to_le_bytes());
	succeeded(&abandoned, 45);
	let twice = call(&app_surface, surface::OP_ABANDON, 46, &0u32.to_le_bytes());
	refused(&twice, 46);

	// CLOSING A SURFACE RESTORES THE CONSOLE, AND THE ANSWER GOES OUT BEFORE THE SURFACE DOES. The
	// reply travels on the very channel the teardown closes, so a service that tore down inside the
	// handler left the client waiting for an answer it had already made unsendable.
	app_surface.send(request(surface::OP_CLOSE, 47, &[])).expect("a close request");
	sched::run_until_idle();
	let closed = app_surface.recv().expect("close is answered before the surface goes");
	succeeded(&closed, 47);
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the close restore");
	screen_changed_hands(&console_events, true);
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "closing the foreground surface restores the console");

	// The private emergency command revokes a frozen foreground display connection.
	let process = Process::new(AddressSpace::create().expect("bound process address space"), sched::root_domain()).expect("a test process");
	let mut bind = alloc::vec::Vec::new();
	bind.extend_from_slice(&display_admin::OP_BIND.to_le_bytes());
	bind.extend_from_slice(&50u32.to_le_bytes());
	bind.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&display_admin_channel, &bind, process.clone(), Rights::MANAGE | Rights::WAIT | Rights::TRANSFER).expect("bind process to display connection");
	sched::run_until_idle();
	let bind_reply = display_admin_channel.recv().expect("bound display reply");
	succeeded(&bind_reply, 50);
	let frozen = bind_reply.caps.first().expect("bound display connection").object().into_any_arc().downcast::<Channel>().expect("bound display is a channel");
	frozen.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new())).expect("bound factory escape attempt");
	sched::run_until_idle();
	assert!(frozen.recv().is_err(), "process-bound display connection cannot mint an unbound child");
	let frozen_surface = create_surface(&frozen, &focus_input, b"SET", 51, 2, 2, 2);
	screen_changed_hands(&console_events, false);
	let frozen_configuration = adopt(&frozen_surface, 52);
	let (frozen_queue, _frozen_producer, _frozen_done) = present_queue(&frozen_surface, 54);
	let frozen_image = provide_queue(&frozen_surface, 55, &frozen_queue, 2);
	assert_eq!(acquire_next(&frozen_surface, 56), display_v1::AcquiredImage::Image(0), "a visible acknowledged surface has an image to give");
	fill(&frozen_image, 0x0000_77dd, 4);
	send_present(&frozen_surface, 57, 0, &frozen_configuration, &damage(&[(0, 0, 2, 2)]));
	device_present(&gpu_kernel, "the frozen app");
	present_reply(&frozen_surface, 57);
	kill_input.send(Message::new(b"KILL".to_vec(), alloc::vec::Vec::new())).expect("emergency display revoke");
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the frozen console restore");
	screen_changed_hands(&console_events, true);
	assert!(frozen.is_peer_closed(), "emergency revoke closes the foreground display connection");
	assert!(process.is_killed(), "emergency revoke SIG_KILLs the process bound by PermissionManager");
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "emergency revoke restores the console surface");

	// CLEANUP IS PROVED INDEPENDENTLY OF CHANNEL PEER LIFETIME, which is the half a channel cannot
	// answer: the connection below stays OPEN for the whole of this because this harness holds its
	// peer, so nothing about the channel says the client is gone. What says so is the bound TASK,
	// which this service watches - and without that watch a dead client's imported images stay
	// mapped for as long as somebody else keeps a channel open.
	let orphan_process = Process::new(AddressSpace::create().expect("bound process address space"), sched::root_domain()).expect("a second test process");
	let mut orphan_bind = alloc::vec::Vec::new();
	orphan_bind.extend_from_slice(&display_admin::OP_BIND.to_le_bytes());
	orphan_bind.extend_from_slice(&58u32.to_le_bytes());
	orphan_bind.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&display_admin_channel, &orphan_bind, orphan_process.clone(), Rights::MANAGE | Rights::WAIT | Rights::TRANSFER).expect("bind a second process to a display connection");
	sched::run_until_idle();
	let orphan_reply = display_admin_channel.recv().expect("a bound display reply");
	succeeded(&orphan_reply, 58);
	let orphan = orphan_reply.caps.first().expect("the bound display connection").object().into_any_arc().downcast::<Channel>().expect("bound display is a channel");
	let orphan_surface = create_surface(&orphan, &focus_input, b"SET", 59, 2, 2, 2);
	screen_changed_hands(&console_events, false);
	let orphan_configuration = adopt(&orphan_surface, 90);
	let (orphan_queue, _orphan_producer, _orphan_done) = present_queue(&orphan_surface, 92);
	provide_queue(&orphan_surface, 93, &orphan_queue, 2);
	let before_orphan = display_resources(&stats_root, 96);
	assert_eq!(before_orphan.surfaces, 2, "the console's surface and the bound client's");
	assert_eq!(before_orphan.present_images, 4, "and two slots each");
	let _ = orphan_configuration;
	// THE ONLY THING THAT CHANGES IS THE PROCESS. This harness holds the connection's peer and never
	// lets go of it, so up to this line and past it the channel says nothing about the client.
	assert!(!orphan.is_peer_closed(), "nothing has happened to the connection yet");
	orphan_process.mark_exited();
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the restore after a bound client died");
	screen_changed_hands(&console_events, true);
	// AND THE SERVICE GAVE THE CONNECTION BACK, which is the direction that makes this a different
	// proof from the peer-close below: there, the client's end went and the service noticed; here,
	// this end never moved and the service let go of its own.
	assert!(orphan.is_peer_closed(), "a dead client's connection is released by the service that watched its task");
	let after_orphan = display_resources(&stats_root, 97);
	assert_eq!(after_orphan.surfaces, 1, "a dead client's surfaces go with it, whatever holds its channel");
	assert_eq!(after_orphan.present_images, 2, "and every slot it held is released");
	assert!(orphan_surface.is_peer_closed(), "and the surface capability it left behind is dead");

	// A crashed client has the same restoration guarantee through channel peer-close.
	let crashed = connect(&console_client);
	let crashed_surface = create_surface(&crashed, &focus_input, b"SET", 60, 2, 2, 2);
	screen_changed_hands(&console_events, false);
	let crashed_configuration = adopt(&crashed_surface, 61);
	let (crashed_queue, _crashed_producer, _crashed_done) = present_queue(&crashed_surface, 63);
	let crashed_image = provide_queue(&crashed_surface, 64, &crashed_queue, 2);
	assert_eq!(acquire_next(&crashed_surface, 65), display_v1::AcquiredImage::Image(0), "a visible acknowledged surface has an image to give");
	fill(&crashed_image, 0x00dd_4400, 4);
	send_present(&crashed_surface, 66, 0, &crashed_configuration, &damage(&[(0, 0, 2, 2)]));
	device_present(&gpu_kernel, "the crashed app");
	present_reply(&crashed_surface, 66);
	assert_eq!(scanout_pixel(&scanout), 0x00dd_4400, "second foreground app reaches scanout");
	drop(crashed);
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the crashed console restore");
	screen_changed_hands(&console_events, true);
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "peer-close restores the console surface");
	// AND A SURFACE WHOSE CONNECTION WENT AWAY IS GONE WITH IT, which is what makes a crashed
	// application's windows disappear rather than freeze.
	assert!(crashed_surface.is_peer_closed(), "a connection going away takes its surfaces with it");

	// Game-class benchmark geometry: replace the stand-in scanout with 1024x768,
	// present a 320x200 software surface, then update a 32x20 source rectangle. The
	// service's own monotonic counters separate CPU scaling from driver ACK latency.
	let large_scanout = match DmaBuffer::create_in(&sched::root_domain(), 1024 * 768 * 4) {
		Ok(scanout) => scanout,
		Err(_) => panic!("large stand-in scanout"),
	};
	// A REPLACEMENT IS AN EVENT ON THE DEVICE STREAM, carrying the new backing and the generation it
	// belongs to. It used to be an `FBNEW` byte message on the channel a present is answered on, which
	// is the arrangement the typed wire exists to end: a driver that replaced its backing while a
	// present was in flight had its replacement read as that present's answer.
	let replaced = display_device::DeviceEvent::Replaced(display_device::Scanout { backing: wire::Buffer { handle: 0, len: 1024 * 768 * 4 }, layout: display_device::ImageLayout { size: display_device::Extent2d { width: 1024, height: 768 }, pitch: 4096, format: graphics_proto::generated::liber::graphics::v1::PixelFormat::B8g8r8x8Unorm, alpha: graphics_proto::generated::liber::graphics::v1::AlphaMode::Opaque, color_space: graphics_proto::generated::liber::graphics::v1::ColorSpace::Srgb, origin: graphics_proto::generated::liber::graphics::v1::RowOrigin::TopLeft }, visible: display_device::Extent2d { width: 1024, height: 768 }, generation: 2 });
	let mut replacement_frame = [0u8; 128];
	let mut replacement_handles = wire::Handles::new();
	let replacement_len = display_device::display_device::events_frame(1, &replaced, &mut replacement_frame, &mut replacement_handles).expect("a replacement event encodes");
	send_cap(&device_events, &replacement_frame[..replacement_len], large_scanout.clone(), Rights::READ | Rights::WRITE | Rights::MAP | Rights::TRANSFER).expect("large framebuffer replacement");
	sched::run_until_idle();
	// A CHANGED EXTENT IS A NEW GENERATION AND NOT MERELY A NEW SERIAL, and A NEW GENERATION HAS NO
	// IMAGES: they were the CLIENT's, and none of them is ever presented into a size it was not
	// drawn for. So NOTHING reaches the device until the client has rebuilt - which is `out-of-date`
	// doing exactly what it says, rather than a service repainting from memory it has given back.
	assert!(gpu_kernel.recv().is_err(), "a generation change presents nothing until the client has rebuilt");
	let regenerated = configure_event(&console_events);
	assert_eq!((regenerated.logical_extent.width, regenerated.logical_extent.height), (1024, 768));
	assert!(regenerated.generation > console_configuration.generation, "a new extent is a new generation");
	assert_eq!(acquire_next(&console_surface, 68), display_v1::AcquiredImage::OutOfDate, "and the client is told to rebuild rather than handed a stale image");

	// THE REBUILD, WHICH IS THE SAME LIFECYCLE RUNNING A SECOND TIME on a surface that already
	// exists: `configure -> rebuild -> ack -> first present Full`. The images are new because the
	// generation is, and the old ones went back to the Domain that created them.
	let console_configuration = adopt(&console_surface, 69);
	assert_eq!(console_configuration.generation, regenerated.generation, "the acknowledgement belongs to the new generation");
	let (console_queue, _rebuilt_producer, _rebuilt_done) = present_queue(&console_surface, 71);
	assert_eq!(console_queue.generation, regenerated.generation, "and so does the queue");
	assert_eq!(console_queue.pitch, 1024 * 4, "whose pitch is the new extent's");
	let console_image = provide_queue(&console_surface, 72, &console_queue, 768);
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::ImageAvailable), "a rebuilt queue that is complete has an image to give");
	assert_eq!(acquire_next(&console_surface, 75), display_v1::AcquiredImage::Image(0), "and the rebuilt surface can draw again");
	fill(&console_image, 0x0000_2244, 1024 * 768);
	send_present(&console_surface, 76, 0, &console_configuration, &damage(&[(0, 0, 1024, 768)]));
	device_present(&gpu_kernel, "the console's first frame of the new generation");
	present_reply(&console_surface, 76);
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::PresentComplete(_)), "the rebuilt surface's frame completes");
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::ImageAvailable), "and its image comes back");
	assert_eq!(scanout_pixel_at(&large_scanout, 0, 0), 0x0000_2244, "the rebuilt console reaches the replaced scanout");

	let benchmark = connect(&console_client);
	let benchmark_surface = create_surface(&benchmark, &focus_input, b"SET", 70, 320, 200, 2);
	screen_changed_hands(&console_events, false);
	let benchmark_configuration = adopt(&benchmark_surface, 71);
	let (benchmark_queue, _benchmark_producer, _benchmark_done) = present_queue(&benchmark_surface, 73);
	let benchmark_image = provide_queue(&benchmark_surface, 74, &benchmark_queue, 200);
	assert_eq!(acquire_next(&benchmark_surface, 75), display_v1::AcquiredImage::Image(0), "a visible acknowledged surface has an image to give");
	fill(&benchmark_image, 0x0033_6699, 320 * 200);
	let before_full = display_stats(&display_admin_channel, 76);
	send_present(&benchmark_surface, 77, 0, &benchmark_configuration, &damage(&[(0, 0, 320, 200)]));
	device_present(&gpu_kernel, "the benchmark full present");
	present_reply(&benchmark_surface, 77);
	let after_full = display_stats(&display_admin_channel, 78);
	assert_eq!(acquire_next(&benchmark_surface, 79), display_v1::AcquiredImage::Image(0), "a settled present gives its image back");
	send_present(&benchmark_surface, 80, 0, &benchmark_configuration, &damage(&[(32, 20, 32, 20)]));
	device_present(&gpu_kernel, "the benchmark damage present");
	present_reply(&benchmark_surface, 80);
	let after_benchmark = display_stats(&display_admin_channel, 81);
	let full_blit_ns = after_full.blit_ns - before_full.blit_ns;
	let full_flush_ns = after_full.flush_ns - before_full.flush_ns;
	let full_pixels = after_full.output_pixels - before_full.output_pixels;
	let damage_blit_ns = after_benchmark.blit_ns - after_full.blit_ns;
	let damage_flush_ns = after_benchmark.flush_ns - after_full.flush_ns;
	let damage_pixels = after_benchmark.output_pixels - after_full.output_pixels;
	crate::serial_println!("display-perf: full blit={}ns flush={}ns pixels={} damage blit={}ns flush={}ns pixels={}", full_blit_ns, full_flush_ns, full_pixels, damage_blit_ns, damage_flush_ns, damage_pixels);
	assert_eq!(full_pixels, 1024 * 768 + 1024 * 640, "first scaled frame clears scanout and fills centered output");
	assert_eq!(damage_pixels, 103 * 64, "32x20 source damage maps to a 103x64 conservative output rectangle");
	assert!(damage_blit_ns < full_blit_ns, "incremental scaled damage must cost less CPU time than a full first frame");
	benchmark_surface.send(request(surface::OP_CLOSE, 82, &[])).expect("benchmark close");
	sched::run_until_idle();
	succeeded(&benchmark_surface.recv().expect("close is answered before the surface goes"), 82);
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the benchmark close restore");
	screen_changed_hands(&console_events, true);
	assert_eq!(scanout_pixel_at(&large_scanout, 0, 0), 0x0000_2244, "closing the benchmark restores the rebuilt console");
	// AND WHAT THE SERVICE HOLDS CAME BACK DOWN WITH THEM. A count that only rose would be a service
	// that leaks a surface per client, which is the thing a bounded count exists to make visible.
	let held = display_resources(&stats_root, 95);
	assert_eq!(held.surfaces, 1, "every surface but the console's has gone");
	assert_eq!(held.present_images, 2, "and so has every slot they held");
	assert_eq!(held.queued_presents, 0, "with nothing left in flight");
}

// WHAT A CLIENT CAN SEND THAT IS NOT A FRAME, and what a display service owes each of it.
//
// THE OTHER DISPLAY TEST IS ABOUT THE HAPPY PATH BEING RIGHT and this one is about the rest of the
// input space being REFUSED rather than half-acted-on. Every case below is a shape a hostile or
// simply broken client can produce with no privilege at all: a malformed extent, a truncated
// message, a capability of the wrong kind where an image belongs, an image nobody acquired, a frame
// drawn for a configuration that has moved on, more surfaces than one connection may own, a close
// while a frame is at the driver, and the driver going away underneath everything.
//
// THE PROPERTY THEY SHARE IS THAT NONE OF THEM ENDS THE SERVICE and none of them takes the screen
// from the console. A refusal that killed the loop would be a denial of service any client could
// perform, and a refusal that left the scanout blank would be the same thing seen from the other
// side.
tagged_test!(display_service_refuses_hostile_input, [Service, Display], id = "kernel.services.display_service_refuses_hostile_input", covers = ["kernel", "bin.display_service"]);
fn display_service_refuses_hostile_input() {
	use display_harness::*;

	let harness = display_harness::start(4, 4);
	let console_client = harness.console;
	let focus_input = harness.focus;
	let stats_root = harness.stats;
	let gpu_kernel = harness.gpu;
	let scanout = harness.scanout;
	let _display_service = harness.service;
	let _boot_kernel = harness.boot;
	let _kill_input = harness.kill;
	let _admin = harness.admin;
	let _device_events = harness.device_events;

	// AN EXTENT WITH ONE AXIS ZERO IS NOT A REQUEST FOR THE NATIVE SIZE. `(0, 0)` asks for the
	// server's preferred one; a zero on ONE axis is a client that computed a size wrongly, and
	// answering it with a guess would be inventing the half it got wrong.
	create_surface_refused(&console_client, 1, 0, 16, 2);
	create_surface_refused(&console_client, 2, 16, 0, 2);
	// AND AN EXTENT PAST WHAT THIS SERVICE WILL HOLD IS REFUSED RATHER THAN CLAMPED, for the reason
	// every other clamp in this contract is refused: a surface silently smaller than the one asked
	// for is a client drawing off the end of its own buffer.
	create_surface_refused(&console_client, 3, 100_000, 100_000, 2);

	let console_surface = create_surface(&console_client, &focus_input, b"CONSOLE", 4, 0, 0, 2);
	let console_configuration = adopt(&console_surface, 5);
	let (console_queue, _console_producer, _console_done) = present_queue(&console_surface, 7);
	let console_image = provide_queue(&console_surface, 8, &console_queue, 4);

	// A TRUNCATED MESSAGE IS NOT A REQUEST. The op is there and the correlation is not, so there is
	// nothing to answer TO - a service that invented a correlation would be answering a call nobody
	// made, and one that died here would be killable by two bytes.
	console_surface.send(Message::new(surface::OP_CONFIGURATION.to_le_bytes().to_vec(), alloc::vec::Vec::new())).expect("a truncated request");
	sched::run_until_idle();
	assert!(console_surface.recv().is_err(), "a message too short to name a call is not answered");
	// A TRAILING BYTE IS THE SAME MISTAKE FROM THE OTHER END: `[op][corr]` and `[op][corr][junk]`
	// are not the same request, and a decoder that stopped at the fields it knew would accept both.
	let mut trailing = request(surface::OP_CONFIGURATION, 9, &[]).bytes;
	trailing.push(0xde);
	console_surface.send(Message::new(trailing, alloc::vec::Vec::new())).expect("a request with a trailing byte");
	sched::run_until_idle();
	assert!(console_surface.recv().is_err(), "nor is one whose writer and reader disagree about its length");
	// AND THE SERVICE IS STILL THERE, which is the half that makes the two above interesting.
	let after_junk = call(&console_surface, surface::OP_CONFIGURATION, 10, &[]);
	succeeded(&after_junk, 10);

	// A REQUEST THAT CARRIES A CAPABILITY ITS SIGNATURE DOES NOT NAME. The defect this is about is
	// not the refusal but what happens to the HANDLE: a service that decoded the bytes it understood
	// and ignored the rest would leave a live capability in nobody's hands and nobody's list - not
	// refused, not closed, and still charged to the sender for the life of the process, one per
	// request, from any client with no privilege at all.
	let (smuggled, smuggled_peer) = Channel::create();
	let mut smuggle = 1u64.to_le_bytes().to_vec();
	send_cap(&console_surface, &request(surface::OP_ACK_CONFIGURE, 19, &smuggle).bytes, smuggled_peer, Rights::ALL).expect("a request carrying an unnamed capability");
	sched::run_until_idle();
	assert!(console_surface.recv().is_err(), "a request whose signature does not account for what it carried is not answered");
	// AND THE CAPABILITY WAS CLOSED RATHER THAN KEPT. The service's own sweep closes what its
	// dispatch refused, so the peer this test still holds sees the other end go.
	assert!(smuggled.is_peer_closed(), "and the capability it smuggled in was released rather than held");
	smuggle.clear();

	// A FORGED IMPORT: a capability of the wrong KIND where an image belongs. The schema declares
	// `handle<image-object>` with `@kernel(memory-object)`, so the generated guard checks the object
	// type as well as the rights - a channel is refused before the service sees it at all.
	let (forged, forged_peer) = Channel::create();
	let mut forged_body = 0u32.to_le_bytes().to_vec();
	forged_body.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&console_surface, &request(surface::OP_PROVIDE_IMAGE, 11, &forged_body).bytes, forged_peer, Rights::ALL).expect("a forged import");
	sched::run_until_idle();
	refused(&console_surface.recv().expect("a refusal is still a reply"), 11);
	drop(forged);

	// DUPLICATE ACQUIRE IS NOT A REFUSAL, IT IS THE QUEUE RUNNING OUT. Two images means two
	// acquires; the third has nothing to give and says so without blocking, which is the property
	// the whole non-blocking rule exists for - a service that blocked here would stop the only loop
	// that could deliver the release that would unblock it.
	assert_eq!(acquire_next(&console_surface, 12), display_v1::AcquiredImage::Image(0), "the first image");
	assert_eq!(acquire_next(&console_surface, 13), display_v1::AcquiredImage::Image(1), "and the second");
	assert_eq!(acquire_next(&console_surface, 14), display_v1::AcquiredImage::Again, "and then there is nothing to give");

	// AN IMAGE NOBODY ACQUIRED CANNOT BE PRESENTED, and an index past the queue is not an index.
	send_present(&console_surface, 15, console_queue.images, &console_configuration, &damage(&[(0, 0, 4, 4)]));
	refused(&console_surface.recv().expect("a refusal is still a reply"), 15);
	// A FRAME DRAWN FOR A CONFIGURATION THAT WAS NEVER ACKNOWLEDGED IS REFUSED, whatever it holds.
	let unacknowledged = display_v1::SurfaceConfiguration { serial: console_configuration.serial + 1, ..console_configuration.clone() };
	send_present(&console_surface, 16, 0, &unacknowledged, &damage(&[(0, 0, 4, 4)]));
	refused(&console_surface.recv().expect("a refusal is still a reply"), 16);
	assert!(gpu_kernel.recv().is_err(), "and none of the refusals reached the device");

	fill(&console_image, 0x0011_2233, 16);
	send_present(&console_surface, 17, 0, &console_configuration, &damage(&[(0, 0, 4, 4)]));
	device_present(&gpu_kernel, "the console's only good frame");
	present_reply(&console_surface, 17);
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "which is the one frame that reaches the scanout");
	// AND PRESENTING THE SAME IMAGE AGAIN IS REFUSED, because a present CONSUMES the image: it went
	// back to the queue when it settled, and what the client holds is an index and not a claim.
	send_present(&console_surface, 18, 0, &console_configuration, &damage(&[(0, 0, 4, 4)]));
	refused(&console_surface.recv().expect("a refusal is still a reply"), 18);

	// A BACKGROUND SURFACE IS TOLD SO RATHER THAN REFUSED, and cannot mint the proof that would let
	// it read the foreground's keys. A client that could would be a keylogger with no privilege.
	let intruder = connect(&console_client);
	let intruder_surface = create_surface(&intruder, &focus_input, b"SET", 20, 2, 2, 2);
	assert_eq!(acquire_next(&console_surface, 22), display_v1::AcquiredImage::NotVisible, "a surface that is not the visible one has nothing to draw into");
	refused(&call(&console_surface, surface::OP_INPUT_FOCUS, 23, &[]), 23);

	// AND FOREGROUND INPUT CANNOT BE RETAINED. The proof is minted for the surface that HAS the
	// screen and is one-shot; a surface that had it and lost it is refused, which is what stops a
	// window that was once in front from going on reading the keyboard behind the one that is.
	succeeded(&call(&intruder_surface, surface::OP_INPUT_FOCUS, 24, &[]), 24);
	let mut held: alloc::vec::Vec<Arc<Channel>> = alloc::vec::Vec::new();
	held.push(create_surface(&intruder, &focus_input, b"SET", 25, 2, 2, 2));
	refused(&call(&intruder_surface, surface::OP_INPUT_FOCUS, 26, &[]), 26);

	// ONE CONNECTION MAY OWN A STATED NUMBER OF SURFACES AND NOT MORE, and the refusal is typed and
	// leaves nothing behind: these are service heap and a service wait set, charged to nobody, which
	// is exactly what an adversarial client multiplies.
	for index in 0..14u32 {
		held.push(create_surface(&intruder, &focus_input, b"SET", 30 + index, 2, 2, 2));
	}
	create_surface_refused(&intruder, 60, 2, 2, 2);
	let crowded = display_resources(&stats_root, 61);
	assert_eq!(crowded.surfaces, 17, "the console's surface and one connection's sixteen");
	assert!(crowded.surfaces <= crowded.surface_bound, "and the service-wide ceiling is not passed either");
	// AND THE CONNECTION GOING AWAY TAKES ALL SIXTEEN WITH IT, which is the half that makes the
	// bound a bound rather than a one-way ratchet.
	drop(held);
	drop(intruder_surface);
	drop(intruder);
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the restore after the crowded connection went");
	let emptied = display_resources(&stats_root, 62);
	assert_eq!(emptied.surfaces, 1, "every surface that connection owned is gone");
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "and the console is on the screen, not a blank scanout");

	// A CLOSE WHILE A FRAME IS AT THE DRIVER. The service is inside its own flush when the close
	// arrives, so the two cannot be reordered by the client: the present is answered, then the
	// surface goes. A service that tore down inside the present would be answering a call on a
	// channel it had just closed.
	let racer = connect(&console_client);
	let racer_surface = create_surface(&racer, &focus_input, b"SET", 70, 2, 2, 2);
	let racer_configuration = adopt(&racer_surface, 71);
	let (racer_queue, _racer_producer, _racer_done) = present_queue(&racer_surface, 73);
	let racer_image = provide_queue(&racer_surface, 74, &racer_queue, 2);
	assert_eq!(acquire_next(&racer_surface, 76), display_v1::AcquiredImage::Image(0), "a visible acknowledged surface has an image to give");
	fill(&racer_image, 0x0077_0077, 4);
	send_present(&racer_surface, 77, 0, &racer_configuration, &damage(&[(0, 0, 2, 2)]));
	sched::run_until_idle();
	let in_flight = gpu_kernel.recv().expect("the frame reached the device");
	// THE CLOSE IS SENT WHILE THE SERVICE IS BLOCKED ON THAT ANSWER.
	racer_surface.send(request(surface::OP_CLOSE, 78, &[])).expect("a close while a frame is in flight");
	sched::run_until_idle();
	let mut acknowledgement = le_u32(&in_flight.bytes, 2).to_le_bytes().to_vec();
	acknowledgement.push(1);
	gpu_kernel.send(Message::new(acknowledgement, alloc::vec::Vec::new())).expect("the device acknowledges");
	sched::run_until_idle();
	present_reply(&racer_surface, 77);
	succeeded(&racer_surface.recv().expect("and the close is answered too"), 78);
	acknowledge_focus(&focus_input, b"CONSOLE");
	device_present(&gpu_kernel, "the restore after the racing close");
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "the console comes back from under a frame that was in flight");

	// THE DRIVER GOES AWAY UNDER EVERYTHING, WHICH IS THE LAST THING THIS SERVICE SURVIVES.
	//
	// A present from here reaches a mapping nobody is reading. `displayed` would be a report of a
	// frame nobody saw, and `driver-lost` is the outcome the profile grew for exactly this.
	let events_reply = call(&console_surface, surface::OP_EVENTS, 80, &[]);
	assert_eq!(le_u32(&events_reply.bytes, 0), 80, "the events reply echoes its correlation id");
	let console_events = events_reply.caps.first().expect("the surface event stream").object().into_any_arc().downcast::<Channel>().expect("an event stream is a channel");
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::Configure(_)), "a new stream opens with the snapshot");
	drop(gpu_kernel);
	sched::run_until_idle();
	let faulted = display_resources(&stats_root, 81);
	assert!(faulted.faulted, "a service whose driver went away says so");
	assert_eq!(acquire_next(&console_surface, 82), display_v1::AcquiredImage::Image(0), "and still hands out images, because the surface is not what broke");
	send_present(&console_surface, 83, 0, &console_configuration, &damage(&[(0, 0, 4, 4)]));
	present_reply(&console_surface, 83);
	match surface_event(&console_events) {
		display_v1::SurfaceEvent::PresentComplete(complete) => assert_eq!(complete.outcome, display_v1::PresentOutcome::DriverLost, "a frame after the backend went away is `driver-lost`"),
		other => panic!("expected a present completion, got {other:?}"),
	}
	// AND THE PRESENT STILL SETTLED, so a client that waits on completions is not left holding an
	// image for ever because the machine lost its display.
	assert!(matches!(surface_event(&console_events), display_v1::SurfaceEvent::ImageAvailable), "and the image comes back");
	let counted = display_stats(&_admin, 84);
	assert!(counted.lost > 0, "and the outcome is counted rather than only reported");

	// THE OUTPUT'S SCALE IS AN INPUT, AND IT DECIDES AN ALLOCATION.
	//
	// Every surface's physical extent is its logical one times this ratio, and every image a client
	// supplies is that extent's worth of memory - so a ratio nobody bounded is a way to make every
	// client in the machine allocate whatever the caller chose. Zero on either side is not a ratio at
	// all; past the bound is refused by the same rule the maximum dimension is.
	let set_scale = |corr: u32, numerator: u32, denominator: u32| -> Message {
		let mut writer = wire::VecWriter::new();
		display_v1::ScaleRatio { numerator, denominator }.write(&mut writer).expect("a scale ratio encodes");
		let body = writer.into_inner().expect("a scale ratio carries no capability");
		_admin.send(request(display_admin::OP_SET_SCALE, corr, &body)).expect("a set-scale request");
		sched::run_until_idle();
		_admin.recv().expect("a set-scale reply")
	};
	for (index, (numerator, denominator)) in [(0u32, 1u32), (1, 0), (0, 0), (9, 1), (1, 9)].into_iter().enumerate() {
		refused(&set_scale(90 + index as u32, numerator, denominator), 90 + index as u32);
	}
	// AND THE OUTPUT KEPT THE SCALE IT HAD. A refusal that had already reconfigured half the surfaces
	// would be a screen whose windows disagree about what a logical pixel is.
	let unchanged = call(&console_surface, surface::OP_CONFIGURATION, 95, &[]);
	let unchanged = display_v1::SurfaceConfiguration::read(&mut wire::Reader::new(succeeded(&unchanged, 95))).expect("a configuration decodes");
	assert_eq!((unchanged.scale.numerator, unchanged.scale.denominator), (1, 1), "a refused scale changes nothing");
	assert_eq!((unchanged.physical_extent.width, unchanged.physical_extent.height), (4, 4), "and no surface was reconfigured");

	// ONE THAT IS ACCEPTED, AND WHAT IT DOES: the LOGICAL extent is held - a window is the same size
	// on the desk after the scale changes - and the PHYSICAL one becomes that times the ratio, in a
	// new generation, because every image of the old one is the wrong number of pixels now.
	succeeded(&set_scale(96, 2, 1), 96);
	let scaled = loop {
		match surface_event(&console_events) {
			display_v1::SurfaceEvent::Configure(configuration) if configuration.scale.numerator == 2 => break configuration,
			display_v1::SurfaceEvent::Configure(_) => continue,
			other => panic!("expected the scale change's configuration, got {other:?}"),
		}
	};
	assert_eq!((scaled.logical_extent.width, scaled.logical_extent.height), (4, 4), "the logical extent is held across a scale change");
	assert_eq!((scaled.physical_extent.width, scaled.physical_extent.height), (8, 8), "and the physical one is it times the ratio");
	assert_eq!((scaled.scale.numerator, scaled.scale.denominator), (2, 1), "which is what tells a client this was a scale change and not a resize");
	assert!(scaled.generation > console_configuration.generation, "a new generation, because every image of the old one is the wrong size now");
}

// THE FRAME LOOP AN APPLICATION HAS, AGAINST A REAL DISPLAYSERVICE.
//
// THE POLICY IS CHECKED AS ARITHMETIC ELSEWHERE - how many frames may be in flight, when the next one
// is due, what a configuration change means, what to do while hidden - because it is a value with no
// syscalls in it. What CANNOT be checked that way is that the policy is wired to the wire: that the
// loop acquires from a real queue, supplies real images, presents over a real device, notices a real
// resize and a real loss of the screen, and comes back from both.
//
// `frame_probe` is the client. It draws nothing worth looking at - a solid colour that changes per
// frame - because what is under test is the SCHEDULING and not the picture, and it reports the step
// it took each time round so a busy spin or a frame past the negotiated limit is visible in the
// output rather than only in a timing.
tagged_test!(the_frame_loop_paces_a_real_display, [Service, Display, Process], id = "kernel.services.the_frame_loop_paces_a_real_display", covers = ["bin.frame_probe", "graphics-app", "surface"]);
fn the_frame_loop_paces_a_real_display() {
	use display_harness::*;

	// AN EIGHT BY EIGHT SCANOUT, because the resize below shrinks the surface and a device may only
	// report a visible extent its backing holds.
	let harness = display_harness::start(8, 8);
	let console_client = harness.console;
	let focus_input = harness.focus;
	let gpu_kernel = harness.gpu;
	let device_events = harness.device_events.expect("DisplayService opened the device's event stream");
	let _display_service = harness.service;
	let _boot_kernel = harness.boot;
	let _stats = harness.stats;
	let _admin = harness.admin;
	let _scanout = harness.scanout;

	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init_package_bytes().expect("init package module not found")).expect("init package parses");
	let probe_elf = program_elf(&package, volume, b"frame_probe").expect("frame_probe in the package or volume");
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let display = connect(&console_client);
	let process = spawn_dynamic_test_process(sched::root_domain(), probe_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("frame_probe stdout");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("endpoint run terminator");
	bootstrap.send(Message::new(crate::tests::launch_context(b"", b"vol://system"), alloc::vec::Vec::new())).expect("frame_probe args");
	send_cap(&bootstrap, b"DISPLAY", display, Rights::ALL).expect("frame_probe display");

	// The phases this harness drives the probe through, in order: run, take the screen away, give it
	// back, resize under it, then let it finish.
	const RUNNING: u8 = 0;
	const TAKING: u8 = 1;
	const BACKGROUND: u8 = 2;
	const RESTORED: u8 = 3;
	const RESIZED: u8 = 4;

	let mut output: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	let mut presents: usize = 0;
	let mut background_presents: usize = 0;
	let mut phase: u8 = RUNNING;
	let mut quiet: usize = 0;
	let mut thief: Option<Arc<Channel>> = None;
	for _ in 0..40_000u32 {
		// A BOUNDED DRAIN AND NOT AN UNBOUNDED ONE, which is the whole difference between a harness
		// that drives a paced client and one that watches it run to completion. `run_until_idle`
		// sleeps to the nearest THREAD deadline and keeps going, so against a loop that paces itself
		// on a timer it never returns at all: the first call here ran the probe to its own iteration
		// ceiling before this loop saw a second pass.
		sched::run_until_idle_until(arch::apic::ticks().saturating_add(1));
		// THE FOCUS TRANSITIONS, ANSWERED WHEREVER THEY ARRIVE. The service blocks inside its own
		// handler for this acknowledgement, so a harness that answered it only at the points it
		// expected one would wedge the service at the first point it did not.
		// EVERY SOURCE IS SERVED ON EVERY PASS AND NONE OF THEM SHORT-CIRCUITS THE OTHERS. This
		// answered a present and went straight back round, so while the probe was drawing the phase
		// machine below was never reached at all - and the harness drove its resize and its loss of
		// the screen at a program that had already finished.
		let mut worked = false;
		if focus_input.recv().is_ok() {
			focus_input.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("focus acknowledgement");
			worked = true;
		}
		while let Ok(present) = gpu_kernel.recv() {
			let mut reply = le_u32(&present.bytes, 2).to_le_bytes().to_vec();
			reply.push(1);
			gpu_kernel.send(Message::new(reply, alloc::vec::Vec::new())).expect("present acknowledgement");
			presents += 1;
			if phase == BACKGROUND {
				background_presents += 1;
			}
			worked = true;
		}
		while let Ok(message) = stdout.recv() {
			output.extend_from_slice(&message.bytes);
		}
		match phase {
			// TAKE THE SCREEN AWAY once the loop has drawn a few frames through it.
			RUNNING if presents >= 2 => {
				let mut writer = wire::VecWriter::new();
				display_v1::SurfaceRequest { logical_extent: display_v1::Extent2d { width: 2, height: 2 }, images: 2 }.write(&mut writer).expect("a surface request encodes");
				let body = writer.into_inner().expect("a surface request carries no capability");
				console_client.send(request(display::OP_CREATE_SURFACE, 900, &body)).expect("a create-surface request");
				phase = TAKING;
			}
			TAKING => {
				if let Ok(reply) = console_client.recv() {
					succeeded(&reply, 900);
					thief = Some(reply.caps.first().expect("the surface capability").object().into_any_arc().downcast::<Channel>().expect("a surface is a channel"));
					phase = BACKGROUND;
					quiet = 0;
				}
			}
			// AND NOTHING REACHES THE DEVICE WHILE IT IS GONE. Frames accepted while hidden are
			// discarded in order and never reach a screen, so a loop that drew them would be doing
			// work that is thrown away - which is exactly what a background client must not do.
			BACKGROUND => {
				quiet += 1;
				if quiet > 40 {
					drop(thief.take());
					phase = RESTORED;
					quiet = 0;
				}
			}
			// GIVE IT BACK, AND THEN RESIZE UNDER IT. A changed extent is a new generation, every
			// image of the old one is stale, and the loop has to rebuild the queue and supply a new
			// set before it can draw again - which is the whole reason the helper owns the queue and
			// the renderer does not.
			RESTORED => {
				quiet += 1;
				if quiet > 20 {
					let mut frame = [0u8; 64];
					let mut handles = wire::Handles::new();
					let resized = display_device::DeviceEvent::Resized(display_device::Extent2d { width: 4, height: 4 });
					let len = display_device::display_device::events_frame(0, &resized, &mut frame, &mut handles).expect("a device event encodes");
					device_events.send(Message::new(frame[..len].to_vec(), alloc::vec::Vec::new())).expect("gpu resize event");
					phase = RESIZED;
				}
			}
			_ => {}
		}
		// THE PROBE ENDING DOES NOT END THIS LOOP UNTIL THE PHASES HAVE RUN, because a probe that
		// finished early would leave the harness reporting a pass over the phases it never drove.
		if process.is_terminated() && phase == RESIZED {
			break;
		}
		// THE BOUNDED DRAIN ABOVE ALREADY GIVES THE GUEST A TICK, so a pass that answered nothing has
		// already waited. `worked` is kept for the shape of the loop rather than for a second wait.
		let _ = worked;
	}
	while let Ok(message) = stdout.recv() {
		output.extend_from_slice(&message.bytes);
	}
	assert!(process.is_terminated(), "the probe ran to completion rather than being cut off");
	assert_eq!(phase, RESIZED, "the harness drove every phase; presents={presents} output={output:?}");
	assert!(output.windows(b"frame-probe: open".len()).any(|window| window == b"frame-probe: open"), "the loop opened a surface: {output:?}");
	// THE FRAME FLOOR IS THE PROBE'S OWN ANSWER and not a number counted here, because what the loop
	// presents depends on how long the phases below take - and a gate that asserted an exact count
	// would be asserting the harness's timing rather than the loop's behaviour.
	assert!(output.windows(b"frame-probe: frames".len()).any(|window| window == b"frame-probe: frames"), "and presented every frame it meant to: {output:?}");
	assert!(output.windows(b"limit=2".len()).any(|window| window == b"limit=2"), "against the count it negotiated: {output:?}");
	// AND IT PACED RATHER THAN DRAWING AS FAST AS THE MACHINE ALLOWS. A backend that reports no
	// timing is the one this tree has, and a loop that read that as permission to spin would cost a
	// core and call it a frame rate.
	assert!(!output.windows(b"idled=0 ".len()).any(|window| window == b"idled=0 "), "the loop waited between frames: {output:?}");
	// AND IT THROTTLED WHILE HIDDEN, which is the same answer for a different reason.
	assert!(!output.windows(b"background=0 ".len()).any(|window| window == b"background=0 "), "the loop throttled while it was not the visible surface: {output:?}");
	assert_eq!(background_presents, 0, "and drew nothing at all while hidden");
	// AND IT REBUILT FOR THE NEW GENERATION rather than presenting an image drawn for the old extent.
	assert!(!output.windows(b"rebuilt=0 ".len()).any(|window| window == b"rebuilt=0 "), "the loop rebuilt when the configuration moved: {output:?}");
}

tagged_test!(audio_service_enforces_scope_and_mixes_streams, [Service, Audio, AudioService], id = "kernel.services.audio_service_enforces_scope_and_mixes_streams", covers = ["kernel", "bin.audio_service"]);
fn audio_service_enforces_scope_and_mixes_streams() {
	run_audio_service_scenario(AudioServiceScenario::ScopeAndMixing);
}

tagged_test!(audio_service_applies_bounded_backpressure, [Service, Audio, AudioService], id = "kernel.services.audio_service_applies_bounded_backpressure", covers = ["kernel"]);
fn audio_service_applies_bounded_backpressure() {
	run_audio_service_scenario(AudioServiceScenario::Backpressure);
}

tagged_test!(audio_service_keeps_mp3_playback_continuous, [Service, Audio, AudioService], id = "kernel.services.audio_service_keeps_mp3_playback_continuous", covers = ["kernel"]);
fn audio_service_keeps_mp3_playback_continuous() {
	run_audio_service_scenario(AudioServiceScenario::Mp3Continuity);
}

tagged_test!(audio_service_closes_streams_after_driver_failure, [Service, Audio, AudioService], id = "kernel.services.audio_service_closes_streams_after_driver_failure", covers = ["kernel"]);
fn audio_service_closes_streams_after_driver_failure() {
	run_audio_service_scenario(AudioServiceScenario::DriverFailure);
}

tagged_test!(dhcp_lease_renews_at_t1_and_restarts_its_clock, [Service, Network, Slow], id = "kernel.services.dhcp_lease_renews_at_t1_and_restarts_its_clock", covers = ["kernel", "services", "bin.network_service"]);
fn dhcp_lease_renews_at_t1_and_restarts_its_clock() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Drive the real userspace NetworkService end to end as its DHCP server AND its
	// frame-mover driver: spawn it with FRAMES + SERVE channels, lead with its MAC,
	// answer the DISCOVER -> REQUEST handshake with a lease whose clock is short
	// (T1 = 1 s, T2 = 2 s, lease 3 s), answer the gratuitous ARP so the service
	// learns the server's MAC, and then let the scheduler tick: at T1 the service
	// must send the lease-extension REQUEST on its own - the RFC 2131 RENEWING form
	// (ciaddr filled, unicast to the server, no server-id option) - and an ACK must
	// restart its clock, proven by the NEXT renewal arriving a full T1 later rather
	// than at the unanswered-retransmit pace.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"network_service").expect("network_service in the package or volume");
	let our_mac: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
	let srv_mac: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
	let leased: [u8; 4] = [10, 0, 2, 99];
	let server: [u8; 4] = [10, 0, 2, 2];

	// Build a DHCP server reply frame (Ethernet + IPv4 + UDP 67 -> 68 + BOOTP reply
	// with the lease-clock options; the stack verifies no checksums).
	let reply = |msg_type: u8, dst_ip: [u8; 4], dst_mac: [u8; 6], xid: [u8; 4], chaddr: [u8; 6]| -> Message {
		let mut bootp = alloc::vec![0u8; 236];
		bootp[0] = 2; // BOOTREPLY
		// A SERVER ECHOES WHAT THE CLIENT SENT. The client draws a transaction identity per exchange
		// and matches it, so a fixture that left these zero is answering a conversation nobody had -
		// which is what a forged reply looks like, and is now refused.
		bootp[4..8].copy_from_slice(&xid);
		bootp[16..20].copy_from_slice(&leased); // yiaddr
		bootp[28..34].copy_from_slice(&chaddr);
		bootp.extend_from_slice(&0x6382_5363u32.to_be_bytes());
		bootp.extend_from_slice(&[53, 1, msg_type]);
		bootp.extend_from_slice(&[54, 4, server[0], server[1], server[2], server[3]]);
		bootp.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
		bootp.extend_from_slice(&[3, 4, server[0], server[1], server[2], server[3]]);
		bootp.extend_from_slice(&[6, 4, 10, 0, 2, 3]);
		bootp.extend_from_slice(&[51, 4, 0, 0, 0, 3]); // lease 3 s
		bootp.extend_from_slice(&[58, 4, 0, 0, 0, 1]); // T1 1 s
		bootp.extend_from_slice(&[59, 4, 0, 0, 0, 2]); // T2 2 s
		bootp.push(255);
		let mut f = alloc::vec::Vec::new();
		f.extend_from_slice(&dst_mac);
		f.extend_from_slice(&srv_mac);
		f.extend_from_slice(&0x0800u16.to_be_bytes());
		let total: u16 = (20 + 8 + bootp.len()) as u16;
		let mut ip = [0u8; 20];
		ip[0] = 0x45;
		ip[2..4].copy_from_slice(&total.to_be_bytes());
		ip[8] = 64;
		ip[9] = 17; // UDP
		ip[12..16].copy_from_slice(&server);
		ip[16..20].copy_from_slice(&dst_ip);
		f.extend_from_slice(&ip);
		f.extend_from_slice(&67u16.to_be_bytes());
		f.extend_from_slice(&68u16.to_be_bytes());
		f.extend_from_slice(&((8 + bootp.len()) as u16).to_be_bytes());
		f.extend_from_slice(&[0, 0]); // checksum: unverified
		f.extend_from_slice(&bootp);
		Message::new(f, alloc::vec::Vec::new())
	};
	// Decode a frame from the service: a DHCP client message's (type, ciaddr,
	// unicast Ethernet destination, server-id option present), or None.
	let decode = |f: &[u8]| -> Option<(u8, [u8; 4], bool, bool)> {
		if f.len() < 14 + 20 + 8 + 240 || f[12..14] != [0x08, 0x00] || f[14 + 9] != 17 {
			return None;
		}
		if f[14 + 20..14 + 22] != [0, 68] || f[14 + 22..14 + 24] != [0, 67] {
			return None;
		}
		let bootp = &f[14 + 20 + 8..];
		let ciaddr: [u8; 4] = [bootp[12], bootp[13], bootp[14], bootp[15]];
		let mut msg_type: u8 = 0;
		let mut server_id: bool = false;
		let mut p: usize = 240;
		while p + 2 <= bootp.len() && bootp[p] != 255 {
			match bootp[p] {
				0 => p += 1,
				53 => {
					msg_type = bootp[p + 2];
					p += 2 + bootp[p + 1] as usize;
				}
				54 => {
					server_id = true;
					p += 2 + bootp[p + 1] as usize;
				}
				_ => p += 2 + bootp[p + 1] as usize,
			}
		}
		Some((msg_type, ciaddr, f[0..6] == srv_mac, server_id))
	};

	let (boot_kernel, boot_user) = Channel::create();
	let (frames_kernel, frames_user) = Channel::create();
	let (_serve_kernel, serve_user) = Channel::create();
	let _network_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	// no config tree serves this scenario: CONFIG with no handle tells the service
	// to fall back to its compiled-in defaults (the neighbor-cache size).
	boot_kernel.send(Message::new(b"CONFIG".to_vec(), alloc::vec::Vec::new())).expect("config bootstrap");
	send_cap(&boot_kernel, b"SERVE", serve_user, Rights::ALL).expect("serve bootstrap");
	// THE PROVIDER CATALOGUE, LAST, AND IT IS HOW THIS SERVICE FINDS ITS NIC. It is not handed a
	// `FRAMES` channel any more - it subscribes to the network kind and opens a connection to what it
	// finds - so this harness answers that conversation. See `serve_provider_catalogue`.
	let (catalogue_server, catalogue_client) = Channel::create();
	send_cap(&boot_kernel, b"CATALOGUE", catalogue_client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the catalogue channel");
	sched::run_until_idle();
	crate::tests::serve_provider_catalogue(&catalogue_server, device_proto::generated::liber::device::v1::ProviderKind::Net, frames_user).expect("the catalogue answered the subscription and the connection");
	// The MAC lead-in and the ARP reply that teaches the service the server's MAC (its own gratuitous
	// ARP pumps it in), so the T1 renewal can go unicast. The OFFER and the ACK are NOT pre-queued:
	// a server echoes the client's transaction identity, and the client now draws one per exchange,
	// so this fixture has to READ the DISCOVER before it can answer it. `run_until_idle` is what
	// makes that possible - it runs the service until it blocks waiting for the reply.
	let mut mac_msg = alloc::vec::Vec::new();
	mac_msg.extend_from_slice(b"MAC");
	mac_msg.extend_from_slice(&our_mac);
	frames_kernel.send(Message::new(mac_msg, alloc::vec::Vec::new())).expect("MAC handoff");
	let mut arp_reply = alloc::vec::Vec::new();
	arp_reply.extend_from_slice(&our_mac);
	arp_reply.extend_from_slice(&srv_mac);
	arp_reply.extend_from_slice(&[0x08, 0x06]);
	arp_reply.extend_from_slice(&[0, 1, 0x08, 0, 6, 4, 0, 2]);
	arp_reply.extend_from_slice(&srv_mac);
	arp_reply.extend_from_slice(&server);
	arp_reply.extend_from_slice(&our_mac);
	arp_reply.extend_from_slice(&leased);
	frames_kernel.send(Message::new(arp_reply, alloc::vec::Vec::new())).expect("the ARP reply should queue");
	// STEPPED FROM HERE ON. An unbounded drain runs the client through its entire DHCP timeout before
	// this thread sees a single frame, and this fixture has to answer that conversation while it is
	// still open.
	let mut online: Option<Message> = None;
	let give_up = arch::apic::ticks() + 400;
	while online.is_none() && arch::apic::ticks() < give_up {
		sched::run_until_idle_until(arch::apic::ticks() + 2);
		online = boot_kernel.recv().ok();
	}

	// The service binds and reports in; its side of the conversation arrives in
	// order: the DISCOVER, the selecting REQUEST (ciaddr empty, server-id present),
	// and the gratuitous ARP announcement.
	// ONLINE COMES BEFORE EITHER FAMILY IS STARTED NOW, so it arrives ahead of the DHCP conversation
	// rather than after it.
	let online = online.expect("NetworkService online report");
	assert_eq!(&online.bytes[..], b"NetworkService: online", "the service binds and reports in");
	// THE LINK NOW CARRIES IPv6 CONTROL TRAFFIC TOO. NetworkService stands an IPv6 host up on the
	// same NIC, so router solicitations, detection probes and listener reports share this channel with
	// the DHCP conversation this test is about. "The next frame" stopped meaning "the next frame this
	// test is about", so every read below takes the next NON-IPv6 one. What is asserted is unchanged;
	// what is skipped is another protocol's traffic, which has tests of its own.
	macro_rules! next_ipv4 {
		() => {{
			loop {
				let taken = frames_kernel.recv();
				match &taken {
					Ok(frame) if frame.bytes.len() >= 14 && frame.bytes[12] == 0x86 && frame.bytes[13] == 0xdd => continue,
					_ => break taken,
				}
			}
		}};
	}
	// STEPPED, NOT DRAINED. An unbounded drain runs the client through its entire DHCP timeout before
	// this thread sees a single frame - which is why the conversation used to be pre-queued. It
	// cannot be any more: the client draws a transaction identity per exchange and a server has to
	// echo it, so this fixture must read the DISCOVER before it can answer it.
	macro_rules! step_for {
		($what:expr) => {{
			let mut taken: Option<Message> = None;
			let give_up = arch::apic::ticks() + 400;
			while taken.is_none() && arch::apic::ticks() < give_up {
				sched::run_until_idle_until(arch::apic::ticks() + 2);
				taken = next_ipv4!().ok();
			}
			taken.expect($what)
		}};
	}
	let discover = step_for!("the DISCOVER should broadcast");
	assert_eq!(decode(&discover.bytes).map(|(t, _, _, _)| t), Some(1), "the first frame is the DISCOVER");
	// The identity this client chose, which the answer has to carry.
	let bootp = &discover.bytes[14 + 20 + 8..];
	let xid: [u8; 4] = [bootp[4], bootp[5], bootp[6], bootp[7]];
	let chaddr: [u8; 6] = [bootp[28], bootp[29], bootp[30], bootp[31], bootp[32], bootp[33]];
	frames_kernel.send(reply(2, [255; 4], [0xff; 6], xid, chaddr)).expect("the OFFER should queue");
	let request = step_for!("the REQUEST should follow the OFFER");
	let (rtype, rciaddr, _, rsid) = decode(&request.bytes).expect("the second frame decodes");
	assert!(rtype == 3 && rciaddr == [0; 4] && rsid, "the selecting REQUEST names the server, ciaddr empty");
	frames_kernel.send(reply(5, [255; 4], [0xff; 6], xid, chaddr)).expect("the ACK should queue");
	let arp = step_for!("the gratuitous ARP should send");
	assert_eq!(&arp.bytes[12..14], &[0x08, 0x06], "the announcement is an ARP request");

	// Let the clock tick to T1: the service must wake itself (the lease deadline is
	// a periodic housekeeping wake) and send the RENEWING-form REQUEST.
	let mut renewal: Option<Message> = None;
	let give_up = arch::apic::ticks() + 500;
	while renewal.is_none() && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		arch::idle_halt();
		renewal = next_ipv4!().ok();
	}
	let renewal = renewal.expect("the T1 renewal REQUEST should arrive unprompted");
	// A RENEWAL IS ITS OWN EXCHANGE and carries its own identity, so the answer has to carry that one
	// rather than the identity of the conversation that granted the lease.
	let renewal_bootp = &renewal.bytes[14 + 20 + 8..];
	let renewal_xid: [u8; 4] = [renewal_bootp[4], renewal_bootp[5], renewal_bootp[6], renewal_bootp[7]];
	let (t, ciaddr, unicast, sid) = decode(&renewal.bytes).expect("the renewal decodes");
	assert_eq!(t, 3, "the renewal is a REQUEST");
	assert_eq!(ciaddr, leased, "the renewal carries the bound address in ciaddr");
	assert!(unicast, "the renewal goes unicast to the server it learned by ARP");
	assert!(!sid, "the RENEWING form omits the server-id option");

	// ACK the renewal (unicast to the bound address) and prove the clock RESTARTED:
	// the next renewal must arrive a full T1 (~100 ticks) later - an unanswered
	// REQUEST would have retransmitted at half the time to T2 (~50 ticks) instead.
	let acked_at = arch::apic::ticks();
	frames_kernel.send(reply(5, leased, our_mac, renewal_xid, chaddr)).expect("the renewal ACK should send");
	let mut second: Option<Message> = None;
	let give_up = acked_at + 500;
	while second.is_none() && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		arch::idle_halt();
		second = next_ipv4!().ok();
	}
	let second = second.expect("the next T1 renewal should arrive");
	let (t2, ciaddr2, _, _) = decode(&second.bytes).expect("the second renewal decodes");
	assert!(t2 == 3 && ciaddr2 == leased, "the clock re-arms another renewal");
	assert!(arch::apic::ticks() - acked_at >= 75, "the renewal came at the restarted T1, not the retransmit pace");
}

tagged_test!(process_service_canonicalizes_short_and_explicit_program_names, [Service, Process, ProcessService], id = "kernel.services.process_service_canonicalizes_short_and_explicit_program_names", covers = ["kernel"]);
fn process_service_canonicalizes_short_and_explicit_program_names() {
	// ProcessService falls back to the init package without a storage client. Both a
	// short logical name and its explicit physical basename must report one identity.
	let artifact: &[u8] = b"log_service.lsexe";
	let (replies, list) = run_process_service_requests(&[(1, b"log_service"), (2, artifact)], None);
	assert!(list.is_none());
	assert_process_start_reply(&replies[0], 1, artifact);
	assert_process_start_reply(&replies[1], 2, artifact);
}

tagged_test!(process_service_lists_every_started_program, [Service, Process, ProcessService], id = "kernel.services.process_service_lists_every_started_program", covers = ["kernel", "bin.process_service"]);
fn process_service_lists_every_started_program() {
	// SEEN TO FAIL ON riscv64, three times in a row, on 2026-08-08: two processes started and
	// acknowledged with koids, and the list held one. It then passed twice - the tag alone and the
	// full 197-test suite - with nothing changed that touches ProcessService, so it is INTERMITTENT
	// and its cause is not known. Recorded here rather than in a milestone because this is where
	// the next person to see it will be standing.
	//
	// The suspect, unproven: `Processes::record` reaps before it pushes, and `reap` drops any entry
	// whose process is not PROC_STATE_RUNNING - which the kernel defines as "has live threads". A
	// launch that has not yet had its entry thread started, or one whose thread has just ended,
	// reads as STOPPED. The second START would then drop the first launch on its way in, and the
	// count would be exactly what was seen.
	//
	// **Seen on x86_64, 2026-08-09**, once in a run of 211 and not on the immediate re-run of the
	// same binary. That answers the question this note used to end with - "whether riscv64's
	// scheduling opens that window and the other two architectures' closes it" - and the answer is
	// no: the window is general and x86_64 merely hits it less often. Whatever is done about this,
	// it is not an architecture's problem.
	//
	// What the fix is NOT, so the next attempt does not start there: reaping only entries that were
	// once seen RUNNING is one line and one field, and it leaks. A launch whose load fails is never
	// seen running, so it would never be reaped and its handle never closed. Any fix has to keep
	// both ends - a live process must not be dropped, and a dead one must not be kept forever.
	//
	// The assertion below names the survivors for this reason: `1 != 2` says nothing about WHICH
	// launch went missing, and that is the first fact the next attempt needs.
	//
	// **FIXED 2026-08-10**, and the suspect above was right. `PROC_STATE_STOPPED` is
	// `live_threads().is_empty()`, which is true both of a process whose threads have exited and of
	// one whose entry thread has been started and not yet picked up - so reaping on the state alone
	// cannot tell them apart. `ProcessStats` carries the discriminator the state does not:
	// `completion_valid`, set from `exit_status()`, which exists only once the process has really
	// finished. `Processes::reap` now keeps an entry that is STOPPED with no exit status.
	//
	// Third sighting, riscv64, in a run of 208 - which is what finally paid for the fix.
	//
	// No deterministic regression test, and the reason is worth stating rather than leaving as an
	// omission: the window needs a spawned thread to exist and NOT be scheduled, and this harness
	// drives the service by queueing every request and then draining with one `run_until_idle`. It
	// offers no way to hold one thread back. What the pair of tests DOES pin is both ends of the
	// invariant - this one requires a live process to be listed, and
	// `process_service_drops_a_terminated_process_from_the_list` requires a finished one to go. The
	// second was watched failing with the new branch widened to keep everything (`left: 1, right:
	// 0`), which is the direction this fix could plausibly have been wrong in.
	// TWO PROGRAMS THAT CANNOT EXIT ON THEIR OWN, and LAUNCH is what makes that true.
	//
	// **The fourth sighting, x86_64 2026-08-19, and this time with the reason.** `holdopen` blocks on
	// its bootstrap channel forever - which is a fact about a channel it HAS. `ProcessService::start`
	// spawns with no bootstrap capability at all ("phase 1: started processes run unattended", and
	// the handle is literally `0`), so `recv_into(0, ..)` failed, `holdopen`'s `Failed` arm broke out
	// of the loop, and the program that "cannot exit on its own" exited as fast as the scheduler
	// could run it. Whether it was still alive when the list request drained was then exactly the
	// race the fixture was introduced to remove. The comment above `holdopen`'s own loop said "the
	// test holds the other end"; with START, nobody did.
	//
	// So the requests are LAUNCHes. Launch hands the caller a bootstrap channel end and a live
	// process handle, which is what the fixture always assumed: the two children block on channels
	// this test holds, they are alive for precisely as long as it keeps them, and dropping the ends
	// at the end of the test is what lets them finish rather than leaving two blocked processes
	// behind for the rest of the suite.
	use object::channel::Channel;
	use object::rights::Rights;

	let (_boot_kernel, service_client) = spawn_service_with_package(b"process_service");
	let mut held: alloc::vec::Vec<alloc::sync::Arc<Channel>> = alloc::vec::Vec::new();
	let mut children: alloc::vec::Vec<alloc::sync::Arc<object::process::Process>> = alloc::vec::Vec::new();
	for correlation in [11u32, 12u32] {
		let (bootstrap_kernel, bootstrap_user) = Channel::create();
		let name: &[u8] = b"holdopen";
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&3u16.to_le_bytes());
		request.extend_from_slice(&correlation.to_le_bytes());
		request.extend_from_slice(&(name.len() as u16).to_le_bytes());
		request.extend_from_slice(name);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&service_client, &request, bootstrap_user, Rights::ALL).expect("launch request");
		sched::run_until_idle();
		let reply = service_client.recv().expect("launch reply");
		assert_eq!(le_u32(&reply.bytes, 0), correlation, "the reply echoes the correlation id");
		assert_eq!(reply.bytes[4], 1, "the launch succeeded");
		// The live process the launch handed back, kept so the state of each child can be READ
		// rather than inferred from the list - the open observation.
		children.push(reply.caps[0].object().into_any_arc().downcast::<object::process::Process>().expect("the launch reply carries a Process"));
		// The end this test keeps. While it lives, the child's blocking receive cannot return.
		held.push(bootstrap_kernel);
	}
	assert_eq!(process_service_list_len(&service_client, 13), 2, "both launched processes are listed");
	// AND THEY LEAVE IT WHEN THE ENDS THIS TEST HOLDS GO. Both children are blocked in `wait` on
	// those channels, `Channel::is_readable` is "inbox non-empty OR peer closed", and `sys_wait`
	// re-checks it on every wake - so dropping the ends must let both return, exit, and be reaped
	// out of the list.
	//
	// THIS ASSERTION WAS RED FOR TWO DAYS ON aarch64 AND THE READING OF IT WAS WRONG. It was
	// recorded as "a blocked ring-3 child is not woken", and the child was woken all along: it ran,
	// returned from `wait`, and exited. What never happened was the RECORDING of its exit status -
	// `SYS_USER_EXIT` is short-circuited in the aarch64 and riscv64 trap paths before it reaches
	// the arm that latches it - and ProcessService keeps a stopped process with no status, because
	// that is exactly what a launch which has not started yet looks like. So the children stayed
	// listed forever on two ports of three, and this is the test that says they do not.
	//
	// Bounded rather than unbounded: a regression here is a failure, not a hang.
	drop(held);
	let mut remaining = u16::MAX;
	for _ in 0..64 {
		sched::run_until_idle();
		remaining = process_service_list_len(&service_client, 14);
		if remaining == 0 {
			break;
		}
	}
	assert_eq!(remaining, 0, "both children observed their peer closing, exited, and left the list");
	for (index, child) in children.iter().enumerate() {
		assert_eq!(child.exit_status(), Some(0), "child {index} recorded the status it exited with");
	}
}

tagged_test!(a_prepared_launch_can_be_cancelled_and_a_client_that_leaves_cancels_its_own, [Service, Process, ProcessService], id = "kernel.services.a_prepared_launch_can_be_cancelled_and_a_client_that_leaves_cancels_its_own", covers = ["kernel"]);
fn a_prepared_launch_can_be_cancelled_and_a_client_that_leaves_cancels_its_own() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// IDL-001. A prepared launch is a loaded process whose first thread exists and has not been
	// queued. There were exactly two ways out of one: release it, or leave it - and "leave it"
	// means a process loaded, stopped, holding its address space, its stacks, its Domain and its
	// bootstrap channel, for the life of the system. PermissionManager takes that path on every
	// early return in pipeline assembly, which a mistyped shell line reaches.
	//
	// So: `cancel`, and a client that simply GOES is cancelled too.
	let (_boot_kernel, service_client) = spawn_service_with_package(b"process_service");

	// One prepared launch on this client, cancelled by name.
	let prepare = |client: &alloc::sync::Arc<Channel>, correlation: u32| -> u64 {
		let (bootstrap_kernel, bootstrap_user) = Channel::create();
		let name: &[u8] = b"holdopen";
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&6u16.to_le_bytes()); // launch-prepared
		request.extend_from_slice(&correlation.to_le_bytes());
		request.extend_from_slice(&(name.len() as u16).to_le_bytes());
		request.extend_from_slice(name);
		request.extend_from_slice(&0u32.to_le_bytes()); // the bootstrap handle's in-stream placeholder
		send_cap(client, &request, bootstrap_user, Rights::ALL).expect("prepare request");
		sched::run_until_idle();
		let reply = client.recv().expect("prepare reply");
		assert_eq!(le_u32(&reply.bytes, 0), correlation, "the reply echoes the correlation id");
		assert_eq!(reply.bytes[4], 1, "the prepare succeeded");
		// `start-result` is the task handle's placeholder then `process-info`, whose first field
		// is the koid: [corr u32][ok u8][handle u32][koid u64].
		let koid = u64::from_le_bytes(reply.bytes[9..17].try_into().expect("a koid in the reply"));
		// The end this test keeps, so the prepared program has a bootstrap channel that outlives
		// the request - the same shape a pipeline stage has.
		core::mem::forget(bootstrap_kernel);
		koid
	};
	let cancel = |client: &alloc::sync::Arc<Channel>, correlation: u32, koid: u64| -> bool {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&9u16.to_le_bytes()); // cancel
		request.extend_from_slice(&correlation.to_le_bytes());
		request.extend_from_slice(&koid.to_le_bytes());
		client.send(Message::new(request, alloc::vec::Vec::new())).expect("cancel request");
		sched::run_until_idle();
		let reply = client.recv().expect("cancel reply");
		assert_eq!(le_u32(&reply.bytes, 0), correlation, "the reply echoes the correlation id");
		assert_eq!(reply.bytes[4], 1, "cancel answered");
		reply.bytes[5] != 0
	};

	let koid = prepare(&service_client, 41);
	assert!(cancel(&service_client, 42, koid), "the launch this client prepared is its to cancel");
	assert!(!cancel(&service_client, 43, koid), "and once cancelled there is nothing left to cancel");

	// AND A CLIENT THAT LEAVES WITHOUT SAYING SO. `serve_multi` used to close the channel and drop
	// it from its set without telling the handler, so nothing here could learn that the client was
	// gone - and everything it had prepared stayed prepared. Measured on the frame allocator,
	// because what is being held is a loaded program: its image, its stacks and its page tables.
	let before = mem::frame::free_count();
	service_client.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new())).expect("connect request");
	sched::run_until_idle();
	let connected = service_client.recv().expect("connect reply");
	let sub: alloc::sync::Arc<Channel> = connected.caps.first().expect("a minted client channel").object().into_any_arc().downcast::<Channel>().expect("the mint is a channel");
	let _ = prepare(&sub, 44);
	let loaded = mem::frame::free_count();
	assert!(loaded < before, "a prepared launch holds the frames of a loaded program: {before} -> {loaded}");
	drop(sub);
	drop(connected);
	sched::run_until_idle();
	let after = mem::frame::free_count();
	assert!(after > loaded, "the client left, so what it prepared was abandoned: {loaded} -> {after}");
}

tagged_test!(process_service_drops_a_terminated_process_from_the_list, [Service, Process, ProcessService], id = "kernel.services.process_service_drops_a_terminated_process_from_the_list", covers = ["kernel"]);
fn process_service_drops_a_terminated_process_from_the_list() {
	use object::channel::Channel;
	use object::process::Process;
	use object::rights::Rights;

	// `ps` used to report every process the system had ever started, because nothing removed
	// an entry - the service held no handle to a launched process and so could not tell that
	// one had ended. It keeps a READ duplicate for exactly this, and both directions are
	// asserted here: without the first the test would pass just as well if the launch had
	// never been recorded, and without the second it would pass on the old behaviour.
	let _boot_kernel = spawn_service_with_package(b"process_service");
	let service_client = &_boot_kernel.1;

	// LAUNCH rather than START, because only LAUNCH hands the live process handle back, and
	// this test has to be the thing that ends the process.
	let (_bootstrap_kernel, bootstrap_user) = Channel::create();
	let name: &[u8] = b"log_service";
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&3u16.to_le_bytes());
	request.extend_from_slice(&21u32.to_le_bytes());
	request.extend_from_slice(&(name.len() as u16).to_le_bytes());
	request.extend_from_slice(name);
	request.extend_from_slice(&0u32.to_le_bytes());
	send_cap(service_client, &request, bootstrap_user, Rights::ALL).expect("launch request");
	sched::run_until_idle();
	let reply = service_client.recv().expect("launch reply");
	assert_eq!(reply.bytes[4], 1, "the launch succeeded");
	let process = reply.caps[0].object().into_any_arc().downcast::<Process>().expect("the launch reply carries a Process");

	assert_eq!(process_service_list_len(service_client, 22), 1, "a running process is listed");
	process.terminate();
	sched::run_until_idle();
	assert_eq!(process_service_list_len(service_client, 23), 0, "a terminated process leaves the list");
}

tagged_test!(process_service_accounts_a_bounded_launch, [Service, Process, ProcessService, Domain], id = "kernel.services.process_service_accounts_a_bounded_launch", covers = ["kernel"]);
fn process_service_accounts_a_bounded_launch() {
	use object::channel::Channel;
	use object::rights::Rights;

	// A per-launch Domain used to be invisible: ProcessService created one, handed it to the
	// process and forgot it, and ResourceManager can only report the Domains it was given -
	// so isolation was enforced and nothing could observe it. `accounting` answers with the
	// live counters of every Domain this service is holding, by value and never by handle.
	let harness = spawn_service_with_package(b"process_service");
	let service_client = &harness.1;

	// Nothing launched under a limit yet, so there is nothing to account. Without this the
	// test could not tell a working report from one that answers with whatever it finds.
	assert_eq!(process_service_accounting(service_client, 31).len(), 0, "a service that has bounded nothing accounts nothing");

	// An ordinary launch runs in the caller's Domain and has no counters of its own, so it
	// must not appear either - listing it would report somebody else's numbers under its name.
	let (_plain_bootstrap, plain_child) = Channel::create();
	let mut plain = alloc::vec::Vec::new();
	plain.extend_from_slice(&3u16.to_le_bytes());
	plain.extend_from_slice(&32u32.to_le_bytes());
	plain.extend_from_slice(&(b"log_service".len() as u16).to_le_bytes());
	plain.extend_from_slice(b"log_service");
	plain.extend_from_slice(&0u32.to_le_bytes());
	send_cap(service_client, &plain, plain_child, Rights::ALL).expect("plain launch request");
	sched::run_until_idle();
	assert_eq!(service_client.recv().expect("plain launch reply").bytes[4], 1, "the plain launch succeeded");
	assert_eq!(process_service_accounting(service_client, 33).len(), 0, "a launch without a stated limit has no Domain of its own to report");

	// A bounded launch does have one, and it is reported under the program's own name.
	const LIMIT: u64 = 64 * 1024 * 1024;
	let (_bounded_bootstrap, bounded_child) = Channel::create();
	let mut bounded = alloc::vec::Vec::new();
	bounded.extend_from_slice(&4u16.to_le_bytes());
	bounded.extend_from_slice(&34u32.to_le_bytes());
	bounded.extend_from_slice(&(b"device_manager".len() as u16).to_le_bytes());
	bounded.extend_from_slice(b"device_manager");
	bounded.extend_from_slice(&LIMIT.to_le_bytes());
	bounded.extend_from_slice(&0u32.to_le_bytes());
	send_cap(service_client, &bounded, bounded_child, Rights::ALL).expect("bounded launch request");
	sched::run_until_idle();
	assert_eq!(service_client.recv().expect("bounded launch reply").bytes[4], 1, "the bounded launch succeeded");

	let accounted = process_service_accounting(service_client, 35);
	assert_eq!(accounted.len(), 1, "the bounded launch is accounted, and only it");
	let (name, memory_limit) = &accounted[0];
	assert_eq!(name.as_slice(), b"device_manager.lsexe", "the budget is named after the program that was launched");
	assert_eq!(*memory_limit, LIMIT, "the reported memory limit is the one the launch asked for");
}

tagged_test!(process_service_resolves_one_final_executable_suffix, [Service, Process], id = "kernel.services.process_service_resolves_one_final_executable_suffix", covers = ["kernel", "services"]);
fn process_service_resolves_one_final_executable_suffix() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let process_elf = package.lookup(b"process_service.lsexe").expect("ProcessService image");
	let source_index = (0..package.len()).find(|&index| package.name(index) == Some(&b"log_service.lsexe"[..])).expect("source executable entry");
	let mut repeated_package = init.to_vec();
	let name_start = abi::PKG_HEADER_LEN + source_index * abi::PKG_ENTRY_LEN;
	repeated_package[name_start..name_start + abi::PKG_NAME_LEN].fill(0);
	let repeated_artifact = b"ping.lsexe.lsexe";
	repeated_package[name_start..name_start + repeated_artifact.len()].copy_from_slice(repeated_artifact);

	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), process_elf, boot_user, Rights::ALL).expect("spawn ProcessService");
	send_package(&boot_kernel, &repeated_package).expect("custom package bootstrap");
	boot_kernel.send(Message::new(b"STORAGE".to_vec(), alloc::vec::Vec::new())).expect("empty storage bootstrap");
	// Likewise an empty "REGISTRY": absent, but still handed over, because the
	// bootstrap consumes one message per handoff in order and a skipped handoff
	// swallows the next message instead of being skipped.
	boot_kernel.send(Message::new(b"REGISTRY".to_vec(), alloc::vec::Vec::new())).expect("empty registry bootstrap");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");

	for (corr, name) in [(1u32, &b"ping"[..]), (2, &b"ping.lsexe"[..]), (3, &b"ping.lsexe.lsexe"[..])] {
		let mut start = alloc::vec::Vec::new();
		start.extend_from_slice(&1u16.to_le_bytes());
		start.extend_from_slice(&corr.to_le_bytes());
		start.extend_from_slice(&(name.len() as u16).to_le_bytes());
		start.extend_from_slice(name);
		service_client.send(Message::new(start, alloc::vec::Vec::new())).expect("start request");
	}
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("quit sentinel");
	sched::run_until_idle();

	assert_eq!(&boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	let bare = service_client.recv().expect("bare-name reply");
	assert_eq!(le_u32(&bare.bytes, 0), 1);
	assert_eq!(bare.bytes[4], 0, "ping must not skip two suffix levels");
	for corr in [2u32, 3] {
		let reply = service_client.recv().expect("repeated-suffix launch reply");
		let bytes = &reply.bytes;
		assert_eq!(le_u32(bytes, 0), corr);
		assert_eq!(bytes[4], 1, "short or exact repeated-suffix launch succeeds");
		let name_len = le_u16(bytes, 13) as usize;
		assert_eq!(&bytes[15..15 + name_len], repeated_artifact, "ProcessInfo preserves the full physical basename");
	}
}

tagged_test!(config_service_serves_the_tree, [Config, Service], id = "kernel.services.config_service_serves_the_tree", covers = ["kernel", "bin.config_service"]);
fn config_service_serves_the_tree() {
	use object::channel::Message;

	// Drive the real userspace ConfigService over its generated Config bindings:
	// spawn it, hand it a serve channel, GET a seeded node, LIST the tree, SET a new
	// node, and GET it back. The wire is the proto framing - request [op u16][corr
	// u32][args], reply [corr u32][result]; strings are [len u16][utf8].
	let (boot_kernel, service_client) = spawn_service(b"config_service");

	// frame a GET: [op = 1 u16][corr u32][key: [len u16][utf8]].
	let get = |corr: u32, key: &[u8]| -> alloc::vec::Vec<u8> {
		let mut m = alloc::vec::Vec::new();
		m.extend_from_slice(&1u16.to_le_bytes());
		m.extend_from_slice(&corr.to_le_bytes());
		m.extend_from_slice(&(key.len() as u16).to_le_bytes());
		m.extend_from_slice(key);
		m
	};
	service_client.send(Message::new(get(1, b"system.name"), alloc::vec::Vec::new())).expect("get");

	// LIST: [op = 2 u16][corr u32].
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&2u16.to_le_bytes());
	list.extend_from_slice(&2u32.to_le_bytes());
	service_client.send(Message::new(list, alloc::vec::Vec::new())).expect("list");

	// SET demo.key = hi: [op = 3 u16][corr u32][config-entry: key string + value string].
	let (k, v): (&[u8], &[u8]) = (b"demo.key", b"hi");
	let mut set = alloc::vec::Vec::new();
	set.extend_from_slice(&3u16.to_le_bytes());
	set.extend_from_slice(&3u32.to_le_bytes());
	set.extend_from_slice(&(k.len() as u16).to_le_bytes());
	set.extend_from_slice(k);
	set.extend_from_slice(&(v.len() as u16).to_le_bytes());
	set.extend_from_slice(v);
	service_client.send(Message::new(set, alloc::vec::Vec::new())).expect("set");
	service_client.send(Message::new(get(4, b"demo.key"), alloc::vec::Vec::new())).expect("get-back");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("quit sentinel");

	sched::run_until_idle();

	let online = boot_kernel.recv().expect("ConfigService online report");
	assert_eq!(&online.bytes[..], b"ConfigService: online", "ConfigService reports in");

	// GET reply: [corr u32 = 1][ok u8 = 1][value: [len u16][utf8]].
	let r = service_client.recv().expect("get reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 1, "get echoes the correlation id");
	assert_eq!(b[4], 1, "get succeeded");
	let vlen = le_u16(b, 5) as usize;
	assert_eq!(&b[7..7 + vlen], b"LiberSystem", "system.name is the seeded value");

	// LIST reply: [corr u32 = 2][ok u8 = 1][count u16][entries...].
	let r = service_client.recv().expect("list reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 2, "list echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	assert!(le_u16(b, 5) >= 4, "the seeded tree has nodes");

	// SET reply: [corr u32 = 3][ok u8 = 1].
	let r = service_client.recv().expect("set reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 3, "set echoes the correlation id");
	assert_eq!(b[4], 1, "set succeeded");

	// GET demo.key reply: the value we just set reads back.
	let r = service_client.recv().expect("get-back reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 4, "get-back echoes the correlation id");
	assert_eq!(b[4], 1, "get-back succeeded");
	let vlen = le_u16(b, 5) as usize;
	assert_eq!(&b[7..7 + vlen], b"hi", "the value just set reads back");
}

tagged_test!(config_set_survives_a_service_reboot, [Config, Service, Storage], id = "kernel.services.config_set_survives_a_service_reboot", covers = ["kernel", "services"]);
fn config_set_survives_a_service_reboot() {
	use alloc::collections::BTreeMap;
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Persistence: a `config set` survives the service's whole lifetime ending.
	// ConfigService write-throughs its tree to `vol://system/libexec/config_service/config.tree`, so a NEW
	// instance over the SAME volume loads it back - the reboot property (and what
	// makes the transparent ConfigService restart stateless). Stand up a
	// StorageService over a writable disk carrying a prepared empty LiberFS volume, run a FIRST
	// ConfigService wired to a minted volume
	// connection, SET a key, end the instance, then run a SECOND instance over
	// another minted connection: the set value AND the seeded defaults both serve.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let (scenario_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let config_elf = program_elf(&package, scenario_volume, b"config_service").expect("config_service in the package or volume");

	// StorageService over a sparse in-memory disk carrying a prepared volume. It used to be a blank
	// disk the service formatted for itself, which is a shape that no longer occurs.
	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), storage_elf, storage_boot_user, Rights::ALL).expect("spawn StorageService");
	send_cap(&storage_boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("BLOCK bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("SERVE bootstrap");
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = crate::tests::whole_device_volume(CAPACITY as usize);
	let mut online = false;
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = storage_boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online (vol://system)");
			online = true;
			break;
		}
	}
	assert!(online, "StorageService should mount the prepared disk and report in");

	// Mint an independent volume connection off the storage root (the CONNECT_OP
	// factory), pumping block traffic while the service answers.
	fn mint_volume(storage_client: &alloc::sync::Arc<object::channel::Channel>, blk_host: &alloc::sync::Arc<object::channel::Channel>, disk: &mut alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>, capacity: u64) -> alloc::sync::Arc<object::channel::Channel> {
		use object::channel::{Channel, Message};
		storage_client.send(Message::new(0xffffu16.to_le_bytes().to_vec(), alloc::vec::Vec::new())).expect("connect request");
		for _ in 0..100_000 {
			sched::run_until_idle();
			pump_block_stand_in(blk_host, disk, capacity);
			if let Ok(reply) = storage_client.recv() {
				let cap = reply.caps.first().expect("the minted connection is transferred");
				return cap.object().into_any_arc().downcast::<Channel>().expect("the connection is a channel");
			}
		}
		panic!("no minted volume connection arrived");
	}

	// The first ConfigService instance: its persistence backing and serve channel.
	let vol1 = mint_volume(&storage_client, &blk_host, &mut disk, CAPACITY);
	let (cfg1_boot, cfg1_boot_user) = Channel::create();
	let (cfg1_server, cfg1_client) = Channel::create();
	let _config1 = spawn_dynamic_test_process(sched::root_domain(), config_elf, cfg1_boot_user);
	send_cap(&cfg1_boot, b"STORAGE", vol1, Rights::ALL).expect("STORAGE bootstrap 1");
	send_cap(&cfg1_boot, b"SERVE", cfg1_server, Rights::ALL).expect("SERVE bootstrap 1");

	// SET persist.key = survives ([op = 3 u16][corr u32][key + value strings]); the
	// write-through to vol://system/libexec/config_service/config.tree completes before the reply.
	let (k, v): (&[u8], &[u8]) = (b"persist.key", b"survives");
	let mut set = alloc::vec::Vec::new();
	set.extend_from_slice(&3u16.to_le_bytes());
	set.extend_from_slice(&1u32.to_le_bytes());
	set.extend_from_slice(&(k.len() as u16).to_le_bytes());
	set.extend_from_slice(k);
	set.extend_from_slice(&(v.len() as u16).to_le_bytes());
	set.extend_from_slice(v);
	cfg1_client.send(Message::new(set, alloc::vec::Vec::new())).expect("set request");
	let mut set_ok = false;
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(reply) = cfg1_client.recv() {
			assert_eq!(le_u32(&reply.bytes, 0), 1, "set echoes the correlation id");
			assert_eq!(reply.bytes[4], 1, "set succeeded");
			set_ok = true;
			break;
		}
	}
	assert!(set_ok, "the set should be answered");
	// End the first instance: the quit sentinel breaks its serve loop and it exits.
	cfg1_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("quit sentinel");
	sched::run_until_idle();

	// The second instance over the SAME volume: the persisted tree loads back.
	let vol2 = mint_volume(&storage_client, &blk_host, &mut disk, CAPACITY);
	let (cfg2_boot, cfg2_boot_user) = Channel::create();
	let (cfg2_server, cfg2_client) = Channel::create();
	let _config2 = spawn_dynamic_test_process(sched::root_domain(), config_elf, cfg2_boot_user);
	send_cap(&cfg2_boot, b"STORAGE", vol2, Rights::ALL).expect("STORAGE bootstrap 2");
	send_cap(&cfg2_boot, b"SERVE", cfg2_server, Rights::ALL).expect("SERVE bootstrap 2");
	let get = |corr: u32, key: &[u8]| -> alloc::vec::Vec<u8> {
		let mut m = alloc::vec::Vec::new();
		m.extend_from_slice(&1u16.to_le_bytes());
		m.extend_from_slice(&corr.to_le_bytes());
		m.extend_from_slice(&(key.len() as u16).to_le_bytes());
		m.extend_from_slice(key);
		m
	};
	cfg2_client.send(Message::new(get(1, b"persist.key"), alloc::vec::Vec::new())).expect("get persisted");
	cfg2_client.send(Message::new(get(2, b"system.name"), alloc::vec::Vec::new())).expect("get seeded");
	let mut replies: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		while let Ok(reply) = cfg2_client.recv() {
			replies.push(reply.bytes);
		}
		if replies.len() >= 2 {
			break;
		}
	}
	assert_eq!(replies.len(), 2, "both gets should be answered");
	assert_eq!(le_u32(&replies[0], 0), 1);
	assert_eq!(replies[0][4], 1, "the persisted key exists in the fresh instance");
	let vlen = le_u16(&replies[0], 5) as usize;
	assert_eq!(&replies[0][7..7 + vlen], b"survives", "the set value survived the service reboot");
	assert_eq!(replies[1][4], 1, "a seeded default still serves");
	let nlen = le_u16(&replies[1], 5) as usize;
	assert_eq!(&replies[1][7..7 + nlen], b"LiberSystem", "the persisted tree overlays, not replaces, the defaults");
	cfg2_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new())).expect("quit sentinel 2");
	sched::run_until_idle();
}

tagged_test!(a_service_that_cannot_be_restarted_is_left_failed_rather_than_reported_up, [Service, Process], id = "kernel.services.a_service_that_cannot_be_restarted_is_left_failed_rather_than_reported_up", covers = ["kernel", "storage"]);
fn a_service_that_cannot_be_restarted_is_left_failed_rather_than_reported_up() {
	// THE FAULT-MATRIX ROW FOR A SERVICE NOBODY CAN BRING BACK.
	//
	// Detector: the supervisor, when the service's channel closes.
	// Owner:    ServiceManager.
	// Outcome:  the service is recorded FAILED and dropped from the wait set. It is NOT restarted,
	//           and - the part that matters - it is not left being reported as running either.
	//
	// Twenty of twenty-three services are in this class today, because their bootstrap cannot be
	// re-run: `relaunch_service` says so in as many words. The generated role plan is what shrinks that number,
	// and until it reaches zero the honest answer for the rest is a state that says so.
	//
	// Driven at the harness rather than through a whole boot: what is being measured is that a
	// service which ENDS is observable as ended, and a storage instance ending is the cheapest
	// truthful version of that.
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let mut ram = StorageHarness::start_memory(storage_elf, b"RAMVOL", 8 * 1024);
	assert!(ram.write(b"vol://ram/live", b"answered while it was there", 0x7d01), "the service answers while it is running");

	ram.kill_service();

	// THE STATE IS OBSERVABLE FROM OUTSIDE. A client that kept calling would find out; the point of
	// a supervisor is that somebody finds out WITHOUT calling. The channel's peer being gone is
	// that signal, and it is what the supervise loop selects on.
	assert!(ram.client_peer_closed(), "the ended service is observable as ended rather than silently absent");

	// WHAT A CALL AFTERWARDS ANSWERS is the neighbouring row of this matrix and is measured there,
	// through the generated client rather than through this harness - which speaks the wire by hand
	// and would prove nothing about the code every real caller goes through. See
	// `a_call_that_never_left_is_told_apart_from_one_that_may_have_landed`.
}

tagged_test!(a_call_that_never_left_is_told_apart_from_one_that_may_have_landed, [Service, Storage], id = "kernel.services.a_call_that_never_left_is_told_apart_from_one_that_may_have_landed", covers = ["kernel", "services"]);
fn a_call_that_never_left_is_told_apart_from_one_that_may_have_landed() {
	// THE ONE DISTINCTION A CLIENT CANNOT DO WITHOUT, and the one every generated client used to
	// throw away.
	//
	// The transport has always known it: `SendRefused` when the request never went out, and
	// `PeerClosed` or `TimedOut` when it did and no reply came. Every client method then collapsed
	// both to `None`, so a caller could not tell a write that never reached the medium from one
	// that may have landed - and there is no safe move that covers both. Retrying the first is
	// correct; retrying the second is how a transfer happens twice.
	//
	// It is answered in the protocol's own words rather than a new channel: `again` for a request
	// that never left, and `commit-uncertain` - the answer `base.error` grew for exactly this - for
	// one whose outcome nobody knows.
	// DRIVEN THROUGH THE GENERATED CLIENT, because that is where the answer was being lost. This
	// suite's storage harness speaks the wire by hand, so it would prove nothing about the code
	// every real caller goes through.
	use object::channel::Channel;
	use object::rights::Rights;
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let probe_elf = program_elf(&package, _volume, b"role_probe").expect("role_probe");

	let (parent, child) = Channel::create();
	let (report, report_child) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), probe_elf, child);
	send_cap(&parent, &[5u8], report_child, Rights::ALL).expect("case selector and report channel");
	// A channel whose FAR END IS DROPPED BEFORE THE CALL. No service ever existed on it, which is
	// the same position as one that has ended: the request cannot be delivered and cannot have
	// been acted on.
	let (near, far) = Channel::create();
	core::mem::drop(far);
	send_cap(&parent, b"DEAD", near, Rights::ALL).expect("dead channel");
	let mut answer = alloc::vec::Vec::new();
	for _ in 0..200_000 {
		sched::run_until_idle();
		if let Ok(reply) = report.recv() {
			answer = reply.bytes;
			break;
		}
		if process.is_terminated() {
			break;
		}
	}
	// `again` and not a bare failure. The request never left this process, so retrying it is safe
	// - and a caller told only "it did not work" has to guess between that and an operation that
	// may already have happened.
	assert_eq!(answer.as_slice(), b"err 3", "a call whose request never left is answered `again`, which says retrying is safe: {:?}", core::str::from_utf8(&answer));
}

tagged_test!(a_call_that_may_have_landed_is_answered_commit_uncertain, [Service, Storage], id = "kernel.services.a_call_that_may_have_landed_is_answered_commit_uncertain", covers = ["kernel", "services"]);
fn a_call_that_may_have_landed_is_answered_commit_uncertain() {
	// THE OTHER HALF OF THE SAME ROW, and the half that decides whether the distinction is worth
	// anything. Its neighbour proves that a request which never left is answered `again`; on its
	// own that proves only that failures have a name. What a caller needs is the LINE: this side of
	// it, retrying is safe; the other side, retrying is how a transfer happens twice.
	//
	// Same probe, same generated client, same op. The ONLY difference is choreography - here the
	// far end is alive when the request goes out and gone before any reply comes back, which is
	// exactly the position a client is in when the service it is calling dies mid-call.
	use object::channel::Channel;
	use object::rights::Rights;
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let probe_elf = program_elf(&package, _volume, b"role_probe").expect("role_probe");

	let (parent, child) = Channel::create();
	let (report, report_child) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), probe_elf, child);
	send_cap(&parent, &[5u8], report_child, Rights::ALL).expect("case selector and report channel");
	// ALIVE AT SEND TIME. The far end is held here, so the probe's request is accepted and queued
	// rather than refused - which is the whole difference from the test above.
	let (near, far) = Channel::create();
	send_cap(&parent, b"LIVE", near, Rights::ALL).expect("channel with a live peer");

	// AND IT REALLY LANDED, asserted rather than assumed. Receiving the request here is the proof
	// that this is not the `again` case wearing a different name: if the send had been refused the
	// probe would already have answered, and the answer would be the neighbouring row's.
	let mut landed = false;
	for _ in 0..200_000 {
		sched::run_until_idle();
		if far.recv().is_ok() {
			landed = true;
			break;
		}
		if process.is_terminated() {
			break;
		}
	}
	assert!(landed, "the request must reach the far end, or this measures the `again` case again");

	// NOW THE SERVICE DIES, with the request received and no reply sent. Nobody - not the caller,
	// not this test - knows whether it acted on it first. That is the state the answer has to name.
	core::mem::drop(far);
	let mut answer = alloc::vec::Vec::new();
	for _ in 0..200_000 {
		sched::run_until_idle();
		if let Ok(reply) = report.recv() {
			answer = reply.bytes;
			break;
		}
		if process.is_terminated() {
			break;
		}
	}
	// `commit-uncertain`, which is 12 in `base.error` - and NOT `again`, which is 3. A caller told
	// `again` here would retry a write that may already have been made.
	assert_eq!(answer.as_slice(), b"err 12", "a call that may have been acted on is answered `commit-uncertain`, which says retrying is not safe: {:?}", core::str::from_utf8(&answer));
}

tagged_test!(a_bootstrap_role_is_refused_by_name_and_leaves_nothing_behind, [Service, Process], id = "kernel.services.a_bootstrap_role_is_refused_by_name_and_leaves_nothing_behind", covers = ["kernel", "services"]);
fn a_bootstrap_role_is_refused_by_name_and_leaves_nothing_behind() {
	use object::channel::{Channel, Message};
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	// THE RECEIVING HALF OF A BOOTSTRAP, WHICH NOTHING WAS TESTING. Every service reads its
	// capabilities as a fixed sequence of tagged messages, by hand, and the two ends agree only
	// because somebody keeps them agreeing. When three programs once read theirs in an order the
	// sender does not use, a blocking tagged read consumed the message that was actually next and
	// then waited forever for one nobody sends - and it surfaced 170 tests away.
	//
	// `rt::receive_roles` checks what a receiver CAN check: that every required role arrived, that
	// the tag in each position is the one declared, that a handle is the kernel object type its
	// role is, and that it carries the rights that role needs. It cannot check which protocol
	// answers on a channel, and does not pretend to.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let probe_elf = program_elf(&package, volume, b"role_probe").expect("role_probe");

	// Drive one case and read the probe's verdict: `tag:reason count`, where the count is how many
	// capabilities the probe still holds. A refusal must leave zero.
	let drive = |case: u8, send: &dyn Fn(&alloc::sync::Arc<Channel>)| -> alloc::vec::Vec<u8> {
		let (parent, child) = Channel::create();
		let (report, report_child) = Channel::create();
		let process = spawn_dynamic_test_process(sched::root_domain(), probe_elf, child);
		send_cap(&parent, &[case], report_child, Rights::ALL).expect("case selector and report channel");
		send(&parent);
		// DROPPED, so a role the caller chose not to send never arrives rather than never being
		// waited for. The probe answers on its own channel, which is why this can be let go.
		core::mem::drop(parent);
		for _ in 0..100_000 {
			sched::run_until_idle();
			if let Ok(reply) = report.recv() {
				return reply.bytes;
			}
			if process.is_terminated() {
				break;
			}
		}
		alloc::vec::Vec::new()
	};
	let channel_cap = |rights: Rights| -> object::handle::Capability {
		let (_keep, far) = Channel::create();
		core::mem::forget(_keep);
		object::handle::Capability::new(far as alloc::sync::Arc<dyn object::KernelObject>, rights)
	};
	let serving: Rights = Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER;

	// 1. EVERY ROLE PRESENT, including the optional one sent as a bare tag with no capability -
	//    which is the ordinary shape of a boot with no second disk, and a receiver that refused it
	//    would be one that cannot start on a smaller machine.
	let all = drive(0, &|parent| {
		for tag in [b"SERVE".as_slice(), b"STORAGE".as_slice()] {
			parent.send(Message::new(tag.to_vec(), alloc::vec![channel_cap(serving)])).expect("role");
		}
		parent.send(Message::new(b"MEDIA".to_vec(), alloc::vec::Vec::new())).expect("absent optional role");
	});
	assert_eq!(all.as_slice(), b"ok 2", "every declared role is accepted, and the absent optional one is not an error");

	// 2. A REQUIRED ROLE THAT NEVER ARRIVES. The peer closes instead of sending it.
	let missing = drive(1, &|parent| {
		parent.send(Message::new(b"SERVE".to_vec(), alloc::vec![channel_cap(serving)])).expect("first role");
	});
	assert_eq!(missing.as_slice(), b"STORAGE:role never arrived 0", "the missing role is named, and the one already taken is closed rather than kept");

	// 3. THE WRONG KIND OF OBJECT under a role that must be a channel. A memory object is not a
	//    channel, and the kernel will say so - which is the whole of what this layer can check.
	let wrong = drive(2, &|parent| {
		let object = MemoryObject::create(4096).expect("probe memory object");
		let cap = object::handle::Capability::new(object as alloc::sync::Arc<dyn object::KernelObject>, Rights::READ | Rights::MAP | Rights::TRANSFER);
		parent.send(Message::new(b"SERVE".to_vec(), alloc::vec![cap])).expect("wrong-type role");
	});
	assert_eq!(wrong.as_slice(), b"SERVE:role carried the wrong kind of object 0", "a memory object under a channel role is refused, and nothing is kept");

	// 4. THE RIGHT OBJECT WITH TOO FEW RIGHTS. A channel that cannot be received on is not one a
	//    service can serve its clients over, and finding that out at the first call rather than at
	//    bootstrap is the difference between a service that fails to start and one that starts
	//    broken.
	let thin = drive(3, &|parent| {
		parent.send(Message::new(b"SERVE".to_vec(), alloc::vec![channel_cap(Rights::SEND | Rights::TRANSFER)])).expect("thin role");
	});
	assert_eq!(thin.as_slice(), b"SERVE:role carried fewer rights than it needs 0", "a channel without RECEIVE cannot be served on, and is refused before ready");

	// 5. MORE RIGHTS THAN THE ROLE IS ALLOWED, which is the half that catches over-granting rather
	//    than under-granting - and it caught the system as it stood. A fresh channel end carries
	//    every right, and the supervisor transferred serve roots exactly as the kernel minted them,
	//    so every service held MANAGE, DUPLICATE and REVOKE over its own service channel. Refused
	//    rather than trimmed: a receiver that quietly narrowed what it was handed would hide the
	//    sender, and the sender is where it has to be fixed.
	let broad = drive(3, &|parent| {
		parent.send(Message::new(b"SERVE".to_vec(), alloc::vec![channel_cap(Rights::ALL)])).expect("over-granted role");
	});
	assert_eq!(broad.as_slice(), b"SERVE:role carried more rights than it is allowed 0", "a serve root carrying every right is refused");

	// 6. A ROLE ARRIVING OUT OF POSITION. The sequence is the contract: a tag in the wrong place
	//    has already displaced every read after it, so it is refused here rather than consumed.
	let disordered = drive(0, &|parent| {
		parent.send(Message::new(b"STORAGE".to_vec(), alloc::vec![channel_cap(serving)])).expect("role out of order");
	});
	assert_eq!(disordered.as_slice(), b"SERVE:a different role arrived in this position 0", "a tag out of order is refused rather than silently consumed");
}

tagged_test!(pty_hosts_a_program, [Service, Shell, Console], id = "kernel.services.pty_hosts_a_program", covers = ["kernel", "services", "term"]);
fn pty_hosts_a_program() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// The PTY abstraction: a program hosts a terminal it is not the hardware console for.
	// ConsoleService opens a pseudo-terminal on request, spawns a slave program on it, and
	// hands back the master channel; the host drives the slave through the line
	// discipline over that master, exactly as the `script` tool (and a future ssh) does.
	// Here we stand in for the host (and for VT 1's idle shell) and drive a `ptyecho` slave:
	// a line written to the master is cooked by the line discipline, delivered to the slave,
	// echoed back prefixed with "pty:", and forwarded out the master to us.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let init = init_package_bytes().expect("init package module not found");
	let console_elf = program_elf(&package, volume, b"console_service").expect("console_service in the package or volume");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("process_service.lsexe in the init package");

	// ConsoleService's bootstrap channel and the channels its __user_main expects: VT 1's
	// data (CLIENT) + control (CONTROL), a factory per service (FSTORAGE..FNET; only FPROCESS
	// is a live ProcessService here, which loads the ptyecho slave - the rest are unused, as
	// the slave needs no services), then GPU (none) and POINTER (none).
	let (boot_kernel, boot_user) = Channel::create();
	let (vt1_console_a, _vt1_console_b) = Channel::create();
	let (ctl_console, ctl_shell) = Channel::create();
	let (dummy_a, _dummy_b) = Channel::create();

	let _console_service = spawn_dynamic_test_process(sched::root_domain(), console_elf, boot_user);

	// A StorageService over the factory volume (which stages ptyecho under bin/), so the
	// ProcessService below can load the ptyecho slave from vol://system/bin/ptyecho.lsexe.
	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), storage_elf, storage_boot_user, Rights::ALL).expect("spawn StorageService");
	send_ramdisk(&storage_boot_kernel, volume).expect("storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");

	// A live ProcessService the console loads and launches the ptyecho slave through (the
	// sole process-creation mechanism), reading it from the system volume through the
	// StorageService client.
	let (proc_boot_kernel, proc_boot_user) = Channel::create();
	let (proc_server, proc_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), process_elf, proc_boot_user, Rights::ALL).expect("spawn ProcessService");
	send_package(&proc_boot_kernel, init).expect("process package bootstrap");
	send_cap(&proc_boot_kernel, b"STORAGE", storage_client, Rights::ALL).expect("process storage bootstrap");
	// The development registry with its far end dropped, so nothing answers and every
	// launch reads the volume. Handed over rather than skipped: the bootstrap consumes
	// one message per handoff in order, so omitting it swallows the SERVE channel.
	let (registry_server, registry_client) = Channel::create();
	core::mem::drop(registry_server);
	send_cap(&proc_boot_kernel, b"REGISTRY", registry_client, Rights::ALL).expect("process registry bootstrap");
	send_cap(&proc_boot_kernel, b"SERVE", proc_server, Rights::ALL).expect("process serve bootstrap");

	send_cap(&boot_kernel, b"CLIENT", vt1_console_a, Rights::ALL).expect("CLIENT bootstrap");
	send_cap(&boot_kernel, b"CONTROL", ctl_console, Rights::ALL).expect("CONTROL bootstrap");
	for tag in [&b"FSTORAGE"[..], &b"FLOG"[..], &b"FDEVICE"[..], &b"FPROCESS"[..], &b"FCONFIG"[..], &b"FTIME"[..], &b"FAUDIO"[..], &b"FSESSION"[..], &b"FPERM"[..], &b"FNET"[..]] {
		let factory: alloc::sync::Arc<dyn object::KernelObject> = if tag == b"FPROCESS" { proc_client.clone() } else { dummy_a.clone() };
		send_cap(&boot_kernel, tag, factory, Rights::ALL).expect("factory bootstrap");
	}
	boot_kernel.send(Message::new(b"GPU".to_vec(), alloc::vec::Vec::new())).expect("GPU bootstrap");
	boot_kernel.send(Message::new(b"POINTER".to_vec(), alloc::vec::Vec::new())).expect("POINTER bootstrap");
	boot_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("READY bootstrap");

	// stand in for the shell's PTY_OPEN request: ask the console to host a `ptyecho` slave
	// on a new pty.
	ctl_shell.send(Message::new(b"PTY_OPENptyecho".to_vec(), alloc::vec::Vec::new())).expect("PTY_OPEN request");

	sched::run_until_idle();

	// the console replies "PTY" + the master channel (the host side of the pty).
	let reply = ctl_shell.recv().expect("a PTY reply should arrive");
	assert_eq!(&reply.bytes[..3], b"PTY", "the console opens the pty");
	let cap = reply.caps.first().expect("the master channel is transferred");
	let master = cap.object().into_any_arc().downcast::<Channel>().expect("the master is a channel");

	// drive the slave: a line through the master is cooked and delivered, the slave echoes
	// it back prefixed, and the prefixed line is forwarded out the master back to us.
	master.send(Message::new(b"hello\n".to_vec(), alloc::vec::Vec::new())).expect("write to the pty master");
	sched::run_until_idle();

	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = master.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	assert!(captured.windows(b"pty:hello".len()).any(|w| w == b"pty:hello"), "the slave's reply is forwarded back out the master");
}

tagged_test!(the_console_answers_a_program_through_its_own_channel, [Service, Console, Display], id = "kernel.services.the_console_answers_a_program_through_its_own_channel", covers = ["kernel", "term", "bin.console_service"]);
fn the_console_answers_a_program_through_its_own_channel() {
	use object::channel::{Channel, Message};
	use object::dma_buffer::DmaBuffer;
	use object::rights::Rights;

	// THE HARNESS THE TERMINAL'S TESTS DID NOT HAVE.
	//
	// Every regression test for the terminal model calls `Screen` directly, which is why an item
	// could be checked off with a note describing a call site the console did not have: the OSC 52
	// clipboard query was finished in the model, tested in the model, and never wired. So was the
	// question of what happens when the program does not read its answers - the console delivered
	// them with an unbounded blocking send, which one program could use to stop every VT.
	//
	// Neither is visible from inside `Screen`. Both are visible from here: a real ConsoleService
	// with a real display behind it, and the test holding the channel a PROGRAM would hold - the
	// same end VT 1's shell is given. Bytes in are what a program prints; messages out are what it
	// reads on its input.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let display_elf = program_elf(&package, volume, b"display_service").expect("display_service in the package or volume");
	let console_elf = program_elf(&package, volume, b"console_service").expect("console_service in the package or volume");

	// A DisplayService over a stand-in scanout, so VT 1 has a grid: a terminal with no framebuffer
	// has no `Screen` at all, and the whole escape-sequence path is skipped.
	let (display_boot_kernel, display_boot_user) = Channel::create();
	let (display_server, display_client) = Channel::create();
	let (gpu_kernel, gpu_user) = Channel::create();
	let (focus_input, focus_display) = Channel::create();
	let (kill_input, kill_display) = Channel::create();
	let _display_service = spawn_dynamic_test_process(sched::root_domain(), display_elf, display_boot_user);
	send_cap(&display_boot_kernel, b"FOCUS", focus_display, Rights::ALL).expect("focus bootstrap");
	send_cap(&display_boot_kernel, b"KILL", kill_display, Rights::ALL).expect("kill bootstrap");
	let (_display_admin, admin) = Channel::create();
	send_cap(&display_boot_kernel, b"ADMIN", admin, Rights::ALL).expect("display admin bootstrap");
	send_cap(&display_boot_kernel, b"SERVE", display_server, Rights::ALL).expect("serve bootstrap");
	display_boot_kernel.send(Message::new(b"DISPLAYCTL".to_vec(), alloc::vec::Vec::new())).expect("display capability bootstrap");
	// AND THE PROVIDER CATALOGUE LAST - see the note on the other DisplayService harness in this
	// file: this service discovers its scanout rather than being handed one.
	let (display_catalogue_server, display_catalogue_client) = Channel::create();
	send_cap(&display_boot_kernel, b"CATALOGUE", display_catalogue_client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the catalogue channel");
	// AN OPTIONAL ROLE'S TAG STILL TRAVELS, carrying nothing: the bootstrap is read POSITIONALLY and
	// in order, so a harness that answered the subscription first would wait for one this service
	// cannot send until it has read everything before it.
	display_boot_kernel.send(Message::new(b"STATS".to_vec(), alloc::vec::Vec::new())).expect("display stats bootstrap");
	sched::run_until_idle();
	crate::tests::serve_provider_catalogue(&display_catalogue_server, device_proto::generated::liber::device::v1::ProviderKind::Display, gpu_user).expect("the catalogue answered the subscription and the connection");

	// 160x64 B8G8R8X8: 20 columns by 4 rows at this font, which is a grid a query can be asked on.
	const FB_W: u32 = 160;
	const FB_H: u32 = 64;
	sched::run_until_idle();
	let fb_request = gpu_kernel.recv().expect("framebuffer request");
	assert_eq!(le_u16(&fb_request.bytes, 0), 1, "DisplayService asks the device for its scanout");
	let scanout = match DmaBuffer::create_in(&sched::root_domain(), (FB_W * FB_H * 4) as usize) {
		Ok(scanout) => scanout,
		Err(_) => panic!("stand-in scanout"),
	};
	let fb_reply = crate::tests::scanout_reply(le_u32(&fb_request.bytes, 2), FB_W, FB_H, (FB_W * FB_H * 4) as u64);
	send_cap(&gpu_kernel, &fb_reply, scanout, Rights::READ | Rights::WRITE | Rights::MAP | Rights::TRANSFER).expect("framebuffer response");
	sched::run_until_idle();
	// The device's event stream, which this stand-in answers and then holds: a service whose stream
	// ends releases the device.
	let _device_events = crate::tests::answer_device_events(&gpu_kernel);
	sched::run_until_idle();
	let online = display_boot_kernel.recv().expect("DisplayService online report");
	assert_eq!(&online.bytes[..], b"DisplayService: online", "DisplayService reports in");

	// Every synchronous present the console makes goes to the gpu and waits for the acknowledgement,
	// so the stand-in gpu has to answer them or the console parks mid-frame. Drains whatever is
	// pending; the console presents once per output batch and not at all when nothing changed.
	let ack_presents = |gpu: &Channel| {
		while let Ok(message) = gpu.recv() {
			if le_u16(&message.bytes, 0) == 2 {
				let mut reply = le_u32(&message.bytes, 2).to_le_bytes().to_vec();
				reply.push(1);
				gpu.send(Message::new(reply, alloc::vec::Vec::new())).expect("present acknowledgement");
			}
			sched::run_until_idle();
		}
	};
	// AND THE FOCUS HANDSHAKE, which is what an acquire actually blocks on: DisplayService tells
	// InputService which surface owns the keyboard and waits for the acknowledgement before it
	// answers the client. Nothing here is InputService, so the test is - and without this the console
	// never finished bring-up and never reported in, which is a hang in the harness rather than
	// anything the console did.
	let ack_focus = |focus: &Channel| {
		while let Ok(_command) = focus.recv() {
			focus.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("focus acknowledgement");
			sched::run_until_idle();
		}
	};

	// ConsoleService, with VT 1's data channel held HERE - the end a program reads its input on.
	let (console_boot_kernel, console_boot_user) = Channel::create();
	let (vt1_console, vt1_program) = Channel::create();
	let (ctl_console, _ctl_program) = Channel::create();
	let (dummy, _dummy_far) = Channel::create();
	let _console_service = spawn_dynamic_test_process(sched::root_domain(), console_elf, console_boot_user);
	send_cap(&console_boot_kernel, b"CLIENT", vt1_console, Rights::ALL).expect("CLIENT bootstrap");
	send_cap(&console_boot_kernel, b"CONTROL", ctl_console, Rights::ALL).expect("CONTROL bootstrap");
	for tag in [&b"FSTORAGE"[..], &b"FLOG"[..], &b"FDEVICE"[..], &b"FPROCESS"[..], &b"FCONFIG"[..], &b"FTIME"[..], &b"FAUDIO"[..], &b"FSESSION"[..], &b"FPERM"[..], &b"FNET"[..]] {
		send_cap(&console_boot_kernel, tag, dummy.clone(), Rights::ALL).expect("factory bootstrap");
	}
	send_cap(&console_boot_kernel, b"DISPLAY", display_client, Rights::ALL).expect("DISPLAY bootstrap");
	console_boot_kernel.send(Message::new(b"POINTER".to_vec(), alloc::vec::Vec::new())).expect("POINTER bootstrap");
	console_boot_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("READY bootstrap");
	// SEVERAL SETTLES, not one. `run_until_idle` returns when nothing is RUNNABLE, and bring-up
	// crosses timed waits - the bounded wait for a ConfigService that is not there, and the display
	// round trips - so a thread parked on a deadline leaves the loop with the work unfinished. The
	// pty harness gets away with one because it drives the console again afterwards.
	let settle = |gpu: &Channel, focus: &Channel| {
		for _ in 0..8 {
			sched::run_until_idle();
			ack_focus(focus);
			ack_presents(gpu);
		}
	};
	settle(&gpu_kernel, &focus_input);
	// The console reports in when it is up, and it does that AFTER acquiring its surface - so this
	// also proves VT 1 has a grid to parse escape sequences into. Without a display the terminal is
	// `None` and the whole escape path is skipped, which would make every assertion below fail for a
	// reason that has nothing to do with what they are testing.
	let online = console_boot_kernel.recv().expect("ConsoleService online report");
	assert_eq!(&online.bytes[..], b"ConsoleService: online", "ConsoleService reports in");

	// Print bytes as a program would, then read what the console owes it back.
	let print = |bytes: &[u8]| {
		vt1_program.send(Message::new(bytes.to_vec(), alloc::vec::Vec::new())).expect("program output");
		settle(&gpu_kernel, &focus_input);
	};
	let read_input = || -> alloc::vec::Vec<u8> {
		let mut out = alloc::vec::Vec::new();
		while let Ok(message) = vt1_program.recv() {
			out.extend_from_slice(&message.bytes);
		}
		out
	};

	// A cursor-position report, which is the reply path that already worked - asserted here so the
	// harness itself is proved before it is used on the path that did not.
	print(b"\x1b[6n");
	let answer = read_input();
	assert!(answer.starts_with(b"\x1b["), "the console answers DSR on the program's own channel: {answer:?}");
	assert!(answer.ends_with(b"R"), "and it is a cursor position report: {answer:?}");

	// THE CLIPBOARD QUERY, END TO END. The model recorded the query and could produce the answer;
	// nothing drained it, so a program asking for the selection was answered with silence.
	print(b"\x1b]52;c;aGVsbG8=\x07"); // the program sets the selection to "hello"
	let _ = read_input();
	print(b"\x1b]52;c;?\x07"); // and asks for it back
	let answer = read_input();
	assert_eq!(answer, b"\x1b]52;c;aGVsbG8=\x1b\\".to_vec(), "the console answers the clipboard query with the selection it holds");

	// A PROGRAM THAT NEVER READS MUST NOT STOP THE CONSOLE. The reply was delivered with an
	// unbounded blocking send, so a program emitting queries and not draining its input filled its
	// channel and the console then waited inside the render of one VT - stopping every other VT,
	// the input path, the pointer path and the display. Nothing here drains `vt1_program` while the
	// queries are sent, so the channel fills; the console must carry on regardless.
	for _ in 0..300 {
		print(b"\x1b[6n");
	}
	let flooded = read_input();
	assert!(!flooded.is_empty(), "the answers that fitted were delivered");
	// And the console is still running: a fresh query on a drained channel is still answered.
	print(b"\x1b[6n");
	let after = read_input();
	assert!(after.ends_with(b"R"), "the console still answers after a client stopped reading: {after:?}");

	core::mem::drop(kill_input);
}

tagged_test!(ps_live_view_drives_the_terminal_contract, [Service, Shell, Console], id = "kernel.services.ps_live_view_drives_the_terminal_contract", covers = ["kernel", "term"]);
fn ps_live_view_drives_the_terminal_contract() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// `ps -i`: the live process/resource view runs full-screen on its controlling
	// terminal - it must enter the alternate screen, hide the cursor and flip the tty
	// raw (the ESC[?1049h / ?25l private modes ConsoleService's terminal
	// honours), redraw a snapshot in place, quit on a raw `q` keystroke, and restore
	// every mode on the way out. Here we stand in for the terminal and both granted
	// services: the service channels answer garbage (so each query degrades to its
	// "unavailable" row - the terminal contract is what is under test), and a raw `q`
	// is queued so the first frame's key check quits the loop.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let ps_elf = program_elf(&package, volume, b"ps").expect("ps should be staged");

	let (boot_kernel, boot_user) = Channel::create();
	let (console_host, console_child) = Channel::create();
	let (res_host, res_child) = Channel::create();
	let (proc_host, proc_child) = Channel::create();
	let _ps = spawn_dynamic_test_process(sched::root_domain(), ps_elf, boot_user);
	send_cap(&boot_kernel, b"STDOUT", console_child, Rights::ALL).expect("STDOUT bootstrap");
	boot_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("endpoint run terminator");
	boot_kernel.send(Message::new(crate::tests::launch_context(b"-i", b""), alloc::vec::Vec::new())).expect("argv bootstrap");
	send_cap(&boot_kernel, b"RESOURCE", res_child, Rights::ALL).expect("RESOURCE bootstrap");
	send_cap(&boot_kernel, b"PROCESS", proc_child, Rights::ALL).expect("PROCESS bootstrap");
	sched::run_until_idle();

	// the first frame queries the process list; answer garbage so it renders the
	// unavailable row, queue the quitting keystroke, then answer the budgets query.
	let _list_req = proc_host.recv().expect("the live view should query the process list");
	proc_host.send(Message::new(b"?".to_vec(), alloc::vec::Vec::new())).expect("the garbage list reply should send");
	console_host.send(Message::new(b"q".to_vec(), alloc::vec::Vec::new())).expect("the raw q keystroke should send");
	sched::run_until_idle();
	let _usage_req = res_host.recv().expect("the live view should query the budgets");
	res_host.send(Message::new(b"?".to_vec(), alloc::vec::Vec::new())).expect("the garbage usage reply should send");
	sched::run_until_idle();

	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = console_host.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	let contains = |needle: &[u8]| captured.windows(needle.len()).any(|w| w == needle);
	// The alternate screen and the cursor are STILL escapes - they are the terminal's own state and
	// a program printing them affects only its own screen. The tty's raw and echo modes are not
	// here any more: those went out over the control channel, because a program's data and a
	// program's request were the same bytes and `cat` on the wrong file reconfigured the terminal.
	assert!(contains(b"\x1b[?1049h\x1b[?25l"), "the live view should enter the alternate screen and hide the cursor");
	assert!(contains(b"live process / resource view"), "the live view should render its header");
	assert!(contains(b"unavailable"), "the degraded queries should render their unavailable rows");
	assert!(contains(b"\x1b[?1049l"), "quitting on q should leave the alternate screen");
}

tagged_test!(storage_serves_volume_file_to_client, [Service, Storage], id = "kernel.services.storage_serves_volume_file_to_client", covers = ["kernel", "liberfs", "storage"]);
fn storage_serves_volume_file_to_client() {
	// The StorageService (a ring-3 process) maps a ramdisk volume, and a client
	// process opens vol://system/hello.txt through it, receives a shared-buffer
	// capability to the file's bytes, maps it, and reports the contents back. The
	// bytes the client read must equal the file straight from the volume archive -
	// an end-to-end, capability-brokered, zero-copy read across two userspace
	// processes coordinated only by IPC.
	let (expected, actual) = run_storage_scenario().expect("the storage scenario should run");
	assert!(!expected.is_empty(), "the volume file should not be empty");
	assert_eq!(actual, expected);
}

tagged_test!(resource_manager_contains_a_domain, [Service, Domain], id = "kernel.services.resource_manager_contains_a_domain", covers = ["kernel", "services", "bin.resource_manager"]);
fn resource_manager_contains_a_domain() {
	// The ResourceManager creates a bounded sub-Domain, launches resource_probe into it, and
	// caps the Domain's memory at four one-page objects above the probe's baseline. It drives
	// the probe to fill the budget (four objects fit) and be refused the fifth - that
	// over-budget allocation fails with RESOURCE_EXHAUSTED, contained to the offending Domain
	// rather than crashing the probe (which survives and answers) or the system. The manager
	// then raises the cap by another four pages at runtime and drives the probe into the new
	// headroom (four more fit). The budget summary must show exactly that: four pages granted
	// under the cap, one contained refusal survived, and four pages regranted after the
	// runtime raise - the kernel enforced the per-Domain budget and the policy adjusted it
	// live.
	let summary = run_resource_scenario().expect("the resource scenario should run");
	assert_eq!(summary.as_slice(), b"granted=4 denied=1 regranted=4", "the kernel enforced the Domain's memory budget, contained the over-budget refusal, and honored the runtime raise");
}

tagged_test!(kernel_reads_file_through_storage_service, [Service, Storage], id = "kernel.services.kernel_reads_file_through_storage_service", covers = ["kernel", "liberfs", "storage", "bin.storage_service"]);
fn kernel_reads_file_through_storage_service() {
	// The kernel drives the StorageService as its own client, sending one open request
	// and a quit sentinel, then reads the returned shared buffer. The bytes must equal
	// the file straight from the volume archive - a round-trip to a real userspace
	// service.
	let expected = pkg::Package::parse(volume_package_bytes().expect("the volume package should be present")).and_then(|p| p.lookup(b"hello.txt").map(|b| b.to_vec())).expect("hello.txt should be in the volume");
	let actual = storage_read(b"vol://system/hello.txt").expect("the storage read should succeed");
	assert!(!expected.is_empty(), "the volume file should not be empty");
	assert_eq!(actual, expected);
}

tagged_test!(storage_serves_staged_tool_binary, [Service, Storage], id = "kernel.services.storage_serves_staged_tool_binary", covers = ["kernel", "liberfs", "storage"]);
fn storage_serves_staged_tool_binary() {
	// The tool ELFs are staged onto the system volume under bin/ by the
	// factory-seed pipeline (build.rs strips them into the volume archive, the boot runner
	// lays that archive at LBA 0, and StorageService seeds it into the freshly-formatted
	// LiberFS). Reading one back through StorageService must return a valid ELF image -
	// proof the whole staging path works end to end.
	let actual = storage_read(b"vol://system/bin/cat.lsexe").expect("the staged tool read should succeed");
	assert!(actual.len() > 4, "the staged tool should not be empty");
	assert_eq!(&actual[..4], b"\x7fELF", "the staged tool should be an ELF image");
}

// THE INTERACTIVE 2D DEMO, AGAINST A REAL DISPLAYSERVICE.
//
// WHAT THE CONFORMANCE RUN CANNOT SHOW. That one walks the profile in memory it allocated itself and
// answers "is each feature implemented"; this one draws a real scene into a real surface's images,
// through a real present queue, and answers the other half: that a drawing made of those features
// reaches a screen, that its DAMAGE is what it changed rather than what it feels like, and that a
// generation change under it is survived rather than presented over.
//
// THE TWO RECTANGLES ARE THE POINT OF THE MIDDLE PHASE. An application that changes two distant
// regions has to reach the driver as TWO transfers - a damage model that unions them covers the
// screen between, which is the whole cost the model exists to avoid - and the only place that is
// checkable is here, at the device end of the whole path.
tagged_test!(the_2d_demo_draws_a_real_scene_with_real_damage, [Service, Display, Process, Image], id = "kernel.services.the_2d_demo_draws_a_real_scene_with_real_damage", covers = ["bin.test2d-sw", "render2d", "soft2d", "graphics-app", "surface"]);
fn the_2d_demo_draws_a_real_scene_with_real_damage() {
	use display_harness::*;

	// A SCANOUT BIG ENOUGH FOR THE SCENE'S OWN GEOMETRY. The patches the multi-rect phase changes sit
	// twenty-four pixels in from two opposite corners, so a surface smaller than that would make the
	// phase's two rectangles one.
	let harness = display_harness::start(192, 128);
	let console_client = harness.console;
	let focus_input = harness.focus;
	let gpu_kernel = harness.gpu;
	let device_events = harness.device_events.expect("DisplayService opened the device's event stream");
	let _display_service = harness.service;
	let _boot_kernel = harness.boot;
	let stats_root = harness.stats;
	let admin = harness.admin;
	let _scanout = harness.scanout;

	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init_package_bytes().expect("init package module not found")).expect("init package parses");
	let demo_elf = program_elf(&package, volume, b"test2d-sw").expect("test2d-sw in the package or volume");
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let display = connect(&console_client);
	let process = spawn_dynamic_test_process(sched::root_domain(), demo_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("the demo's console");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("endpoint run terminator");
	// THE DETERMINISTIC CONTROLS, which are launch ARGUMENTS and not keys: a key that changed the run
	// would be a key a person could press by accident. Three frames a phase is enough to see each
	// phase's damage shape, and the run ends on its own frame count rather than on this loop's.
	bootstrap.send(Message::new(crate::tests::launch_context(b"--frames=240 --phase-frames=3 --no-input", b"vol://system"), alloc::vec::Vec::new())).expect("the demo's launch context");
	send_cap(&bootstrap, b"DISPLAY", display, Rights::ALL).expect("the demo's display");

	let mut output: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	let mut whole_surface_presents = 0usize;
	let mut two_rectangle_presents = 0usize;
	let mut presents = 0usize;
	let mut resized = false;
	// THE SCALE CHANGE, WHICH IS THE ONE PHASE A COUNTER CANNOT REACH AND A RESIZE IS NOT. It is
	// driven through the display ADMIN channel, because the scale is the system's to choose: a client
	// that could set it would be deciding how much memory every other client's images need.
	let mut scaled = false;
	let mut scale_acknowledged = false;
	// THE DEMO'S OWN CHARGED MEMORY, SAMPLED TWICE INSIDE ONE GENERATION. What a steady frame loop
	// may not do is GROW: the queue and the resources exist after the first frames, and a loop that
	// charged another page every frame would be a drawing that cannot run for an hour. Both samples
	// are taken before the resize, because a new generation legitimately allocates a new set of
	// images - measuring across one would be measuring the rebuild.
	let mut settled_bytes: Option<u64> = None;
	let mut later_bytes: Option<u64> = None;
	let mut thief: Option<Arc<Channel>> = None;
	let mut thief_requested = false;
	let mut presents_when_hidden = 0usize;
	let mut hidden_passes = 0u32;
	let mut hidden_presents = usize::MAX;
	for _ in 0..80_000u32 {
		// A BOUNDED DRAIN, for the reason the frame-loop gate states: `run_until_idle` sleeps to the
		// nearest thread deadline and keeps going, so against a client that paces itself it never
		// returns.
		sched::run_until_idle_until(arch::apic::ticks().saturating_add(1));
		if focus_input.recv().is_ok() {
			focus_input.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("focus acknowledgement");
		}
		// EVERY PRESENT THE DEVICE SEES, WITH ITS RECTANGLES. This is the end of the path the damage
		// travelled, and counting the SHAPES here is what the middle phase is for.
		while let Ok(present) = gpu_kernel.recv() {
			// THE DEVICE'S OWN VIEW OF THE DAMAGE, read the way the device reads it: the operation,
			// the correlation, the backing generation, and then the region.
			let mut reader = wire::Reader::new(&present.bytes);
			let _op = reader.u16();
			let _corr = reader.u32();
			let _generation = reader.u32();
			if let Some(region) = display_device::DamageRegion::read(&mut reader) {
				match region.rects.len() {
					2 => two_rectangle_presents += 1,
					1 => {
						let rect = &region.rects[0];
						// A WHOLE-SURFACE PRESENT IS ONE RECTANGLE COVERING THE SURFACE, which is
						// what `Full` becomes by the time the device sees it - and is a different
						// thing from a one-rectangle partial update.
						if rect.size.width >= 96 && rect.size.height >= 64 {
							whole_surface_presents += 1;
						}
					}
					_ => {}
				}
			}
			let mut reply = le_u32(&present.bytes, 2).to_le_bytes().to_vec();
			reply.push(1);
			gpu_kernel.send(Message::new(reply, alloc::vec::Vec::new())).expect("present acknowledgement");
			presents += 1;
		}
		while let Ok(message) = stdout.recv() {
			output.extend_from_slice(&message.bytes);
		}
		// TAKE THE SCREEN AWAY FROM IT, AND GIVE IT BACK. A second surface from another connection
		// becomes the visible one, which is the transition an application has to survive: while it is
		// in the background it may draw NOTHING - frames accepted while hidden are discarded in order
		// and never reach a screen, so a loop that drew them would be doing work that is thrown away.
		if !thief_requested && presents >= 6 {
			let mut writer = wire::VecWriter::new();
			display_v1::SurfaceRequest { logical_extent: display_v1::Extent2d { width: 32, height: 32 }, images: 2 }.write(&mut writer).expect("a surface request encodes");
			let body = writer.into_inner().expect("a surface request carries no capability");
			console_client.send(request(display::OP_CREATE_SURFACE, 900, &body)).expect("a create-surface request");
			thief_requested = true;
		}
		if thief_requested
			&& thief.is_none()
			&& let Ok(reply) = console_client.recv()
		{
			succeeded(&reply, 900);
			let taken: Arc<Channel> = reply.caps.first().expect("the surface capability").object().into_any_arc().downcast::<Channel>().expect("a surface is a channel");
			presents_when_hidden = presents;
			// AND THE OTHER SURFACE PRESENTS WHILE IT OWNS THE SCREEN, which is the half of the
			// multi-surface claim a background client cannot make: two surfaces, each drawing while
			// it is the visible one, on one connection's worth of service state.
			//
			// EVERY DRAIN HERE IS BOUNDED. The ordinary surface helpers call `run_until_idle`, which
			// against a client pacing itself on a timer - and the demo is one, still running - does
			// not return; so this sequence pumps a tick at a time and answers the focus and device
			// channels on every pass, which is what the service is blocked on in between.
			let pump = |waiting_for: &Channel, saw_present: &mut bool| -> Option<Message> {
				for _ in 0..40_000u32 {
					sched::run_until_idle_until(arch::apic::ticks().saturating_add(1));
					if focus_input.recv().is_ok() {
						focus_input.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("focus acknowledgement");
					}
					while let Ok(present) = gpu_kernel.recv() {
						let mut reply = le_u32(&present.bytes, 2).to_le_bytes().to_vec();
						reply.push(1);
						gpu_kernel.send(Message::new(reply, alloc::vec::Vec::new())).expect("present acknowledgement");
						*saw_present = true;
					}
					if let Ok(message) = waiting_for.recv() {
						return Some(message);
					}
				}
				None
			};
			let mut ignored = false;
			taken.send(request(surface::OP_CONFIGURATION, 910, &[])).expect("a configuration request");
			let reply = pump(&taken, &mut ignored).expect("the taken surface answers its configuration");
			let configuration = display_v1::SurfaceConfiguration::decode(succeeded(&reply, 910)).expect("a configuration snapshot decodes");
			assert!(configuration.visible, "the surface that just took the screen is the visible one");
			taken.send(request(surface::OP_ACK_CONFIGURE, 911, &configuration.serial.to_le_bytes())).expect("an acknowledgement");
			let acknowledged = pump(&taken, &mut ignored).expect("the acknowledgement is answered");
			succeeded(&acknowledged, 911);
			taken.send(request(surface::OP_QUEUE, 912, &[])).expect("a queue request");
			let queue_reply = pump(&taken, &mut ignored).expect("the queue is answered");
			let body = succeeded(&queue_reply, 912);
			let mut placeholders = wire::Handles::new();
			placeholders.push(1).expect("a stand-in producer handle");
			placeholders.push(2).expect("a stand-in completion handle");
			let mut reader = wire::Reader::with_handle_list(body, &placeholders);
			let queue = display_v1::PresentQueue::read(&mut reader).expect("a present queue decodes");
			let length: u64 = u64::from(queue.pitch) * u64::from(configuration.physical_extent.height);
			let mut supplied: alloc::vec::Vec<Arc<MemoryObject>> = alloc::vec::Vec::new();
			for index in 0..queue.images {
				let object = MemoryObject::create(length as usize).expect("a presentable image");
				let mut body = index.to_le_bytes().to_vec();
				body.extend_from_slice(&0u32.to_le_bytes());
				send_cap(&taken, &request(surface::OP_PROVIDE_IMAGE, 920 + index, &body).bytes, object.clone(), Rights::READ | Rights::MAP | Rights::TRANSFER).expect("a provide-image request");
				let provided = pump(&taken, &mut ignored).expect("a provide-image is answered");
				succeeded(&provided, 920 + index);
				supplied.push(object);
			}
			taken.send(request(surface::OP_ACQUIRE_NEXT, 930, &[])).expect("an acquire");
			let acquired_reply = pump(&taken, &mut ignored).expect("an acquire is answered");
			let acquired = display_v1::AcquiredImage::decode(succeeded(&acquired_reply, 930)).expect("an acquire answers one of the four");
			let display_v1::AcquiredImage::Image(image) = acquired else { panic!("the visible surface has an image to draw into, and answered {acquired:?}") };
			let mut reached_the_device = false;
			taken.send(request(surface::OP_PRESENT, 931, &present_body(image, &configuration, &damage(&[(0, 0, 8, 8)])))).expect("a present");
			let presented = pump(&taken, &mut reached_the_device).expect("a present is answered");
			succeeded(&presented, 931);
			assert!(reached_the_device, "the frame of the surface that owns the screen reaches the device");
			thief = Some(taken);
			let _held = supplied;
		}
		// AND THE MOMENT IT COMES BACK IS THE MOMENT THE THIEF GOES. Dropping the channel ends the
		// surface, and the demo's own surface is the visible one again.
		if thief.is_some() && presents_when_hidden != 0 {
			hidden_passes += 1;
			if hidden_passes > 400 {
				hidden_presents = presents - presents_when_hidden;
				drop(thief.take());
			}
		}
		if presents >= 4 && settled_bytes.is_none() {
			settled_bytes = Some(process.domain().account().memory().used());
		}
		if presents >= 10 && later_bytes.is_none() {
			later_bytes = Some(process.domain().account().memory().used());
		}
		// RESIZE UNDER IT ONCE IT HAS DRAWN THROUGH ITS FIRST PHASES. A changed extent is a new
		// generation: every image of the old one is stale, and what this checks is that the demo
		// rebuilds and goes back to a WHOLE-surface damage rather than presenting a partial update
		// computed for the size it no longer has.
		// AFTER THE PHASES THAT COME BEFORE IT HAVE DRAWN, and after the screen has been taken away
		// and given back. Three frames each for full, partial and multi-rect is nine, so a resize
		// before the twelfth present would cut the middle phase off at its first frame - which is a
		// gate that passes without ever seeing the thing it is for.
		if !resized && presents >= 12 && hidden_presents != usize::MAX {
			let mut frame = [0u8; 64];
			let mut handles = wire::Handles::new();
			let event = display_device::DeviceEvent::Resized(display_device::Extent2d { width: 160, height: 112 });
			let len = display_device::display_device::events_frame(0, &event, &mut frame, &mut handles).expect("a device event encodes");
			device_events.send(Message::new(frame[..len].to_vec(), alloc::vec::Vec::new())).expect("gpu resize event");
			resized = true;
		}
		// AND THEN THE SCALE, ONCE THE RESIZE HAS LANDED. The order matters: both are a rebuild, and
		// what tells them apart is the configuration - the same LOGICAL extent with a different ratio
		// is a scale change and nothing else is - so the resize has to have been seen and named
		// before this one arrives, or the two would be one transition the demo could not attribute.
		if !scaled && resized && output.windows(23).any(|window| window == b"test2d-sw: phase resize") {
			let mut writer = wire::VecWriter::new();
			display_v1::ScaleRatio { numerator: 2, denominator: 1 }.write(&mut writer).expect("a scale ratio encodes");
			let body = writer.into_inner().expect("a scale ratio carries no capability");
			admin.send(request(display_admin::OP_SET_SCALE, 910, &body)).expect("a set-scale request");
			scaled = true;
		}
		if scaled
			&& !scale_acknowledged
			&& let Ok(reply) = admin.recv()
		{
			succeeded(&reply, 910);
			scale_acknowledged = true;
		}
		if process.is_terminated() && resized {
			break;
		}
	}
	while let Ok(message) = stdout.recv() {
		output.extend_from_slice(&message.bytes);
	}
	for line in output.split(|byte| *byte == b'\n') {
		if !line.is_empty() {
			crate::serial_println!("  {}", alloc::string::String::from_utf8_lossy(line));
		}
	}
	let contains = |needle: &[u8]| output.windows(needle.len()).any(|window| window == needle);

	assert!(process.is_terminated(), "the demo ran to completion rather than being cut off: {presents} device present(s), {whole_surface_presents} whole, {two_rectangle_presents} two-rect, output={output:?}");
	assert!(contains(b"test2d-sw: open"), "it opened a surface: {output:?}");
	// THE PHASES IT WENT THROUGH, NAMED. A demo whose phases are a comment is a demo whose damage
	// nobody can attribute to a decision.
	assert!(contains(b"test2d-sw: phase partial"), "it reached the partial-damage phase: {output:?}");
	assert!(contains(b"test2d-sw: phase multi-rect"), "and the multi-rect one: {output:?}");
	assert!(contains(b"test2d-sw: phase resize"), "and the resize, which a REBUILD enters and no counter does: {output:?}");
	// AND THE SCALE, WHICH IS A REBUILD TOO AND IS NOT A RESIZE. The demo tells them apart from the
	// configuration alone - the same logical extent with a different ratio - so reaching this phase
	// is proof of both halves: that the service held the logical size across the change, and that it
	// gave the client a new PHYSICAL extent to resolve its edges at.
	assert!(scale_acknowledged, "the display service accepted the scale change");
	assert!(contains(b"test2d-sw: phase scale"), "and the demo entered the scale phase, with its layout unchanged and its edges resolved at the new physical resolution: {output:?}");
	// THE WHOLE POINT, AT THE DEVICE END: two distant regions arrive as TWO rectangles.
	assert!(two_rectangle_presents > 0, "the multi-rect phase's two rectangles reached the driver as two: {presents} presents, {output:?}");
	// AND A FULL FRAME IS STILL A FULL FRAME, which is what the first phase and every generation's
	// first frame are.
	assert!(whole_surface_presents > 0, "the full phases presented the whole surface: {presents} presents");
	// THE SECOND SURFACE, which is the object model's own proof: opened, presented to, and closed
	// with the first one's resources and generation surviving it.
	assert!(!contains(b"second=0 "), "the second surface presented: {output:?}");
	// THE SCREEN CHANGED HANDS AND THE DEMO NOTICED. A client that kept drawing while hidden and one
	// that never heard look identical in a frame count, which is why both halves are asserted: the
	// transitions it SAW, and that nothing of its reached the device while it was in the background.
	assert!(!contains(b"visibility=0 "), "the demo saw the screen change hands: {output:?}");
	assert!(!contains(b"background=0 "), "and idled while it was not the visible surface: {output:?}");
	assert_eq!(hidden_presents, 0, "and presented nothing at all while another surface owned the screen");
	assert!(contains(b"test2d-sw: done"), "and the run ended cleanly: {output:?}");
	// AND IT DID NOT GROW WHILE IT DREW. Six frames apart, inside one generation, with the queue and
	// every resource already made: what this refuses is the loop that charges another page per frame.
	if let (Some(settled), Some(later)) = (settled_bytes, later_bytes) {
		assert!(later <= settled, "the demo's charged memory grew from {settled} to {later} bytes across six frames of a steady loop");
	}

	// AND WHAT THE DEMO HELD IS GONE. A demo that took the screen and left its surfaces behind would
	// leave a system with no visible console - which a person sees as a machine that died - so the
	// service's OBSERVATION root is asked what the display path still holds once the process has
	// ended. The reclamation is the process's death and not a polite close: the demo exits without
	// destroying anything, which is exactly what a crash looks like from here.
	for _ in 0..4_000u32 {
		sched::run_until_idle_until(arch::apic::ticks().saturating_add(1));
		if focus_input.recv().is_ok() {
			focus_input.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("focus acknowledgement");
		}
		while let Ok(present) = gpu_kernel.recv() {
			let mut reply = le_u32(&present.bytes, 2).to_le_bytes().to_vec();
			reply.push(1);
			gpu_kernel.send(Message::new(reply, alloc::vec::Vec::new())).expect("present acknowledgement");
		}
		if display_resources(&stats_root, 800).surfaces == 0 {
			break;
		}
	}
	let held = display_resources(&stats_root, 801);
	assert_eq!(held.surfaces, 0, "every surface the demo held is reclaimed when its process ends: {held:?}");
	assert_eq!(held.present_images, 0, "and so is every image of every queue it supplied");
}
