// btgatt - the LE gate's application: a GATT client on one aliased peer, through the grant PermissionManager mints for
// it and nothing else. DEVELOPMENT-ONLY.
//
// Its policy row names the peer aliased `tag-1` and two services, the battery's and a custom one. It proves what a
// grant reaches and what it does not: the granted services and no other, their characteristics, a read, a write, a
// notification - and a handle outside the granted services refused before anything goes on the radio.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use ipc_client::ChannelTransport;
use proto::system::{Error, LaunchContext, bluetooth_gatt};
use rt::*;
use wire::Sink;

const TICKS: u64 = 100;

fn fail(line: &str) -> ! {
	print(format!("btgatt: FAIL {line}\n").as_bytes());
	exit_with(1);
}

fn say(line: &str) {
	print(format!("btgatt: {line}\n").as_bytes());
}

// Queue through the real grant without waiting, so the owner can end while the
// independent peer deliberately withholds the subscription discovery response.
fn queued(grant: u64, op: u16, corr: u32, handle: u16, value: Option<&[u8]>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u16(op)?;
		writer.u32(corr)?;
		writer.u16(handle)?;
		if let Some(value) = value {
			writer.u16(value.len() as u16)?;
			writer.raw(value)?;
		}
		Some(())
	})();
	if !encoded.and_then(|()| writer.into_inner()).is_some_and(|bytes| send_blocking(grant, &bytes, 0)) {
		fail("the queued grant operation could not be sent");
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 128];
	inherit_stdout(bootstrap);
	let launch = recv_launch_bytes(bootstrap);
	let context = launch.as_deref().and_then(LaunchContext::decode);
	let mode = context.as_ref().map_or("", |context| context.arguments.trim());
	if !matches!(mode, "" | "indicate" | "timeout" | "revoke") {
		fail("expected indicate, timeout or revoke");
	}
	let indicate = mode == "indicate";
	let grant = recv_tagged(bootstrap, &mut buf, b"BTGATT").unwrap_or(0);
	if grant == 0 {
		fail("no GATT grant was delivered");
	}
	let client = || bluetooth_gatt::Client::new(ChannelTransport { chan: grant });
	// THE PEER MAY STILL BE COMING BACK through the accept list: closed is retried for a few seconds.
	let deadline = clock() + 10 * TICKS;
	let services = loop {
		match client().services() {
			Some(Ok(services)) => break services,
			Some(Err(Error::Closed)) if clock() < deadline => sleep_until(clock() + TICKS / 4),
			other => fail(&format!("the granted services could not be listed: {other:?}")),
		}
	};
	let battery = services.iter().find(|service| service.uuid == 0x180f).cloned().unwrap_or_else(|| fail("the battery service was not listed"));
	let custom = services.iter().find(|service| service.uuid == 0xfff0).cloned().unwrap_or_else(|| fail("the custom service was not listed"));
	if services.iter().any(|service| matches!(service.uuid, 0x1800 | 0x1801)) || services.len() != 2 {
		fail("the grant listed a service it does not cover");
	}
	say("the two granted services are listed, and neither GAP nor GATT");
	let level = match client().characteristics(&battery.start) {
		Some(Ok(found)) => found.into_iter().find(|characteristic| characteristic.uuid == 0x2a19).unwrap_or_else(|| fail("the battery level was not found")),
		other => fail(&format!("the battery's characteristics could not be listed: {other:?}")),
	};
	match client().read(&level.handle) {
		Some(Ok(value)) if value == [87] => say("the battery level reads 87"),
		other => fail(&format!("the battery level read {other:?}")),
	}
	let value = match client().characteristics(&custom.start) {
		Some(Ok(found)) => found.into_iter().find(|characteristic| characteristic.uuid == if indicate { 0xfff2 } else { 0xfff1 }).unwrap_or_else(|| fail("the custom characteristic was not found")),
		other => fail(&format!("the custom service's characteristics could not be listed: {other:?}")),
	};
	match client().read(&value.handle) {
		Some(Ok(read)) if read == b"fixture" => {}
		other => fail(&format!("the custom value read {other:?}")),
	}
	// OUTSIDE THE GRANT: the peer's name, in GAP, is refused here.
	if !matches!(client().read(&0x0003), Some(Err(Error::Denied))) {
		fail("a handle outside the granted services was not refused");
	}
	say("a handle outside the granted services is refused");
	if mode == "revoke" {
		queued(grant, bluetooth_gatt::OP_SUBSCRIBE, 0x10001, value.handle, None);
		queued(grant, bluetooth_gatt::OP_WRITE, 0x10002, value.handle, Some(b"revoked-write"));
		// Cached services replies immediately; FIFO request delivery proves both
		// preceding operations reached BluetoothService before this owner exits.
		if !matches!(client().services(), Some(Ok(_))) {
			fail("the queue barrier was not answered");
		}
		say("the subscription and write are queued; the owner will end");
		sleep_until(clock() + 10 * TICKS);
		close(grant);
		print(b"btgatt: PASS revoke owner ended with queued operations\n");
		exit();
	}
	let started = clock();
	let subscribed = client().subscribe(&value.handle);
	if mode == "timeout" {
		let elapsed = clock().saturating_sub(started);
		if !matches!(subscribed, Some(Err(Error::TimedOut))) || !(30 * TICKS..=45 * TICKS).contains(&elapsed) {
			fail(&format!("silent subscription result {subscribed:?}, elapsed {elapsed} ticks"));
		}
		print(format!("btgatt: PASS silent subscription timed out after {elapsed} ticks\n").as_bytes());
		exit();
	}
	let stream = match subscribed {
		Some(Ok(stream)) => stream,
		other => fail(&format!("the subscription was refused: {other:?}")),
	};
	if !matches!(client().write(&value.handle, &b"hello".to_vec()), Some(Ok(()))) {
		fail("the write was refused");
	}
	let mut frame = [0u8; 300];
	match recv_caps_deadline(stream, &mut frame, clock() + 5 * TICKS) {
		DeadlineCaps::Message { len, mut handles } => match bluetooth_gatt::subscribe_read(&frame[..len], &mut handles) {
			Some(notified) if notified.handle == value.handle && notified.value == b"hello" => {}
			other => fail(&format!("the notification was {other:?}")),
		},
		_ => fail("no notification arrived"),
	}
	say(if indicate { "the write went, and the peer's indication came back on the subscription" } else { "the write went, and the peer's notification came back on the subscription" });
	print(b"btgatt: PASS\n");
	exit();
}
