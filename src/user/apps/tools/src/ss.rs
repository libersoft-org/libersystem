// ss - a standalone foreground net tool the shell spawns: list the live sockets.
//
// The netstat/ss equivalent: the shell mints a fresh NetworkService client channel
// (network.open), spawns this program, and transfers that channel. ss asks
// NetworkService for its socket table over its OWN channel and renders one row per
// socket - the TCP state, the local port, and the remote endpoint (a listening socket
// shows no peer) - then signals completion and exits. A standalone program, not a
// shell built-in.

#![no_std]
#![no_main]

extern crate alloc;

use network_client::NetworkClient;
use proto::system::{FamilyReadiness, SockState};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	// Governed launch sends arguments first, then the tagged NetworkService grant.
	inherit_stdout(bootstrap);
	let Some((_, attached)) = recv_launch_with(bootstrap) else { exit() };
	let netsvc: u64 = granted_capability(bootstrap, attached, CAP_NETWORK, &mut buf).unwrap_or_else(|| exit());
	show(netsvc);
	close(netsvc);
	exit();
}

// Query the socket table and render it: a header, then one `<state> <local> <peer>`
// row per socket - a listening socket shows `*` for its peer.
fn show(netsvc: u64) {
	let mut client = NetworkClient::new(netsvc);
	match client.sockets() {
		Some(Ok(socks)) => {
			if socks.is_empty() {
				eprint(b"ss: no sockets\n");
			} else {
				print(b"State     Local                                Peer\n");
				for s in &socks {
					// A ROW HOLDS TWO FULL ADDRESSES NOW. Two scoped IPv6 endpoints with ports run to
					// over a hundred characters, so a line cut for two dotted quads would truncate
					// exactly the rows this milestone exists to show.
					let mut line: [u8; 224] = [0u8; 224];
					let mut pos: usize = 0;
					pos = put(&mut line, pos, state_label(s.state));
					pos = pad_col(&mut line, pos, 10);
					pos += endpoint(&s.local, &mut line[pos..]);
					pos = pad_col(&mut line, pos, 47);
					// A LISTENER HAS NO PEER, and says so rather than printing the unspecified address it
					// carries - `::` is a real address, and this is the absence of one.
					match s.state {
						SockState::Listen => pos = put(&mut line, pos, b"*"),
						_ => pos += endpoint(&s.remote, &mut line[pos..]),
					}
					line[pos] = b'\n';
					pos += 1;
					print(&line[..pos]);
				}
			}
		}
		Some(Err(_)) => eprint(b"ss: network error\n"),
		None => eprint(b"ss: service unavailable\n"),
	}
	// The pool utilization footer: the client, socket and listener channels the
	// service currently stands on, and its live TCP connections. Every set grows on
	// demand, so these are live counts against the domain's handle budget, not a cap.
	if let Some(Ok(cap)) = client.capacity() {
		let mut line: [u8; 96] = [0u8; 96];
		let mut pos: usize = 0;
		pos = put(&mut line, pos, b"channels: clients ");
		pos += write_u16(cap.clients as u16, &mut line[pos..]);
		pos = put(&mut line, pos, b", sockets ");
		pos += write_u16(cap.sockets as u16, &mut line[pos..]);
		pos = put(&mut line, pos, b", listeners ");
		pos += write_u16(cap.listeners as u16, &mut line[pos..]);
		pos = put(&mut line, pos, b"; connections ");
		pos += write_u16(cap.connections as u16, &mut line[pos..]);
		line[pos] = b'\n';
		pos += 1;
		print(&line[..pos]);
		// WHERE EACH FAMILY HAS GOT TO, which is the fact a caller acts on: the service answers as
		// soon as it is listening, so "online" says nothing about whether either family can carry a
		// packet yet.
		let mut second: [u8; 96] = [0u8; 96];
		let mut at: usize = 0;
		at = put(&mut second, at, b"families: ipv4 ");
		at = put(&mut second, at, readiness(&cap.ipv4));
		at = put(&mut second, at, b", ipv6 ");
		at = put(&mut second, at, readiness(&cap.ipv6));
		at = put(&mut second, at, b"; diagnostic ");
		at += write_u16(cap.diagnostic_used as u16, &mut second[at..]);
		second[at] = b'/';
		at += 1;
		at += write_u16(cap.diagnostic_limit as u16, &mut second[at..]);
		at = put(&mut second, at, b", refused ");
		at += write_u16(cap.diagnostic_refusals as u16, &mut second[at..]);
		second[at] = b'\n';
		at += 1;
		print(&second[..at]);
	}
}

// The word for one family's readiness.
fn readiness(state: &FamilyReadiness) -> &'static [u8] {
	match state {
		FamilyReadiness::Disabled => b"disabled",
		FamilyReadiness::Configuring => b"configuring",
		FamilyReadiness::Ready => b"ready",
		FamilyReadiness::Failed => b"failed",
	}
}

// One endpoint: the address, and the port after a colon.
//
// AN IPv6 ADDRESS GOES IN BRACKETS. `fe80::1:80` cannot be read - the colon before the port is
// indistinguishable from the colons inside the address, and every reader of this output would have
// to guess where one ends.
fn endpoint(value: &proto::generated::liber::network::v1::ScopedEndpoint, out: &mut [u8]) -> usize {
	let mut pos: usize = 0;
	let bracketed: bool = value.addr.addr.is_v6();
	if bracketed {
		out[pos] = b'[';
		pos += 1;
	}
	pos += value.addr.render(&mut out[pos..]);
	if bracketed {
		out[pos] = b']';
		pos += 1;
	}
	out[pos] = b':';
	pos += 1;
	pos + write_u16(value.port, &mut out[pos..])
}

// The human-readable label for a socket's TCP state.
fn state_label(s: SockState) -> &'static [u8] {
	match s {
		SockState::Listen => b"LISTEN",
		SockState::SynSent => b"SYN-SENT",
		SockState::SynRcvd => b"SYN-RCVD",
		SockState::Established => b"ESTAB",
		SockState::FinWait => b"FIN-WAIT",
		SockState::Closed => b"CLOSED",
	}
}

// Copy `bytes` into `line` at `pos`, returning the new write position.
fn put(line: &mut [u8], pos: usize, bytes: &[u8]) -> usize {
	line[pos..pos + bytes.len()].copy_from_slice(bytes);
	pos + bytes.len()
}

// Pad `line` with spaces from `pos` up to column `col` (at least one space if already
// at or past it), returning the new write position - the simple column aligner.
fn pad_col(line: &mut [u8], pos: usize, col: usize) -> usize {
	let mut p: usize = pos;
	if p >= col {
		line[p] = b' ';
		p += 1;
	} else {
		while p < col {
			line[p] = b' ';
			p += 1;
		}
	}
	p
}

// Write `v` as decimal digits into `out`, returning the count (1-5).
fn write_u16(v: u16, out: &mut [u8]) -> usize {
	if v == 0 {
		out[0] = b'0';
		return 1;
	}
	let mut digits: [u8; 5] = [0u8; 5];
	let mut n: usize = 0;
	let mut x: u16 = v;
	while x > 0 {
		digits[n] = b'0' + (x % 10) as u8;
		x /= 10;
		n += 1;
	}
	let mut i: usize = 0;
	while i < n {
		out[i] = digits[n - 1 - i];
		i += 1;
	}
	n
}
