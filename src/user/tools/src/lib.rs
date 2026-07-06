// Shared helpers for the standalone tools.
//
// The tools are separate ELF programs (one `[[bin]]` each) the shell spawns, so they
// cannot share code the way modules of one program do - yet many repeat the same tiny
// routines: trimming argument whitespace, splitting an argument string into words,
// parsing decimal numbers and ports, formatting decimals into a JSON document, and the
// receive-the-argument-then-parse-a-JsonMode handshake every `--json`-capable tool
// performs. Those live here once; each bin pulls them in with `use tools::*`, so the
// routing and the parsing/formatting cannot drift between tools.

#![no_std]

extern crate alloc;

use alloc::string::String;
use proto::codec::JsonMode;
use rt::{Received, exit, recv_blocking};

// Drop leading and trailing ASCII whitespace from a byte slice.
pub fn trim(s: &[u8]) -> &[u8] {
	let mut start: usize = 0;
	let mut end: usize = s.len();
	while start < end && s[start].is_ascii_whitespace() {
		start += 1;
	}
	while end > start && s[end - 1].is_ascii_whitespace() {
		end -= 1;
	}
	&s[start..end]
}

// Iterate the space-separated, non-empty words of an argument string - the shared
// tokenizer behind the tools that scan their arguments word by word.
pub fn split_args(s: &[u8]) -> impl Iterator<Item = &[u8]> {
	s.split(|&b| b == b' ').filter(|t: &&[u8]| !t.is_empty())
}

// Parse an unsigned decimal integer, or None if empty, non-digit, or it overflows u64.
pub fn parse_u64(s: &[u8]) -> Option<u64> {
	if s.is_empty() {
		return None;
	}
	let mut v: u64 = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
	}
	Some(v)
}

// Parse a decimal port number (0-65535), or None if malformed or out of range.
pub fn parse_port(s: &[u8]) -> Option<u16> {
	if s.len() > 5 {
		return None;
	}
	match parse_u64(s) {
		Some(v) if v <= 65535 => Some(v as u16),
		_ => None,
	}
}

// Append a decimal number to `out` - the digit formatter the tools use when building
// JSON documents and human-readable sizes.
pub fn push_decimal(out: &mut String, value: u64) {
	let mut digits: [u8; 20] = [0u8; 20];
	let mut v: u64 = value;
	let mut n: usize = 0;
	loop {
		digits[n] = b'0' + (v % 10) as u8;
		v /= 10;
		n += 1;
		if v == 0 {
			break;
		}
	}
	for i in 0..n {
		out.push(digits[n - 1 - i] as char);
	}
}

// Receive a tool's argument string (the first bootstrap message) and parse the JSON
// mode it selects: `Some` for `json` / `json-min`, `None` for the default text form.
// The peer closing before the argument arrives means the launcher gave up, so the tool
// exits - the same handshake every `--json`-capable tool performs.
//
// # Safety
// `bootstrap` must be the tool's live bootstrap channel handle.
pub unsafe fn recv_json_mode(bootstrap: u64, buf: &mut [u8]) -> Option<JsonMode> {
	match unsafe { recv_blocking(bootstrap, buf) } {
		Received::Message { len, .. } => JsonMode::parse(&buf[..len]),
		Received::Closed => exit(),
	}
}
