// ping - a standalone foreground net tool the shell spawns.
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it alongside the target (a hostname or a
// dotted-decimal address) as its argument. ping resolves the target (parsing it as an
// address, else asking NetworkService to resolve it via DNS), then sends one echo per
// second over its OWN client channel until it reaches the `-c` count or the user
// presses Ctrl+C. Ctrl+C is caught (rt::catch_interrupt) so the output survives the
// interrupt instead of the tool being killed.
//
// The same probe results render in one of two representations - the "one model, many
// codecs" idea applied to a tool's output: by default a line per reply plus
// a statistics summary, or, with `--json`/`-j`, a single JSON document
// {target, address, replies, statistics} that reuses the generated PingReply codec
// for each reply body. Because an unbounded run never produces its final JSON
// document, JSON mode defaults to four probes when no `-c` count is given.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use proto::codec::{json_escape, JsonMode};
use proto::system::{network, Ipv4Addr, PingReply, PingStatus};
use rt::*;

// The representation ping renders its results in. Extend with further codecs (e.g.
// CBOR) as needed - the probe loop is representation-agnostic.
#[derive(Clone, Copy, PartialEq)]
enum OutputFormat {
	Cli,
	Json(JsonMode),
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// The shell hands us our argument (the target plus any flags) and our
		// NetworkService client channel as a transferred capability, in one message.
		inherit_stdout(bootstrap);
		let (len, netsvc): (usize, u64) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => exit(),
		};
		ping(netsvc, &buf[..len]);
		// Drop our client channel (NetworkService reclaims the slot) and exit; the
		// kernel closes the bootstrap with the process, which is what a waiting
		// parent observes.
		close(netsvc);
	}
	exit();
}

// Running ping statistics, all round-trip times in microseconds.
struct Stats {
	transmitted: u32,
	received: u32,
	min_us: u32,
	max_us: u32,
	sum_us: u64,
	sum_sq: u128,
}

impl Stats {
	fn new() -> Stats {
		Stats { transmitted: 0, received: 0, min_us: u32::MAX, max_us: 0, sum_us: 0, sum_sq: 0 }
	}

	// Fold one received reply's round-trip time into the accumulators.
	fn add_reply(&mut self, rtt_us: u32) {
		self.received += 1;
		if rtt_us < self.min_us {
			self.min_us = rtt_us;
		}
		if rtt_us > self.max_us {
			self.max_us = rtt_us;
		}
		self.sum_us += rtt_us as u64;
		self.sum_sq += (rtt_us as u128) * (rtt_us as u128);
	}
}

// Resolve `target` and ping it once per second, printing each reply, until the `-c`
// count is reached or Ctrl+C is pressed - then print the statistics summary.
unsafe fn ping(netsvc: u64, args: &[u8]) {
	unsafe {
		let (count, format, target): (Option<u32>, OutputFormat, &[u8]) = match parse_args(args) {
			Some(parsed) => parsed,
			None => {
				print(b"ping: usage: ping [-c count] [--json] <host>\n");
				return;
			}
		};
		if target.is_empty() {
			print(b"ping: usage: ping [-c count] [--json] <host>\n");
			return;
		}
		// An unbounded ping never produces its final JSON document, so default to four
		// probes in JSON mode when no count was given; CLI keeps its infinite default.
		let count: Option<u32> = match (format, count) {
			(OutputFormat::Json(_), None) => Some(4),
			_ => count,
		};
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		// Resolve the target: a dotted-decimal address parses directly, otherwise ask
		// NetworkService to resolve the name over DNS.
		let addr: Ipv4Addr = match Ipv4Addr::parse(target) {
			Some(a) => a,
			None => match core::str::from_utf8(target).ok().and_then(|name: &str| client.resolve(name)) {
				Some(Ok(a)) => a,
				_ => {
					let mut line: String = String::new();
					line.push_str("ping: cannot resolve ");
					append_bytes(&mut line, target);
					line.push_str(": unknown host\n");
					print(line.as_bytes());
					return;
				}
			},
		};
		let mut ip_buf: [u8; 16] = [0u8; 16];
		let ip_len: usize = addr.render(&mut ip_buf);
		let ip: &[u8] = &ip_buf[..ip_len];
		// PING <target> (<ip>) 56(84) bytes of data. (CLI representation only - the JSON
		// document carries the same target/address in its header fields instead.)
		if format == OutputFormat::Cli {
			let mut header: String = String::new();
			header.push_str("PING ");
			append_bytes(&mut header, target);
			header.push_str(" (");
			append_bytes(&mut header, ip);
			header.push_str(") 56(84) bytes of data.\n");
			print(header.as_bytes());
		}

		// Arm Ctrl+C so we stop cleanly and still emit our output instead of being killed.
		catch_interrupt();

		let start_ns: u64 = clock_ns();
		let mut stats: Stats = Stats::new();
		let mut attempts: Vec<(u32, PingReply)> = Vec::new();
		let mut seq: u32 = 0;
		let mut was_interrupted: bool = false;
		loop {
			seq += 1;
			stats.transmitted += 1;
			let send_ns: u64 = clock_ns();
			match client.ping(&addr) {
				Some(Ok(reply)) => {
					if reply.status == PingStatus::Reply {
						stats.add_reply(reply.rtt_us);
					}
					match format {
						// CLI: one line per reply (timeouts are silent losses).
						OutputFormat::Cli => match reply.status {
							PingStatus::Reply => {
								let mut line: String = String::new();
								line.push_str("64 bytes from ");
								append_bytes(&mut line, ip);
								let _ = write!(line, ": icmp_seq={} ttl={} time=", seq, reply.ttl);
								append_ms2(&mut line, reply.rtt_us);
								line.push_str(" ms\n");
								print(line.as_bytes());
							}
							PingStatus::Unreachable => {
								let mut line: String = String::new();
								line.push_str("From ");
								append_bytes(&mut line, ip);
								let _ = write!(line, " icmp_seq={} Destination Host Unreachable\n", seq);
								print(line.as_bytes());
							}
							PingStatus::Timeout => {}
						},
						// JSON: collect the wire record for the final document.
						OutputFormat::Json(_) => attempts.push((seq, reply)),
					}
				}
				// A service-side error counts as a loss; record it as a timeout in JSON.
				Some(Err(_)) => {
					if format != OutputFormat::Cli {
						attempts.push((seq, PingReply { status: PingStatus::Timeout, ttl: 0, rtt_us: 0 }));
					}
				}
				None => {
					if format == OutputFormat::Cli {
						print(b"ping: network service unavailable\n");
					}
					break;
				}
			}
			if interrupted() {
				was_interrupted = true;
				break;
			}
			if let Some(c) = count {
				if seq >= c {
					break;
				}
			}
			// Sleep until one second after this packet's send time, polling for Ctrl+C
			// in short steps so the interrupt is noticed promptly.
			let wake_ns: u64 = send_ns + 1_000_000_000;
			while clock_ns() < wake_ns {
				if interrupted() {
					was_interrupted = true;
					break;
				}
				wait(netsvc, clock() + 5);
			}
			if was_interrupted {
				break;
			}
		}
		// Render the collected results in the chosen representation.
		match format {
			OutputFormat::Cli => print_summary(target, &stats, start_ns, was_interrupted),
			OutputFormat::Json(mode) => print_json(target, ip, &attempts, &stats, start_ns, mode),
		}
	}
}

// Print the closing statistics block. A leading blank line separates it from the last reply
// only when we stopped on the count; on Ctrl+C the console already echoed "^C" on its own line,
// so none is added.
unsafe fn print_summary(target: &[u8], stats: &Stats, start_ns: u64, was_interrupted: bool) {
	unsafe {
		let elapsed_ms: u64 = clock_ns().saturating_sub(start_ns) / 1_000_000;
		let lost: u32 = stats.transmitted - stats.received;
		let loss_pct: u64 = if stats.transmitted > 0 { lost as u64 * 100 / stats.transmitted as u64 } else { 0 };
		let mut out: String = String::new();
		if !was_interrupted {
			out.push('\n');
		}
		out.push_str("--- ");
		append_bytes(&mut out, target);
		out.push_str(" ping statistics ---\n");
		let _ = write!(out, "{} packets transmitted, {} received, {}% packet loss, time {}ms\n", stats.transmitted, stats.received, loss_pct, elapsed_ms);
		if stats.received > 0 {
			let n: u128 = stats.received as u128;
			let mean: u128 = stats.sum_us as u128 / n;
			let variance: u128 = (stats.sum_sq / n).saturating_sub(mean * mean);
			let mdev_us: u64 = isqrt(variance) as u64;
			out.push_str("rtt min/avg/max/mdev = ");
			append_ms3(&mut out, stats.min_us as u64);
			out.push('/');
			append_ms3(&mut out, mean as u64);
			out.push('/');
			append_ms3(&mut out, stats.max_us as u64);
			out.push('/');
			append_ms3(&mut out, mdev_us);
			out.push_str(" ms\n");
		}
		print(out.as_bytes());
	}
}

// Render the probe results as one JSON document - the machine-readable representation
// selected by `--json`. Each reply body reuses the generated PingReply codec (prefixed
// with the client-side icmp-seq), so the wire model stays the single source of truth;
// it is framed with the target, resolved address, and the same statistics the CLI
// summary reports. Timeouts and lost probes appear as "timeout" replies.
unsafe fn print_json(target: &[u8], ip: &[u8], attempts: &[(u32, PingReply)], stats: &Stats, start_ns: u64, mode: JsonMode) {
	unsafe {
		let elapsed_ms: u64 = clock_ns().saturating_sub(start_ns) / 1_000_000;
		let lost: u32 = stats.transmitted - stats.received;
		let loss_pct: u64 = if stats.transmitted > 0 { lost as u64 * 100 / stats.transmitted as u64 } else { 0 };
		let mut out: String = String::new();
		out.push_str("{\"target\":");
		json_escape(core::str::from_utf8(target).unwrap_or(""), &mut out);
		out.push_str(",\"address\":");
		json_escape(core::str::from_utf8(ip).unwrap_or(""), &mut out);
		out.push_str(",\"replies\":[");
		let mut first: bool = true;
		for (seq, reply) in attempts {
			if !first {
				out.push(',');
			}
			first = false;
			// Reuse the wire record's JSON, prepending the client-side sequence number.
			let _ = write!(out, "{{\"icmp-seq\":{},", seq);
			out.push_str(&reply.to_json()[1..]);
		}
		out.push_str("],\"statistics\":{");
		let _ = write!(out, "\"transmitted\":{},\"received\":{},\"packet-loss-pct\":{},\"time-ms\":{},\"rtt\":", stats.transmitted, stats.received, loss_pct, elapsed_ms);
		if stats.received > 0 {
			let n: u128 = stats.received as u128;
			let mean: u128 = stats.sum_us as u128 / n;
			let variance: u128 = (stats.sum_sq / n).saturating_sub(mean * mean);
			let mdev_us: u64 = isqrt(variance) as u64;
			let _ = write!(out, "{{\"min-us\":{},\"avg-us\":{},\"max-us\":{},\"mdev-us\":{}}}", stats.min_us, mean as u64, stats.max_us, mdev_us);
		} else {
			out.push_str("null");
		}
		out.push_str("}}");
		print(mode.render(out).as_bytes());
		print(b"\n");
	}
}

// Parse `[-c count] [--json] <host>` (in any order), returning the optional count, the
// chosen output format, and the target. None on a malformed count or an unknown flag.
fn parse_args(args: &[u8]) -> Option<(Option<u32>, OutputFormat, &[u8])> {
	let mut count: Option<u32> = None;
	let mut format: OutputFormat = OutputFormat::Cli;
	let mut target: &[u8] = b"";
	let mut rest: &[u8] = args;
	loop {
		rest = skip_spaces(rest);
		if rest.is_empty() {
			break;
		}
		let (tok, after): (&[u8], &[u8]) = next_token(rest);
		if tok == b"-c" {
			let (num, after_num): (&[u8], &[u8]) = next_token(skip_spaces(after));
			count = Some(parse_u32(num)?);
			rest = after_num;
		} else if tok == b"--json" || tok == b"-j" || tok == b"json" {
			format = OutputFormat::Json(JsonMode::Pretty);
			rest = after;
		} else if tok == b"--json-min" || tok == b"json-min" {
			format = OutputFormat::Json(JsonMode::Min);
			rest = after;
		} else if tok.first() == Some(&b'-') {
			return None;
		} else {
			target = tok;
			rest = after;
		}
	}
	Some((count, format, target))
}

// Drop leading spaces.
fn skip_spaces(s: &[u8]) -> &[u8] {
	let mut i: usize = 0;
	while i < s.len() && s[i] == b' ' {
		i += 1;
	}
	&s[i..]
}

// Split off the next space-delimited token, returning it and the remainder.
fn next_token(s: &[u8]) -> (&[u8], &[u8]) {
	let mut i: usize = 0;
	while i < s.len() && s[i] != b' ' {
		i += 1;
	}
	(&s[..i], &s[i..])
}

// Parse a decimal u32, or None if empty or non-numeric.
fn parse_u32(s: &[u8]) -> Option<u32> {
	if s.is_empty() {
		return None;
	}
	let mut val: u32 = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
	}
	Some(val)
}

// Append raw ASCII bytes (a hostname or rendered address) to the string.
fn append_bytes(out: &mut String, bytes: &[u8]) {
	for &b in bytes {
		out.push(b as char);
	}
}

// Append microseconds as milliseconds with two decimals ("1.02"), rounded.
fn append_ms2(out: &mut String, us: u32) {
	let hundredths: u64 = (us as u64 + 5) / 10;
	let _ = write!(out, "{}.{:02}", hundredths / 100, hundredths % 100);
}

// Append microseconds as milliseconds with three decimals ("1.020").
fn append_ms3(out: &mut String, us: u64) {
	let _ = write!(out, "{}.{:03}", us / 1000, us % 1000);
}

// Integer square root (floor) for the mean-deviation computation.
fn isqrt(n: u128) -> u128 {
	if n == 0 {
		return 0;
	}
	let mut x: u128 = n;
	let mut y: u128 = (x + 1) / 2;
	while y < x {
		x = y;
		y = (x + n / x) / 2;
	}
	x
}
