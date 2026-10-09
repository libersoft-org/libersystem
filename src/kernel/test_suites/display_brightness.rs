//! Actual DisplayService wait-loop behavior with independently controlled backlight endpoints.

use super::display_harness::*;
use super::*;
use device_proto::generated::liber::device::v1 as device;
use display_device_proto::generated::liber::display_device::v1 as backlight_wire;
use display_v1::{BacklightState, BrightnessTarget, display_brightness, display_brightness_control};
use wire::Sink as _;

fn pump() {
	// An ordinary run_until_idle would advance straight to the very deadline the test is holding.
	sched::run_until_idle_until(arch::apic::ticks().saturating_add(1));
}

fn receive(channel: &Channel, why: &str) -> Message {
	for _ in 0..100 {
		if let Ok(message) = channel.recv() {
			return message;
		}
		pump();
	}
	panic!("no reply: {why}")
}

fn reply(channel: &Channel, corr: u32, body: impl FnOnce(&mut wire::VecWriter)) {
	let mut writer = wire::VecWriter::new();
	writer.u32(corr).unwrap();
	writer.u8(1).unwrap();
	body(&mut writer);
	channel.send(Message::new(writer.into_inner().unwrap(), alloc::vec::Vec::new())).expect("provider reply");
}

fn snapshot(read: &Channel, corr: u32) -> alloc::vec::Vec<BacklightState> {
	read.send(request(display_brightness::OP_BACKLIGHTS, corr, &[])).unwrap();
	let message = receive(read, "brightness snapshot");
	let mut reader = wire::Reader::new(succeeded(&message, corr));
	let count = reader.u16().expect("snapshot list count");
	let states = (0..count).map(|_| BacklightState::read(&mut reader).expect("complete backlight state")).collect();
	reader.finish().expect("the whole snapshot decoded");
	states
}

fn announce(stream: &Channel, slot: u32, live: bool) {
	let info = device::ProviderInfo { kind: device::ProviderKind::Backlight, bus: 0, dev: 4, func: 0, binding_generation: 1, slot, provider_generation: 1, name: alloc::string::String::new(), live, platform: None };
	let mut frame = [0u8; 128];
	let mut handles = wire::Handles::new();
	let len = device::provider_catalogue::subscribe_frame(slot, &info, &mut frame, &mut handles).unwrap();
	stream.send(Message::new(frame[..len].to_vec(), alloc::vec::Vec::new())).unwrap();
}

fn open_reply(catalogue: &Channel, corr: u32) -> Arc<Channel> {
	let (driver, client) = Channel::create();
	let mut bytes = corr.to_le_bytes().to_vec();
	bytes.push(1);
	bytes.extend_from_slice(&0u32.to_le_bytes());
	send_cap(catalogue, &bytes, client, Rights::ALL).unwrap();
	driver
}

fn describe(driver: &Channel, key: &str, source: backlight_wire::BacklightSource) {
	let describe = receive(driver, "backlight describe");
	assert_eq!(le_u16(&describe.bytes, 0), backlight_wire::backlight::OP_DESCRIBE);
	// This valid description is well over the old shared 128-byte request buffer.
	let description = backlight_wire::BacklightDescription { source, key: key.into(), scale: backlight_wire::BacklightScale::Levels((0..=100).collect()), ac_default: None, battery_default: None, target: backlight_wire::BacklightTarget::Function(backlight_wire::BacklightFunction { bus: 0, dev: 4, func: 0 }) };
	reply(driver, le_u32(&describe.bytes, 2), |writer| description.write(writer).unwrap());
}

fn finish_admission(driver: &Channel) -> Arc<Channel> {
	let get = receive(driver, "backlight get");
	complete_admission(driver, get)
}

fn complete_admission(driver: &Channel, get: Message) -> Arc<Channel> {
	assert_eq!(le_u16(&get.bytes, 0), backlight_wire::backlight::OP_GET);
	reply(driver, le_u32(&get.bytes, 2), |writer| writer.u32(50).unwrap());
	let events = receive(driver, "backlight events");
	assert_eq!(le_u16(&events.bytes, 0), backlight_wire::backlight::OP_EVENTS);
	let (producer, consumer) = Channel::create();
	send_cap(driver, &le_u32(&events.bytes, 2).to_le_bytes(), consumer, Rights::ALL).unwrap();
	pump();
	producer
}

fn admit(driver: &Channel, key: &str) -> Arc<Channel> {
	describe(driver, key, backlight_wire::BacklightSource::Native);
	finish_admission(driver)
}

fn set(control: &Channel, key: &str, corr: u32, level: u32) {
	let mut writer = wire::VecWriter::new();
	writer.bytes_lp(key.as_bytes()).unwrap();
	BrightnessTarget::Level(level).write(&mut writer).unwrap();
	writer.u8(0).unwrap();
	control.send(request(display_brightness_control::OP_SET, corr, &writer.into_inner().unwrap())).unwrap();
}

fn device_level(events: &Channel, level: u32) {
	let mut frame = [0u8; 32];
	let mut handles = wire::Handles::new();
	let len = backlight_wire::backlight::events_frame(0, &backlight_wire::BacklightEvent::Level(level), &mut frame, &mut handles).unwrap();
	events.send(Message::new(frame[..len].to_vec(), alloc::vec::Vec::new())).unwrap();
	pump();
}

tagged_test!(display_service_keeps_serving_while_backlights_stall, [Service, Display], id = "kernel.services.display_service_keeps_serving_while_backlights_stall", covers = ["bin.display_service", "service-logic", "display-device-proto", "display-proto"]);
fn display_service_keeps_serving_while_backlights_stall() {
	let (read, read_service) = Channel::create();
	let (control, control_service) = Channel::create();
	let (catalogue, catalogue_service) = Channel::create();
	let harness = start_with_brightness(4, 4, Some([read_service, control_service, catalogue_service]));
	let subscription = receive(&catalogue, "backlight catalogue subscription");
	assert_eq!(le_u16(&subscription.bytes, 0), device::provider_catalogue::OP_SUBSCRIBE);
	let (stream, consumer) = Channel::create();
	send_cap(&catalogue, &le_u32(&subscription.bytes, 2).to_le_bytes(), consumer, Rights::ALL).unwrap();
	pump();

	// A describe that never answers must not stop the display root or the read-only brightness root.
	announce(&stream, 1, true);
	let open = receive(&catalogue, "first provider open");
	let silent = open_reply(&catalogue, le_u32(&open.bytes, 2));
	let describe = receive(&silent, "held describe");
	assert_eq!(le_u16(&describe.bytes, 0), backlight_wire::backlight::OP_DESCRIBE);
	harness.console.send(Message::new(abi::HEARTBEAT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new())).unwrap();
	assert_eq!(receive(&harness.console, "display heartbeat during describe").bytes, b"PONG");
	assert!(snapshot(&read, 10).is_empty());
	assert!(!silent.is_peer_closed(), "both independent requests completed before describe's deadline");
	sched::run_until_idle_until(arch::apic::ticks().saturating_add(15));
	assert!(silent.is_peer_closed(), "a silent description lost its connection at the deadline");

	// A late catalogue open cannot be mistaken for a later publication's open, and its transferred
	// endpoint must be closed rather than leaked in the service.
	announce(&stream, 2, true);
	let old_open = receive(&catalogue, "held catalogue open");
	sched::run_until_idle_until(arch::apic::ticks().saturating_add(15));
	announce(&stream, 3, true);
	let new_open = receive(&catalogue, "new catalogue open");
	assert_ne!(le_u32(&old_open.bytes, 2), le_u32(&new_open.bytes, 2));
	let stale = open_reply(&catalogue, le_u32(&old_open.bytes, 2));
	pump();
	assert!(stale.is_peer_closed(), "an unmatched late open releases its transferred channel");
	let driver = open_reply(&catalogue, le_u32(&new_open.bytes, 2));
	let events = admit(&driver, "native:async-primary");
	let states = snapshot(&read, 11);
	assert_eq!(states.len(), 1);
	assert_eq!(states[0].level, Some(50));
	assert!(!states[0].failed);

	// A held write leaves the whole display and read path runnable; after its timeout the published
	// state says failed, and the waiting writer receives an explicit deadline error.
	set(&control, "native:async-primary", 20, 70);
	let old_set = receive(&driver, "held level write");
	assert_eq!(le_u16(&old_set.bytes, 0), backlight_wire::backlight::OP_SET);
	harness.console.send(Message::new(abi::HEARTBEAT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new())).unwrap();
	assert_eq!(receive(&harness.console, "display heartbeat during set").bytes, b"PONG");
	assert_eq!(snapshot(&read, 12)[0].level, Some(50));
	let timeout = receive(&control, "timed-out set reply");
	refused(&timeout, 20);
	assert_eq!(display_v1::Error::read(&mut wire::Reader::new(&timeout.bytes[5..])), Some(display_v1::Error::TimedOut));
	assert!(snapshot(&read, 13)[0].failed);

	// A device event recovers the provider. Its old write response cannot finish or modify a new
	// write on that same channel, even when the response also smuggles a stream handle.
	device_level(&events, 33);
	assert!(!snapshot(&read, 14)[0].failed);
	set(&control, "native:async-primary", 21, 60);
	let new_set = receive(&driver, "new level write");
	assert_ne!(le_u32(&old_set.bytes, 2), le_u32(&new_set.bytes, 2));
	let (leaked, late_handle) = Channel::create();
	let mut late_bytes = le_u32(&old_set.bytes, 2).to_le_bytes().to_vec();
	late_bytes.push(1);
	late_bytes.extend_from_slice(&77u32.to_le_bytes());
	send_cap(&driver, &late_bytes, late_handle, Rights::ALL).unwrap();
	pump();
	assert!(leaked.is_peer_closed(), "a late completion closes every transferred handle");
	assert!(control.recv().is_err(), "the new writer is still waiting for its own correlation");
	assert_eq!(snapshot(&read, 15)[0].level, Some(33));
	reply(&driver, le_u32(&new_set.bytes, 2), |writer| writer.u32(60).unwrap());
	let completed = receive(&control, "current set reply");
	let result = display_v1::BrightnessSet::read(&mut wire::Reader::new(succeeded(&completed, 21))).unwrap();
	assert_eq!(result.level, 60);

	// Multiple long descriptions are still a whole read snapshot; withdrawing the provider closes
	// both the call endpoint and the event consumer and cancels its outstanding write.
	announce(&stream, 4, true);
	let open = receive(&catalogue, "second live provider open");
	let second = open_reply(&catalogue, le_u32(&open.bytes, 2));
	let second_events = admit(&second, "native:async-shadow");
	assert_eq!(snapshot(&read, 16).len(), 2, "both complete backlight rows fit the reply");
	set(&control, "native:async-primary", 22, 80);
	let _held = receive(&driver, "write interrupted by withdrawal");
	announce(&stream, 3, false);
	let closed = receive(&control, "withdrawn writer refusal");
	refused(&closed, 22);
	assert_eq!(display_v1::Error::read(&mut wire::Reader::new(&closed.bytes[5..])), Some(display_v1::Error::Closed));
	assert!(driver.is_peer_closed());
	assert!(events.is_peer_closed());
	assert_eq!(snapshot(&read, 17).len(), 1);
	announce(&stream, 4, false);
	pump();
	assert!(second.is_peer_closed());
	assert!(second_events.is_peer_closed());
	assert!(snapshot(&read, 18).is_empty());
	harness.service.terminate();
}

tagged_test!(display_service_admits_optional_firmware_metadata_without_stalling, [Service, Display], id = "kernel.services.display_service_admits_optional_firmware_metadata_without_stalling", covers = ["bin.display_service", "service-logic", "display-device-proto", "display-proto"]);
fn display_service_admits_optional_firmware_metadata_without_stalling() {
	let (read, read_service) = Channel::create();
	let (control, control_service) = Channel::create();
	let (catalogue, catalogue_service) = Channel::create();
	let harness = start_with_brightness(4, 4, Some([read_service, control_service, catalogue_service]));
	let subscription = receive(&catalogue, "backlight catalogue subscription");
	let (stream, consumer) = Channel::create();
	send_cap(&catalogue, &le_u32(&subscription.bytes, 2).to_le_bytes(), consumer, Rights::ALL).unwrap();
	pump();
	let mut providers = alloc::vec::Vec::new();
	for (slot, key, id) in [(1, "acpi:ZZZ0", None), (2, "acpi:AAA0", None), (3, "acpi:LCD1", Some(0x400)), (4, "acpi:LCD0", Some(0x80010400))] {
		announce(&stream, slot, true);
		let open = receive(&catalogue, "firmware provider open");
		let driver = open_reply(&catalogue, le_u32(&open.bytes, 2));
		describe(&driver, key, backlight_wire::BacklightSource::Firmware);
		let metadata = receive(&driver, "optional firmware display id");
		assert_eq!(le_u16(&metadata.bytes, 0), backlight_wire::backlight::OP_FIRMWARE_DISPLAY_ID);
		let metadata_corr = le_u32(&metadata.bytes, 2);
		let events = if slot == 1 {
			// An older provider silently ignores the additive opcode. Normal display/read work
			// completes before the metadata deadline, and GET follows with its own correlation.
			harness.console.send(Message::new(abi::HEARTBEAT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new())).unwrap();
			assert_eq!(receive(&harness.console, "display heartbeat during metadata").bytes, b"PONG");
			assert!(snapshot(&read, 1).is_empty());
			assert!(driver.recv().is_err(), "GET does not race the pending metadata call");
			let get = receive(&driver, "GET after optional metadata deadline");
			assert_ne!(metadata_corr, le_u32(&get.bytes, 2));
			let (leaked, consumer) = Channel::create();
			let mut late = metadata_corr.to_le_bytes().to_vec();
			late.extend_from_slice(&[1, 1]);
			late.extend_from_slice(&0x400u32.to_le_bytes());
			send_cap(&driver, &late, consumer, Rights::ALL).unwrap();
			pump();
			assert!(leaked.is_peer_closed(), "late metadata releases an unclaimed transferred handle");
			assert!(driver.recv().is_err(), "late metadata cannot complete the pending GET");
			complete_admission(&driver, get)
		} else {
			if let Some(id) = id {
				reply(&driver, metadata_corr, |writer| {
					writer.u8(1).unwrap();
					writer.u32(id).unwrap();
				});
			} else {
				let mut writer = wire::VecWriter::new();
				writer.u32(metadata_corr).unwrap();
				writer.u8(0).unwrap();
				backlight_wire::Error::Unsupported.write(&mut writer).unwrap();
				driver.send(Message::new(writer.into_inner().unwrap(), alloc::vec::Vec::new())).unwrap();
			}
			finish_admission(&driver)
		};
		providers.push((driver, events));
		let states = snapshot(&read, 10 + slot);
		assert_eq!(states.len(), slot as usize);
		for state in states {
			assert_eq!(state.output, Some(0));
			assert_eq!(state.reason, Some(display_v1::JoinReason::FirmwareAdapter));
			assert!(!state.failed);
			if state.key == key {
				assert_eq!(state.standing, display_v1::BacklightStanding::Active);
			} else {
				assert_eq!(state.standing, display_v1::BacklightStanding::Shadowed);
				assert_eq!(state.shadowed_by.as_deref(), Some(key));
			}
		}
	}
	// Withdrawal during optional admission closes the original connection and does not start GET.
	announce(&stream, 5, true);
	let open = receive(&catalogue, "withdrawn metadata provider open");
	let withdrawn = open_reply(&catalogue, le_u32(&open.bytes, 2));
	describe(&withdrawn, "acpi:LCD2", backlight_wire::BacklightSource::Firmware);
	let _metadata = receive(&withdrawn, "metadata interrupted by withdrawal");
	announce(&stream, 5, false);
	pump();
	assert!(withdrawn.is_peer_closed());
	assert_eq!(snapshot(&read, 20).len(), 4);
	for (index, (driver, events)) in providers.iter().enumerate() {
		announce(&stream, index as u32 + 1, false);
		pump();
		assert!(driver.is_peer_closed());
		assert!(events.is_peer_closed());
	}
	assert!(snapshot(&read, 21).is_empty());
	drop(control);
	harness.service.terminate();
}
