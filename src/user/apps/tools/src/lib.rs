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
use alloc::vec::Vec;
use lico::TerminalWriter;
use proto::codec::JsonMode;
use proto::system::{FileInfo, OpenOpts, volume};
use rt::{Received, ReceivedVec, close, exit, map_object, recv_blocking, recv_tagged, recv_vec_blocking, send_blocking, unmap_object};
use storage_proto::path;
use volume_client::VolumeClient;

/// The shared adapter from a governed tool's full-duplex console capability to the
/// `lico` terminal lifecycle. The individual LiberCommander executables never hand-roll
/// mode writes or their failure behavior.
pub struct ConsoleWriter {
	channel: u64,
}

impl ConsoleWriter {
	#[inline(always)]
	pub const fn new(channel: u64) -> ConsoleWriter {
		ConsoleWriter { channel }
	}
}

impl TerminalWriter for ConsoleWriter {
	#[inline(always)]
	fn write(&mut self, bytes: &[u8]) -> bool {
		self.channel != 0 && unsafe { send_blocking(self.channel, bytes, 0) }
	}
}

/// The seven volume clients carried by the governed `Volumes` capability bundle.
///
/// The count is part of the protocol, not a detail: the bundle is a fixed-order sequence of
/// tagged messages with no length in front of it, so a receiver that reads five when seven were
/// sent leaves two behind - and those are then consumed as whatever it reads next, which is its
/// working directory. Adding a volume means adding it here in the same position.
pub struct VolumeSet {
	pub system: u64,
	pub media: u64,
	pub iso: u64,
	pub udf: u64,
	pub usb: u64,
	pub ram: u64,
	pub tmp: u64,
}

impl VolumeSet {
	/// Receive the fixed-order volume bundle after a tool's argument message.
	#[inline(always)]
	pub unsafe fn receive(bootstrap: u64, buffer: &mut [u8]) -> VolumeSet {
		unsafe { VolumeSet { system: recv_tagged(bootstrap, buffer, b"SYSTEM").unwrap_or(0), media: recv_tagged(bootstrap, buffer, b"MEDIA").unwrap_or(0), iso: recv_tagged(bootstrap, buffer, b"ISO").unwrap_or(0), udf: recv_tagged(bootstrap, buffer, b"UDF").unwrap_or(0), usb: recv_tagged(bootstrap, buffer, b"USB").unwrap_or(0), ram: recv_tagged(bootstrap, buffer, b"RAM").unwrap_or(0), tmp: recv_tagged(bootstrap, buffer, b"TMP").unwrap_or(0) } }
	}

	/// Route one path argument to its already-granted volume client.
	#[inline(always)]
	pub fn client_for(&self, cwd: &str, argument: &[u8]) -> u64 {
		path::volume_client(cwd, argument, self.system, self.media, self.iso, self.udf, self.usb)
	}
}

/// Why a read-only volume file could not be copied into an application's bounded buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadFileError {
	Unavailable,
	NotFound,
	TooLarge,
	OutOfMemory,
	MapFailed,
}

/// Open a file through an already-granted volume client and copy no more than `limit` bytes.
/// The handed file capability is closed on every return path.
#[inline(always)]
pub unsafe fn read_volume_file(storage: u64, path: &str, limit: usize) -> Result<Vec<u8>, ReadFileError> {
	unsafe {
		if storage == 0 {
			return Err(ReadFileError::Unavailable);
		}
		let mut client = VolumeClient::new(storage);
		let opened = match client.open(&OpenOpts { path: String::from(path), write: false, create: false }) {
			Some(Ok(opened)) if opened.file != 0 => opened,
			_ => return Err(ReadFileError::NotFound),
		};
		let length = match usize::try_from(opened.size) {
			Ok(length) if length <= limit => length,
			_ => {
				close(opened.file);
				return Err(ReadFileError::TooLarge);
			}
		};
		if length == 0 {
			close(opened.file);
			return Ok(Vec::new());
		}
		let address = match map_object(opened.file) {
			Some(address) => address,
			None => {
				close(opened.file);
				return Err(ReadFileError::MapFailed);
			}
		};
		let mut bytes = Vec::new();
		if bytes.try_reserve_exact(length).is_err() {
			unmap_object(opened.file);
			close(opened.file);
			return Err(ReadFileError::OutOfMemory);
		}
		bytes.extend_from_slice(core::slice::from_raw_parts(address as *const u8, length));
		unmap_object(opened.file);
		close(opened.file);
		Ok(bytes)
	}
}

/// Why a directory stream could not become a bounded panel listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListDirectoryError {
	Unavailable,
	TooManyEntries,
	OutOfMemory,
	// A frame arrived that would not decode. Its own ending rather than one of the above, because
	// the answer to it is different: retrying an out-of-memory listing may work, retrying a
	// malformed one will not.
	Malformed,
}

/// Collect at most `limit` typed directory entries from a granted volume client.
/// The stream consumer and any unexpected transferred frame handle are closed on every path.
#[inline(always)]
pub unsafe fn list_volume_directory(storage: u64, path: &str, limit: usize) -> Result<Vec<FileInfo>, ListDirectoryError> {
	unsafe {
		if storage == 0 {
			return Err(ListDirectoryError::Unavailable);
		}
		let mut client = VolumeClient::new(storage);
		let consumer = client.list(path).ok_or(ListDirectoryError::Unavailable)?;
		let mut entries = Vec::new();
		loop {
			match recv_vec_blocking(consumer) {
				ReceivedVec::Message { bytes, mut handle } => {
					// The terminal frame: everything before it was the whole directory.
					if bytes.is_empty() {
						close(consumer);
						return Ok(entries);
					}
					let entry = volume::list_read(&bytes, &mut handle);
					if handle != 0 {
						close(handle);
					}
					// A frame that will not decode ends the listing rather than being dropped from
					// it: the caller asked what is in a directory and must not be handed a shorter
					// answer that looks whole.
					let Some(entry) = entry else {
						close(consumer);
						return Err(ListDirectoryError::Malformed);
					};
					{
						if entries.len() == limit {
							close(consumer);
							return Err(ListDirectoryError::TooManyEntries);
						}
						if entries.try_reserve(1).is_err() {
							close(consumer);
							return Err(ListDirectoryError::OutOfMemory);
						}
						entries.push(entry);
					}
				}
				// Closed WITHOUT the terminal frame: the producer gave up part way, so what arrived
				// is a prefix. Returning it as the directory is the defect this marker exists for.
				ReceivedVec::Closed => {
					close(consumer);
					return Err(ListDirectoryError::Malformed);
				}
				// The caller asked for a directory's contents and gets an error instead of a
				// prefix. `OutOfMemory` already exists for exactly this and is what an abnormal
				// ending means here.
				ReceivedVec::Failed => {
					close(consumer);
					return Err(ListDirectoryError::OutOfMemory);
				}
			}
		}
	}
}

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
