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
use proto::system::{Error, FileInfo, FileType, OpenOpts, volume};
use rt::{ReceivedVecCaps, close, map_object, recv_tagged, recv_vec_caps_blocking, send_blocking, unmap_object};
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
	// The volume refused the listing before any stream existed, and said why. A directory that is
	// not there, a path outside the grant and a volume that could not be read used to arrive here
	// as `Unavailable` - the schema had no error arm, so "no stream" was the only signal.
	Refused(Error),
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
		let consumer = match client.list(path) {
			Some(Ok(consumer)) => consumer,
			Some(Err(e)) => return Err(ListDirectoryError::Refused(e)),
			None => return Err(ListDirectoryError::Unavailable),
		};
		let mut entries = Vec::new();
		loop {
			let mut frame_handles = proto::codec::Handles::new();
			match recv_vec_caps_blocking(consumer, &mut frame_handles) {
				ReceivedVecCaps::Message { bytes } => {
					// The terminal frame: everything before it was the whole directory.
					if bytes.is_empty() {
						close(consumer);
						return Ok(entries);
					}
					let entry = volume::list_read(&bytes, &mut frame_handles);
					for handle in frame_handles.as_slice() {
						close(*handle);
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
				ReceivedVecCaps::Closed => {
					close(consumer);
					return Err(ListDirectoryError::Malformed);
				}
				// The caller asked for a directory's contents and gets an error instead of a
				// prefix. `OutOfMemory` already exists for exactly this and is what an abnormal
				// ending means here.
				ReceivedVecCaps::Failed => {
					close(consumer);
					return Err(ListDirectoryError::OutOfMemory);
				}
			}
		}
	}
}

/// Read one bounded window of a file through a granted volume client.
///
/// The window is what makes a tool bounded: `read_volume_file` maps the whole file, which is right
/// for a configuration file and wrong for a log. A short answer is the end of the file - see the
/// contract at `volume.read` - so a caller streams by repeating this until it gets nothing.
#[inline(always)]
pub unsafe fn read_volume_window(storage: u64, path: &str, offset: u64, length: u32) -> Result<Vec<u8>, ReadFileError> {
	unsafe {
		if storage == 0 {
			return Err(ReadFileError::Unavailable);
		}
		let mut client = VolumeClient::new(storage);
		let buffer = match client.read(path, offset, length) {
			Some(Ok(buffer)) => buffer,
			_ => return Err(ReadFileError::NotFound),
		};
		let length = match usize::try_from(buffer.len) {
			Ok(length) => length,
			Err(_) => {
				close(buffer.handle);
				return Err(ReadFileError::TooLarge);
			}
		};
		if length == 0 {
			// A zero-length window still carries a capability, and closing it is the caller's only
			// chance to: nothing else knows it exists.
			if buffer.handle != 0 {
				close(buffer.handle);
			}
			return Ok(Vec::new());
		}
		let address = match map_object(buffer.handle) {
			Some(address) => address,
			None => {
				close(buffer.handle);
				return Err(ReadFileError::MapFailed);
			}
		};
		let mut bytes = Vec::new();
		if bytes.try_reserve_exact(length).is_err() {
			unmap_object(buffer.handle);
			close(buffer.handle);
			return Err(ReadFileError::OutOfMemory);
		}
		bytes.extend_from_slice(core::slice::from_raw_parts(address as *const u8, length));
		unmap_object(buffer.handle);
		close(buffer.handle);
		Ok(bytes)
	}
}

/// A file on a volume, read a window at a time - the `ChunkSource` every streaming tool consumes.
///
/// It holds ONE chunk, so the memory a tool spends on its input is the chunk size whatever the
/// file's size is. That is the whole difference between `wc` on a log and `wc` on a log that does
/// not fit.
pub struct VolumeSource {
	storage: u64,
	path: String,
	offset: u64,
	chunk: u32,
	held: Vec<u8>,
}

impl VolumeSource {
	#[inline(always)]
	pub fn new(storage: u64, path: &str, chunk: u32) -> VolumeSource {
		VolumeSource { storage, path: String::from(path), offset: 0, chunk: chunk.max(1), held: Vec::new() }
	}

	/// Start reading from `offset` rather than from the beginning - what `tail` and `hexdump` do
	/// once they know where the part they want begins.
	#[inline(always)]
	pub fn seek(&mut self, offset: u64) {
		self.offset = offset;
	}

	/// How far the source has read, which is where the next window starts.
	#[inline(always)]
	pub fn position(&self) -> u64 {
		self.offset
	}
}

impl cli::ChunkSource for VolumeSource {
	#[inline(always)]
	fn next_chunk(&mut self) -> Result<&[u8], cli::ChunkError> {
		match unsafe { read_volume_window(self.storage, &self.path, self.offset, self.chunk) } {
			Ok(bytes) => {
				self.offset = self.offset.saturating_add(bytes.len() as u64);
				self.held = bytes;
				Ok(&self.held)
			}
			Err(ReadFileError::Unavailable) => Err(cli::ChunkError::Unavailable),
			Err(ReadFileError::OutOfMemory) => Err(cli::ChunkError::OutOfMemory),
			Err(_) => Err(cli::ChunkError::Unavailable),
		}
	}
}

/// One entry a walk visited: its full `vol://` path, its facts, and how deep below the root it is.
pub struct Visit<'a> {
	pub path: &'a str,
	pub entry: &'a FileInfo,
	pub depth: usize,
}

/// What a walk's visitor decides about a directory it was shown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
	/// Keep walking, descending into this entry if it is a directory.
	Continue,
	/// Do not descend into this directory, but keep walking the rest.
	Skip,
	/// Stop the walk. What has been visited stays visited.
	Stop,
}

/// Why a walk ended other than by finishing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WalkError {
	/// A directory could not be listed. The walk goes on - one unreadable directory in a tree is
	/// not a reason to abandon the tree - and this is what the caller is told at the end.
	Unreadable,
	/// The tree is deeper or wider than the bounds allowed.
	Bounded,
	OutOfMemory,
}

/// Walk a directory tree ITERATIVELY, one listing at a time, calling `visit` for every entry.
///
/// ITERATIVE and not recursive, with an explicit frontier: a recursive walker's depth is the
/// tree's depth, and a tree's depth is decided by whoever made the directories. `max_depth` bounds
/// how far it descends and `max_pending` bounds how many directories it may owe itself - so a wide
/// tree costs memory proportional to one level rather than to the whole of it.
///
/// The visitor is called for EVERY entry, files and directories alike, before the directory is
/// descended into - so a caller can print a tree as it is discovered rather than after.
#[inline(always)]
pub unsafe fn walk<F: FnMut(Visit<'_>) -> Step>(storage: u64, root: &str, max_depth: usize, max_pending: usize, limit: usize, mut visit: F) -> Result<(), WalkError> {
	let mut pending: Vec<(String, usize)> = Vec::new();
	if pending.try_reserve(1).is_err() {
		return Err(WalkError::OutOfMemory);
	}
	pending.push((String::from(root), 0));
	let mut unreadable = false;
	while let Some((directory, depth)) = pending.pop() {
		let entries = match unsafe { list_volume_directory(storage, &directory, limit) } {
			Ok(entries) => entries,
			// One directory that cannot be read does not end the walk: a tree with a permission
			// hole in it is still worth walking, and the hole is reported once at the end.
			Err(ListDirectoryError::OutOfMemory) => return Err(WalkError::OutOfMemory),
			Err(_) => {
				unreadable = true;
				continue;
			}
		};
		for entry in &entries {
			let mut child = String::new();
			if child.try_reserve_exact(directory.len() + 1 + entry.name.len()).is_err() {
				return Err(WalkError::OutOfMemory);
			}
			child.push_str(&directory);
			if !child.ends_with('/') {
				child.push('/');
			}
			child.push_str(&entry.name);
			match visit(Visit { path: &child, entry, depth }) {
				Step::Stop => return if unreadable { Err(WalkError::Unreadable) } else { Ok(()) },
				Step::Skip => continue,
				Step::Continue => {}
			}
			if entry.r#type != FileType::Dir || depth + 1 > max_depth {
				continue;
			}
			if pending.len() >= max_pending {
				return Err(WalkError::Bounded);
			}
			if pending.try_reserve(1).is_err() {
				return Err(WalkError::OutOfMemory);
			}
			pending.push((child, depth + 1));
		}
	}
	if unreadable { Err(WalkError::Unreadable) } else { Ok(()) }
}

// Drop leading and trailing ASCII whitespace from a byte slice.
#[inline(always)]
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
#[inline(always)]
pub fn split_args(s: &[u8]) -> impl Iterator<Item = &[u8]> {
	s.split(|&b| b == b' ').filter(|t: &&[u8]| !t.is_empty())
}

// Parse an unsigned decimal integer, or None if empty, non-digit, or it overflows u64.
#[inline(always)]
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
#[inline(always)]
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
#[inline(always)]
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

// ONE INPUT CONTRACT FOR EVERY STREAM TOOL, so `wc file` and `cat file | wc` run the same code.
//
// The nine tools this exists for - `grep`, `head`, `tail`, `sort`, `cut`, `tee`, `wc`, `less`,
// `hexdump` - all had the same shape: resolve a path, pick a volume client, loop over
// `read_volume_window`. A pipeline gives them bytes with no path and no volume at all, and the
// wrong way to fix that is nine `if stdin_is_wired` branches, because then nine tools have two
// input paths and the pipeline one is the one nothing tests.
//
// THE MIGRATION CHECKLIST, which is the deliverable this type carries:
//
//   1. Where the tool refuses with "usage: ... <path>" because it got no path, call
//      `Source::from_stdin` instead and refuse only if THAT is absent too. A tool launched with
//      no path and no input stream genuinely has nothing to read.
//   2. Replace the `read_volume_window` loop with `Source::next`, and treat `Window::Failed` as
//      an error rather than as an end - a producer that could not finish must not be reported as
//      a complete short answer.
//   3. Write output through `rt::write_stdout` rather than `print` wherever the tool is in a loop
//      it could stop: `false` means the consumer exited, and a tool that keeps going is doing
//      work for nobody. (Diagnostics stay on `eprint`, which is a different endpoint.)
//   4. Drop `Source` when finished. A consumer that stops early closes its read end, which the
//      producer sees as a broken pipe at its next write - that is the whole propagation mechanism
//      and it needs no code in the tool.
//   5. Do not detect "am I in a pipeline" any other way. There is no environment variable for it
//      and there must not be: the presence of a stdin endpoint is the only signal, it is a
//      capability, and it is the same one the tool reads from.
//
// A tool that follows those five points needs no broader volume grant to work in a pipeline than
// it needs to work on a path - `Source::Stream` holds a channel and nothing else.
//
// EVERY METHOD HERE IS `#[inline(always)]`, like the rest of this file, and that is a link-time
// requirement rather than a performance choice. Each tool is built as one object that may define
// no global but `__user_main`; anything else has to come from a declared shared provider, and this
// crate is not one - it is a dependency compiled into each tool. A `pub fn` without the attribute
// is emitted as an import nothing provides, and the shared build refuses the image by name.
pub enum Source {
	// Bytes from a file, one bounded window at a time through `volume.read`. The window is the
	// existing `VolumeSource`, so the path case here IS the path case every tool already had -
	// this adds a second source beside it rather than a second implementation of the first.
	Volume(VolumeSource),
	// Bytes from the stage upstream. Owns the endpoint, so dropping it breaks the producer's pipe.
	Stream(rt::stream::Reader, Vec<u8>),
}

// What one step of a source produced.
pub enum Window {
	// Bytes to process. Never empty - see `rt::stream::Chunk::Data` for why an empty window
	// cannot be allowed to mean an end.
	Bytes(Vec<u8>),
	// No more input, normally.
	End,
	// The input ended and the thing producing it is reporting that it could not finish. A tool
	// that treats this as `End` publishes a truncated answer as a complete one.
	Failed,
}

impl Source {
	// Read from a path, the way every one of these tools already did. `window` is the tool's own
	// read size, kept a parameter because these tools chose different ones for reasons - 16 KiB
	// for the line readers, 64 KiB for the counters - and collapsing them here would change how
	// many round trips each makes without anybody deciding to.
	#[inline(always)]
	pub fn from_path(storage: u64, uri: &str, window: u32) -> Self {
		Source::Volume(VolumeSource::new(storage, uri, window))
	}

	// Read from the stage upstream, or `None` when nothing is wired to this launch's input.
	//
	// THE ABSENCE IS THE SIGNAL. A terminal launch has no stdin endpoint, a pipeline stage after
	// the first has one, and that is the whole test - no environment variable, no flag, and
	// nothing the tool could be lied to about by a caller that cannot forge a capability.
	#[inline(always)]
	pub unsafe fn from_stdin() -> Option<Self> {
		let input: u64 = unsafe { rt::stdin() };
		if input == 0 { None } else { Some(Source::Stream(rt::stream::Reader::new(input), Vec::new())) }
	}

	// The next window of input, for the tools that work on windows rather than lines. The ones
	// that work on lines wrap this same type in `cli::Lines` through the `ChunkSource` impl below.
	#[inline(always)]
	pub unsafe fn next(&mut self) -> Window {
		use cli::ChunkSource;
		match self.next_chunk() {
			Ok(bytes) if bytes.is_empty() => Window::End,
			Ok(bytes) => Window::Bytes(bytes.to_vec()),
			Err(_) => Window::Failed,
		}
	}

	// Discard the first `count` bytes.
	//
	// A VOLUME SEEKS AND A STREAM DRAINS, which is not an implementation detail a caller can
	// ignore: skipping a gigabyte of a file costs nothing and skipping a gigabyte of a stream
	// costs reading a gigabyte. It is still the right thing to offer, because `hexdump -s` has to
	// mean the same thing on both, and the alternative is a tool that silently ignores its flag
	// on the input it was piped.
	//
	// Returns false when the input ended before `count` bytes, or failed.
	#[inline(always)]
	pub unsafe fn skip(&mut self, count: u64) -> bool {
		if count == 0 {
			return true;
		}
		match self {
			Source::Volume(source) => {
				source.seek(source.position().saturating_add(count));
				true
			}
			Source::Stream(..) => {
				let mut left: u64 = count;
				while left > 0 {
					match unsafe { self.next() } {
						Window::Bytes(bytes) => left = left.saturating_sub(bytes.len() as u64),
						Window::End | Window::Failed => return false,
					}
				}
				true
			}
		}
	}

	// What to call this input in a diagnostic or a per-file label. A stream has no name a user
	// would recognise, and inventing a path for it would be a lie about where the bytes came from.
	#[inline(always)]
	pub fn label(&self) -> &[u8] {
		match self {
			Source::Volume(_) => b"",
			Source::Stream(..) => b"-",
		}
	}
}

// THE REASON `Source` IS A `ChunkSource` AND NOT A SECOND MECHANISM. `cli::Lines` turns any chunk
// source into lines across window boundaries, and `grep`, `sort` and `cut` are built on it. Making
// the stream a chunk source means those three take stdin by changing which value they construct,
// not by growing a second reading loop - and the loop that would have been duplicated is exactly
// the one with the boundary handling in it.
impl cli::ChunkSource for Source {
	#[inline(always)]
	fn next_chunk(&mut self) -> Result<&[u8], cli::ChunkError> {
		match self {
			Source::Volume(source) => source.next_chunk(),
			Source::Stream(reader, held) => {
				held.clear();
				if held.try_reserve(rt::stream::MAX_CHUNK).is_err() {
					return Err(cli::ChunkError::OutOfMemory);
				}
				held.resize(rt::stream::MAX_CHUNK, 0);
				match unsafe { reader.read(held) } {
					rt::stream::Chunk::Data(len) => {
						held.truncate(len);
						Ok(held)
					}
					// An empty chunk is how `ChunkSource` spells the end.
					rt::stream::Chunk::End => {
						held.clear();
						Ok(held)
					}
					// A producer that reported it could not finish. NOT an end: a consumer that
					// cannot tell the two apart prints a truncated answer as a complete one, which
					// is the failure the whole stream contract exists to prevent.
					rt::stream::Chunk::Failed => Err(cli::ChunkError::Unavailable),
				}
			}
		}
	}
}
