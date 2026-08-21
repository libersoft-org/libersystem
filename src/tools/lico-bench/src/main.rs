//! What the LiberCommander core costs at the sizes a person actually meets.
//!
//! The milestone asks for huge directories, large text viewing, highlighting, search and copy, with
//! PEAK MEMORY as well as latency - and the memory half is the reason this is a program rather than
//! a `#[bench]`. Every one of these paths is bounded on purpose (`MAX_OPERATION_ENTRIES`,
//! `MAX_PATTERN_BYTES`, `UNDO_BYTE_BUDGET`), and a bound is a claim about memory that a timing
//! number cannot check. The allocator below counts every byte the run asks for, so each case
//! reports the high-water mark it actually reached rather than the one its constants promise.
//!
//! Host-only and release-only, like the other suites in `./bench.sh`: these are numbers a person
//! asks for, not a gate. Nothing here asserts a threshold - a benchmark that fails a build on a
//! busy machine teaches people to ignore it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

// Live bytes and the high-water mark, both in bytes. Relaxed throughout: this is single-threaded
// and the numbers are a measurement, not a synchronisation.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

impl Counting {
	fn grew(by: usize) {
		let live = LIVE.fetch_add(by, Ordering::Relaxed) + by;
		PEAK.fetch_max(live, Ordering::Relaxed);
	}
}

unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let ptr = unsafe { System.alloc(layout) };
		if !ptr.is_null() {
			Counting::grew(layout.size());
		}
		ptr
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
		unsafe { System.dealloc(ptr, layout) };
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		let out = unsafe { System.realloc(ptr, layout, new_size) };
		if !out.is_null() {
			if new_size >= layout.size() {
				Counting::grew(new_size - layout.size());
			} else {
				LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
			}
		}
		out
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// One measured case. `items` is what the per-item column divides by, and it is the case's own unit -
// entries, lines, bytes - because a single "operations" column over five different shapes would be
// a number nobody could act on.
struct Measured {
	name: &'static str,
	items: u64,
	unit: &'static str,
	nanos: u128,
	peak: usize,
	note: String,
}

// Run `body` with the peak counter reset, so each case reports its OWN high-water mark rather than
// the largest the whole program ever reached. `LIVE` is not reset: it is the real amount held at
// the moment the case starts, and pretending otherwise would understate every case after the first.
fn measure(name: &'static str, items: u64, unit: &'static str, body: impl FnOnce() -> String) -> Measured {
	PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
	let before = LIVE.load(Ordering::Relaxed);
	let start = Instant::now();
	let note = body();
	let nanos = start.elapsed().as_nanos();
	let peak = PEAK.load(Ordering::Relaxed).saturating_sub(before);
	Measured { name, items, unit, nanos, peak, note }
}

// A directory listing of `count` entries, with the shapes an ordering actually has to separate:
// directories and files interleaved, extensions repeating, sizes colliding, and names that share
// long prefixes (the case a comparison cannot answer from its first byte).
fn directory(count: usize) -> Vec<(Vec<u8>, u64, u64, bool)> {
	let extensions = [b"rs".as_slice(), b"toml", b"md", b"lsidl", b""];
	(0..count)
		.map(|index| {
			let extension = extensions[index % extensions.len()];
			let mut name = format!("libersystem-component-{:06}", index).into_bytes();
			if !extension.is_empty() {
				name.push(b'.');
				name.extend_from_slice(extension);
			}
			let is_dir = index % 7 == 0;
			// Sizes collide deliberately: the tie-break on name is the comparison that runs on
			// every one of them, and a benchmark over unique sizes never reaches it.
			(name, (index as u64 % 512) * 4096, 1_700_000_000 + (index as u64 % 86_400), is_dir)
		})
		.collect()
}

// A text body of roughly `bytes` bytes, shaped like source: indentation, strings, comments, block
// comments that span lines, and the occasional very long line.
fn source_text(bytes: usize) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(bytes + 4096);
	let mut line = 0usize;
	while out.len() < bytes {
		match line % 11 {
			0 => out.extend_from_slice(b"// a comment that says why rather than what\n"),
			1 => out.extend_from_slice(b"fn measure(name: &'static str, items: u64) -> Measured {\n"),
			2 => out.extend_from_slice(b"\tlet message = \"a string with an escaped \\\" quote inside it\";\n"),
			3 => out.extend_from_slice(b"\t/* an opening block comment\n"),
			4 => out.extend_from_slice(b"\t   that continues across lines\n"),
			5 => out.extend_from_slice(b"\t   and closes here */\n"),
			6 => out.extend_from_slice(b"\tlet value = compute(index, offset, width) + 1;\n"),
			7 => {
				// One long line per group: the display renderer's per-column work is what this
				// measures, and a file of short lines never shows it.
				out.extend_from_slice(b"\tconst LONG: &str = \"");
				out.extend_from_slice(&vec![b'x'; 900]);
				out.extend_from_slice(b"\";\n");
			}
			8 => out.extend_from_slice("\tlet unicode = \"příliš žluťoučký kůň úpěl ďábelské ódy\";\n".as_bytes()),
			9 => out.extend_from_slice(b"}\n"),
			_ => out.extend_from_slice(b"\n"),
		}
		line += 1;
	}
	out
}

fn main() {
	let mut results: Vec<Measured> = Vec::new();

	// ---- huge directories -------------------------------------------------
	//
	// Fifty thousand is past any directory this system creates and inside what a mounted volume
	// from elsewhere can hand it, which is the point: the panel sorts whatever it is pointed at.
	const ENTRIES: usize = 50_000;
	let entries = directory(ENTRIES);
	for (key, label) in [(lico::SortKey::Name, "name"), (lico::SortKey::Size, "size"), (lico::SortKey::Extension, "extension"), (lico::SortKey::Type, "type")] {
		let spec = lico::SortSpec { key, reverse: false, directories_first: true, show_hidden: true };
		results.push(measure("directory sort", ENTRIES as u64, "entries", || {
			let mut keys: Vec<lico::EntryKey> = entries.iter().map(|(name, size, modified, is_dir)| lico::EntryKey { name, size: *size, modified: *modified, is_dir: *is_dir }).collect();
			keys.sort_by(|left, right| lico::order(spec, left, right));
			format!("by {label}, first {}", String::from_utf8_lossy(keys[0].name))
		}));
	}
	results.push(measure("quick search", ENTRIES as u64, "entries", || {
		// The worst shape for a prefix search: a needle only the last entry answers, so every
		// name is tested and the wrap-around half runs too.
		let names = entries.iter().map(|(name, ..)| name.as_slice());
		let hit = lico::quick_search(names, b"libersystem-component-049999", 0);
		format!("hit {hit:?}")
	}));

	// ---- large text viewing ----------------------------------------------
	const TEXT_BYTES: usize = 8 * 1024 * 1024;
	let text = source_text(TEXT_BYTES);
	let lines: Vec<&[u8]> = text.split(|byte| *byte == b'\n').collect();
	results.push(measure("display render", lines.len() as u64, "lines", || {
		let mut rendered = 0usize;
		let mut out: Vec<u8> = Vec::new();
		for line in &lines {
			out.clear();
			if lico::append_display_line(line, 120, 8, &mut out).is_ok() {
				rendered += 1;
			}
		}
		format!("{rendered} of {} lines rendered at 120 columns", lines.len())
	}));

	// ---- highlighting -----------------------------------------------------
	//
	// The REAL descriptor the system installs, not a toy: the rule count and the nesting are what
	// the per-line cost is made of, and a two-rule descriptor measures nothing about either.
	const RUST_SYNTAX: &[u8] = include_bytes!("../../../volume/bin/lico/syntax/rust.syntax");
	let descriptor = lico::parse_descriptor(RUST_SYNTAX).expect("the installed rust descriptor parses");
	results.push(measure("highlight", lines.len() as u64, "lines", || {
		let mut state = descriptor.initial_state();
		let mut spans = vec![lico::TokenSpan { start: 0, end: 0, style: 0 }; 256];
		let mut tokens = 0u64;
		for line in &lines {
			tokens += descriptor.highlight_line(&mut state, line, &mut spans).spans as u64;
		}
		format!("{tokens} spans from {} rules in {} contexts", descriptor.rule_count(), descriptor.context_count())
	}));

	// ---- search -----------------------------------------------------------
	results.push(measure("text search forward", text.len() as u64, "bytes", || {
		// A needle that is NOT there: the whole body is scanned, which is the case a person waits
		// through. A hit halfway would measure half the work and none of the waiting.
		let query = lico::TextQuery::new(b"a needle this body does not contain");
		format!("miss={:?}", query.find(&text, 0, false))
	}));
	results.push(measure("text search backward", text.len() as u64, "bytes", || {
		let query = lico::TextQuery::new(b"a needle this body does not contain");
		format!("miss={:?}", query.find(&text, text.len(), true))
	}));
	results.push(measure("hex search", text.len() as u64, "bytes", || {
		// A wildcard in the middle, which is what the hex pattern is for and what makes its inner
		// loop different from the text one.
		let pattern = lico::HexPattern::parse(b"6c 69 ?? 6f").expect("a hex pattern with a wildcard");
		format!("hit={:?}", pattern.find(&text, 0, false))
	}));

	// ---- copy planning ----------------------------------------------------
	//
	// At the bound, because that is where the planner's own cost lives: every source is checked
	// against the destination for the cycle `cp -r a a/b`, which is the check the whole planner
	// exists for.
	let names: Vec<Vec<u8>> = (0..lico::MAX_OPERATION_ENTRIES).map(|index| format!("entry-{index:05}").into_bytes()).collect();
	let paths: Vec<Vec<u8>> = names.iter().map(|name| [b"vol://system/src/".as_slice(), name].concat()).collect();
	results.push(measure("copy plan", lico::MAX_OPERATION_ENTRIES as u64, "entries", || {
		let sources: Vec<lico::Source> = paths.iter().zip(&names).enumerate().map(|(index, (path, name))| lico::Source { path, name, is_dir: index % 9 == 0, size: (index as u64) * 1024 }).collect();
		let plan = lico::plan(lico::Operation::Copy, &sources, b"vol://system/backup", lico::Overwrite::Skip).expect("a plan at the entry bound");
		format!("{} steps, {} bytes{}", plan.steps.len(), plan.total_bytes, if plan.total_is_partial { " (partial: a directory was not walked)" } else { "" })
	}));

	// ---- the report -------------------------------------------------------
	println!("lico-bench: the LiberCommander core at size (release, host)");
	println!();
	println!("{:<22} {:>10} {:>12} {:>14} {:>12}   {}", "case", "items", "wall (ms)", "per item (ns)", "peak (KiB)", "note");
	for result in &results {
		let per_item = if result.items == 0 { 0 } else { result.nanos / result.items as u128 };
		println!("{:<22} {:>10} {:>12.3} {:>14} {:>12}   {} [{}]", result.name, result.items, result.nanos as f64 / 1_000_000.0, per_item, result.peak / 1024, result.note, result.unit);
	}
}
