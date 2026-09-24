// modemswap - the modem gate's data client that MAY replace an uplink. DEVELOPMENT-ONLY.
//
// On a machine with a NIC it proves the other half of admission: a data grant whose policy allows
// replacing the selected uplink activates over it, carries traffic through ordinary network calls, and on
// deactivation the NIC it replaced comes back - as a new interface generation, with nothing carried over.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{ContextState, IpAddress, Ipv4Addr as WireIp, LaunchContext, PingStatus, ScopedAddress, modem_data, network};
use rt::*;

const TICKS: u64 = 100;
const GATEWAY: [u8; 4] = [10, 64, 0, 1];

fn fail(line: &[u8]) -> ! {
	print(b"modemswap: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	if recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode).is_none() {
		exit();
	}
	let net = recv_tagged(bootstrap, &mut buf, b"NETWORK").unwrap_or(0);
	let data = recv_tagged(bootstrap, &mut buf, b"MODEMDATA").unwrap_or(0);
	if net == 0 || data == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let network = || network::Client::with_deadline(ChannelTransport { chan: net }, clock() + 20 * TICKS);
	let modem = || modem_data::Client::with_deadline(ChannelTransport { chan: data }, clock() + 90 * TICKS);
	let before = match network().info() {
		Some(Ok(info)) if info.name == "net0" => info,
		_ => fail(b"there is no NIC selected - this run needs a machine with one"),
	};
	let id = match modem().status() {
		Some(Ok(status)) => status.id,
		_ => fail(b"the modem's status could not be read"),
	};
	let context = match modem().activate(&id) {
		Some(Ok(status)) if status.state == ContextState::Active => status.id,
		_ => fail(b"a grant allowed to replace the NIC could not activate"),
	};
	match network().info() {
		Some(Ok(info)) if info.name == "wwan0" && info.scope.generation > before.scope.generation => {}
		_ => fail(b"the modem link did not replace the NIC as a new interface generation"),
	}
	let gateway = ScopedAddress { addr: IpAddress::V4(WireIp { a: GATEWAY[0], b: GATEWAY[1], c: GATEWAY[2], d: GATEWAY[3] }), scope: None };
	match network().ping(&gateway) {
		Some(Ok(reply)) if reply.status == PingStatus::Reply => {}
		_ => fail(b"the gateway did not answer over the replacing link"),
	}
	if !matches!(modem().deactivate(&context), Some(Ok(()))) {
		fail(b"the context could not be deactivated");
	}
	// THE NIC IT REPLACED COMES BACK, as another generation: nothing of the modem link carries over.
	let deadline = clock() + 10 * TICKS;
	loop {
		if let Some(Ok(info)) = network().info()
			&& info.name == "net0"
			&& info.scope.generation > before.scope.generation
		{
			break;
		}
		if clock() >= deadline {
			fail(b"the replaced NIC did not come back after the deactivation");
		}
		sleep_until(clock() + TICKS / 5);
	}
	print(b"modemswap: PASS a grant allowed to replace the NIC did, carried traffic, and on deactivation the NIC came back as a new generation\n");
	exit();
}
