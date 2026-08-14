// traceroute - show the path a packet takes, one hop at a time.
//
// IT NEVER TOUCHES THE NIC. A traceroute is conventionally a program holding a raw socket, which is
// ambient authority over every packet the machine sends and receives - handed out to discover a
// route. This one holds a NetworkService client and asks a typed question: `probe(addr, ttl)`, "who
// discards a datagram that lives this long". The service owns the interface; the tool owns one
// question and cannot ask a different one.
//
// HOW IT WORKS is the same trick every traceroute uses: a datagram sent with a TTL of `n` is
// discarded by the `n`-th router on the path, which reports itself with an ICMP Time Exceeded. The
// route falls out of a field that exists for a completely different reason - stopping packets from
// circulating forever - and the whole tool is a loop over that field.
//
// FOUR OUTCOMES, KEPT APART, because they mean different things about the path and a tool that
// showed them the same way would hide where it breaks:
//   - a hop answered, which is the ordinary row;
//   - the DESTINATION answered, which ends the trace;
//   - somebody refused the datagram (`!`), which is an answer saying the path is blocked;
//   - nothing came back (`*`), which is silence - a hop that does not report itself, which is a
//     configuration choice and not a fault.
//
// THE PROBES ARE PACED. Several probes per hop over a bounded number of hops is a burst aimed at a
// path somebody else operates, and a tool that sent them as fast as it could would be
// indistinguishable from something malicious. The service's own per-probe timeout paces the
// unanswered case; this adds a gap between probes so an answered one does not turn into a flood.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_u64};
use network_client::NetworkClient;
use proto::codec::{JsonMode, json_escape};
use proto::system::{HopStatus, Ipv4Addr, LaunchContext};
use rt::*;
use tools::{push_decimal, split_args};

// The furthest this will look. Thirty is the conventional ceiling and is past any real path; a
// bound is what keeps a trace of an unreachable address from running until somebody stops it.
const DEFAULT_MAX_HOPS: u64 = 30;
const HOP_CEILING: u64 = 64;
// Probes per hop. Three is the convention, and the reason for more than one is that a single
// timeout says nothing: routers rate-limit their own error messages, so one silent probe and three
// silent probes are different amounts of evidence.
const DEFAULT_PROBES: u64 = 3;
const PROBE_CEILING: u64 = 10;
// The gap between probes, in scheduler ticks - the pacing this tool adds on top of the service's
// own timeout.
const PROBE_GAP_TICKS: u64 = 5;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let Some((context_bytes, attached)) = recv_launch_with(bootstrap) else { exit() };
		let context: LaunchContext = match LaunchContext::decode(&context_bytes) {
			Some(context) => context,
			None => exit(),
		};
		let arguments: Vec<u8> = context.arguments.clone().into_bytes();
		let netsvc: u64 = granted_capability(bootstrap, attached, CAP_NETWORK, &mut buf).unwrap_or_else(|| exit());

		let mut max_hops = DEFAULT_MAX_HOPS;
		let mut probes = DEFAULT_PROBES;
		let mut json: Option<JsonMode> = None;
		let mut target: Option<&[u8]> = None;
		let mut expect: Option<u8> = None;
		for word in split_args(&arguments) {
			if let Some(which) = expect.take() {
				let Some(value) = parse_u64(word).filter(|value| *value > 0) else {
					eprint(b"traceroute: the count is a whole number, at least one\n");
					exit();
				};
				match which {
					b'm' => max_hops = value.min(HOP_CEILING),
					_ => probes = value.min(PROBE_CEILING),
				}
				continue;
			}
			match classify(word) {
				Arg::Short(b'm') => expect = Some(b'm'),
				Arg::Short(b'q') => expect = Some(b'q'),
				Arg::Long(b"max-hops", None) => expect = Some(b'm'),
				Arg::Long(b"probes", None) => expect = Some(b'q'),
				Arg::Value(b"json") | Arg::Value(b"json-min") => json = JsonMode::parse(word),
				Arg::Value(value) if target.is_none() => target = Some(value),
				_ => {
					eprint(b"traceroute: usage: traceroute [-m HOPS][-q PROBES] <host> [json]\n");
					exit();
				}
			}
		}
		let (Some(target), None) = (target, expect) else {
			eprint(b"traceroute: usage: traceroute [-m HOPS][-q PROBES] <host> [json]\n");
			exit();
		};

		// A NAME OR A NUMBER, resolved the same way `ping` resolves one - through the service's own
		// DNS, so the tool needs no resolver and no configuration of its own.
		let mut client = NetworkClient::new(netsvc);
		let destination: Ipv4Addr = match Ipv4Addr::parse(target) {
			Some(addr) => addr,
			None => {
				let Ok(name) = core::str::from_utf8(target) else {
					eprint(b"traceroute: the destination is neither an address nor a name\n");
					exit();
				};
				match client.resolve(name) {
					Some(Ok(addr)) => addr,
					_ => {
						eprint(b"traceroute: cannot resolve ");
						eprint(target);
						eprint(b"\n");
						exit();
					}
				}
			}
		};
		catch_interrupt();
		trace(&mut client, &destination, target, max_hops, probes, json);
		close(netsvc);
	}
	exit();
}

// Walk the path: one row per TTL, several probes each, stopping at the destination.
unsafe fn trace(client: &mut NetworkClient, destination: &Ipv4Addr, shown: &[u8], max_hops: u64, probes: u64, json: Option<JsonMode>) {
	unsafe {
		let mut document = String::from("{\"target\":");
		let mut rendered: [u8; 16] = [0u8; 16];
		let length: usize = destination.render(&mut rendered);
		json_escape(&String::from_utf8_lossy(shown), &mut document);
		document.push_str(",\"address\":\"");
		document.push_str(&String::from_utf8_lossy(&rendered[..length]));
		document.push_str("\",\"hops\":[");
		if json.is_none() {
			print(b"traceroute to ");
			print(shown);
			print(b" (");
			print(&rendered[..length]);
			print(b"), ");
			let mut header = String::new();
			push_decimal(&mut header, max_hops);
			print(header.as_bytes());
			print(b" hops max\n");
		}
		let mut wrote_hop = false;
		for ttl in 1..=max_hops {
			if interrupted() {
				break;
			}
			// THE ROW IS BUILT FIRST AND PRINTED WHOLE. A row printed piece by piece as the probes
			// answer would interleave with nothing else here, but it would also mean a trace
			// interrupted halfway leaves a half-line on the screen.
			let mut line: Vec<u8> = Vec::new();
			let mut number = String::new();
			push_decimal(&mut number, ttl);
			for _ in number.len()..3 {
				line.push(b' ');
			}
			line.extend_from_slice(number.as_bytes());
			line.push(b' ');
			// The address this hop reported, so the row names it once rather than after every
			// probe - which is what makes three probes of one router read as one hop.
			let mut named: Option<Ipv4Addr> = None;
			let mut arrived = false;
			let mut refused = false;
			let mut times: Vec<u32> = Vec::new();
			for probe in 0..probes {
				if interrupted() {
					break;
				}
				if probe > 0 {
					// The pacing: a gap between probes, so an answered hop is not a burst.
					let deadline = clock() + PROBE_GAP_TICKS;
					while clock() < deadline {
						if interrupted() {
							break;
						}
						wait(0, deadline);
					}
				}
				let hop = match client.probe(destination, ttl.min(255) as u8) {
					Some(Ok(hop)) => hop,
					// The SERVICE refused or vanished, which is not a property of the path - and
					// carrying on would turn one broken call into a screen of stars.
					_ => {
						eprint(b"traceroute: the network service did not answer\n");
						return;
					}
				};
				match hop.status {
					HopStatus::Timeout => times.push(u32::MAX),
					status => {
						if named.is_none() {
							named = Some(hop.addr);
						}
						times.push(hop.rtt_us);
						if matches!(status, HopStatus::Reply) {
							arrived = true;
						}
						if matches!(status, HopStatus::Unreachable) {
							refused = true;
						}
					}
				}
			}
			match &named {
				Some(addr) => {
					let length: usize = addr.render(&mut rendered);
					line.extend_from_slice(&rendered[..length]);
				}
				// EVERY PROBE SILENT is the conventional row of stars, and it is not a failure: a
				// router that does not answer is a router configured not to answer.
				None => line.extend_from_slice(b"*"),
			}
			for time in &times {
				line.push(b' ');
				if *time == u32::MAX {
					line.push(b'*');
					continue;
				}
				let mut ms = String::new();
				push_decimal(&mut ms, (*time / 1000) as u64);
				ms.push('.');
				push_decimal(&mut ms, ((*time % 1000) / 100) as u64);
				line.extend_from_slice(ms.as_bytes());
				line.extend_from_slice(b"ms");
			}
			if refused {
				line.extend_from_slice(b" !");
			}
			line.push(b'\n');
			if json.is_none() {
				print(&line);
			} else {
				if wrote_hop {
					document.push(',');
				}
				wrote_hop = true;
				document.push_str("{\"hop\":");
				push_decimal(&mut document, ttl);
				document.push_str(",\"address\":");
				match &named {
					Some(addr) => {
						let length: usize = addr.render(&mut rendered);
						json_escape(&String::from_utf8_lossy(&rendered[..length]), &mut document);
					}
					None => document.push_str("null"),
				}
				document.push_str(",\"status\":\"");
				document.push_str(if arrived {
					"reply"
				} else if refused {
					"unreachable"
				} else if named.is_some() {
					"time-exceeded"
				} else {
					"timeout"
				});
				document.push_str("\",\"rtt_us\":[");
				for (index, time) in times.iter().enumerate() {
					if index > 0 {
						document.push(',');
					}
					if *time == u32::MAX {
						document.push_str("null");
					} else {
						push_decimal(&mut document, *time as u64);
					}
				}
				document.push_str("]}");
			}
			// THE TRACE ENDS WHEN THE DESTINATION ANSWERS, and only then. A refusal is a row and
			// not an ending: the next hop may well answer, and a tool that stopped at the first
			// `!` would report a path shorter than the one that exists.
			if arrived {
				break;
			}
		}
		if let Some(mode) = json {
			document.push_str("]}");
			let out = mode.render(document);
			print(out.as_bytes());
			print(b"\n");
		}
	}
}
