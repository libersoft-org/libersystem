// modemdata - the modem gate's data-only client. DEVELOPMENT-ONLY.
//
// Its manifest REQUESTS data, subscriber identity and management, and its policy grants data and the
// network alone. It proves what that leaves it: no identity connection and no management connection
// were delivered at all, and its ordinary network connection cannot install a link - the request an
// installation is made of, sent where an ordinary client can send it, installs nothing.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{InterfaceId, Ipv4Addr as WireIp, LaunchContext, LinkAttachment, LinkFamily, LinkProvider, network, network_link_admin};
use rt::*;

const TICKS: u64 = 100;

fn fail(line: &[u8]) -> ! {
	print(b"modemdata: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

// What the network is running on, as a client sees it: the interface, or none.
fn link(net: u64) -> Option<InterfaceId> {
	match network::Client::with_deadline(ChannelTransport { chan: net }, clock() + 20 * TICKS).info() {
		Some(Ok(info)) => Some(info.scope),
		_ => None,
	}
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
		fail(b"the network and data grants were not both delivered");
	}
	// NOTHING MORE ARRIVES: identity and management were withheld, not delivered late.
	sleep_until(clock() + TICKS / 2);
	if let PolledCaps::Message { handles, .. } = try_recv_caps(bootstrap, &mut buf) {
		for &handle in handles.as_slice() {
			close(handle);
		}
		fail(b"a grant past data was delivered to a data-only client");
	}
	// AN INSTALLATION SENT ON AN ORDINARY NETWORK CONNECTION installs nothing.
	let before = link(net);
	let Some((ours, theirs)) = channel() else { fail(b"a channel could not be made") };
	let attachment = LinkAttachment { provider: LinkProvider { slot: 0, generation: 1, binding_generation: 1 }, sim_generation: 1, context_generation: 1, packets: theirs, family: LinkFamily::Ipv4, address: WireIp { a: 10, b: 64, c: 0, d: 2 }, prefix: 30, gateway: None, dns: alloc::vec::Vec::new(), mtu: 1400 };
	let answer = network_link_admin::Client::with_deadline(ChannelTransport { chan: net }, clock() + 3 * TICKS).install(&1, &attachment);
	if matches!(answer, Some(Ok(_))) {
		fail(b"an ordinary network connection installed a link");
	}
	close(ours);
	if link(net) != before {
		fail(b"the attempt changed the link");
	}
	print(b"modemdata: PASS identity and management were withheld from a data-only client, and an ordinary network connection installed nothing\n");
	exit();
}
