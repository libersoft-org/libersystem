// The dedicated device-tree fixture: a method-only platform row whose interrupt specifier names a GPIO
// controller's child. DeviceManager must hand over a scoped connection, never a wired interrupt.

use alloc::format;
use drivers::common;
use ipc_client::ChannelTransport;
use proto::system::{GpioTrigger, gpio_device};
use rt::*;
use wire::Handles;

fn say(text: &str) {
	print(format!("tree-fixture: {text}\n").as_bytes());
}

pub fn serve(bootstrap: u64, bind: &common::Bind, resources: &common::Resources) -> ! {
	let connections = bind.info.platform.connections();
	if bind.info.platform.identity() != b"dt:/liber-fixture" || connections.len() != 1 || connections[0].kind != CONNECTION_GPIO_LINE || resources.connection_count != 1 || resources.connections[0] == 0 || resources.line_count != 0 || resources.irq != 0 {
		say("refused: expected one GPIO connection and no wired interrupt");
		common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let connection = resources.connections[0];
	let client = || gpio_device::Client::with_deadline(ChannelTransport { chan: connection }, clock() + super::TICKS);
	let Some(Ok(line)) = client().line() else {
		say("refused: the connection did not describe its line");
		common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	};
	if line.line != 2 || line.trigger != GpioTrigger::Rising {
		say("refused: the connection was not scoped to rising edges on line 2");
		common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let events = client().events().unwrap_or(0);
	if events == 0 {
		say("refused: the line has no event stream");
		common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	if !common::online(bootstrap, bind, b"driver.acpi-fixture: online (device tree)", &[]) {
		exit();
	}
	say("dt:/liber-fixture bound through GPIO line 2 (rising), no wired interrupt; ready");
	let mut buf = [0u8; 256];
	let mut received = 0u32;
	loop {
		if common::wait_or_answer(bootstrap, bind, &[events]).is_none() {
			if common::stop_requested() {
				close(events);
				close(connection);
				common::finish_stop(bootstrap, bind, resources.device, true);
			}
			exit();
		}
		loop {
			match try_recv_caps(events, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let mut frame = Handles::new();
					let Some(event) = gpio_device::events_read(&buf[..len], &mut frame) else {
						say("refused: malformed GPIO event");
						common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable);
					};
					if !event.level || !matches!(client().acknowledge(), Some(Ok(()))) {
						say("refused: rising event or acknowledgement failed");
						common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable);
					}
					received += 1;
					say(&format!("event {received}: GPIO line 2 high, acknowledged"));
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					say("the GPIO controller closed its event stream");
					exit();
				}
			}
		}
	}
}
