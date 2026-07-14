// ConfigService - the userspace typed configuration service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. ConfigService reports in, then waits for its bootstrap
// deliveries: an optional "STORAGE" message carrying a system-volume client (the
// persistence backing), then a "SERVE" message carrying the channel its clients
// reach it on. Over that channel clients speak the
// generated `liber:system` Config bindings: they GET a node by its dotted-path key,
// LIST the whole tree, or SET a node, receiving typed `config-entry` records that
// render as CLI / JSON on the client. Configuration is structured data - a typed
// tree, never parsed from text; a textual form would only ever be a representation
// of these nodes.
//
// The tree is durable: it loads from `vol://system/config.tree` at start - the
// persisted nodes overriding (and extending) the seeded defaults, so a new default
// key in a later build still appears while an operator's `set` values win - and
// every successful SET writes the whole tree back through the volume client. A
// `config set` therefore survives both a transparent ConfigService restart (the
// replacement reloads the file) and a reboot. Without a volume (a test scenario,
// or storage never came up) the tree is in-memory, exactly as before. When the
// supervisor that started it drops the bootstrap channel, the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::config::{self, Service};
use proto::system::{ConfigEntry, Error, OpenOpts, volume};
use rt::*;

// The persisted tree's location on the system volume, and its format magic (a
// structured, versioned binary - never parsed text): the magic, a count, then per
// entry a length-prefixed key and value.
const TREE_PATH: &str = "vol://system/config.tree";
const TREE_MAGIC: &[u8; 8] = b"LSCFGTR1";

// The configuration tree, behind the generated Config contract. Keys are dotted
// paths (the tree path); the value is the node. `volume` is the persistence
// backing (0 = in-memory only).
struct Config {
	entries: Vec<ConfigEntry>,
	volume: u64,
}

impl Config {
	// The default tree: a few real system facts the other services also know, and
	// the bounded-by-nature policy knobs their owning services read from here (each
	// seeded with the value that used to be that service's compiled-in constant).
	fn seeded() -> Config {
		let mut entries: Vec<ConfigEntry> = Vec::new();
		entries.push(ConfigEntry { key: String::from("system.name"), value: String::from("LiberSystem") });
		entries.push(ConfigEntry { key: String::from("system.volume"), value: String::from("system") });
		entries.push(ConfigEntry { key: String::from("shell.prompt"), value: String::from("> ") });
		// ConsoleService reads these at every VT creation, so a set applies to the
		// next VT; LogService reads its journal depth when the supervisor delivers
		// its config client; NetworkService reads the neighbor-cache size at start;
		// ServiceManager reads its supervision knobs once ConfigService is up.
		entries.push(ConfigEntry { key: String::from("console.scrollback"), value: String::from("1000") });
		entries.push(ConfigEntry { key: String::from("console.history"), value: String::from("512") });
		entries.push(ConfigEntry { key: String::from("log.capacity"), value: String::from("4096") });
		// On-disk journal rotation: bytes per boot file (0 = derive from the volume's
		// size) and how many boots to keep.
		entries.push(ConfigEntry { key: String::from("log.disk-cap"), value: String::from("0") });
		entries.push(ConfigEntry { key: String::from("log.boots"), value: String::from("8") });
		entries.push(ConfigEntry { key: String::from("net.arp-cache"), value: String::from("1024") });
		entries.push(ConfigEntry { key: String::from("net.mtu"), value: String::from("1500") });
		entries.push(ConfigEntry { key: String::from("service.restart-budget"), value: String::from("3") });
		entries.push(ConfigEntry { key: String::from("service.watchdog-ticks"), value: String::from("100") });
		Config { entries, volume: 0 }
	}

	// The durable tree: the seeded defaults overlaid with whatever
	// `vol://system/config.tree` persisted - a set value wins over its default, a
	// persisted key with no default is appended, and a NEW default in a later build
	// still appears (it has no persisted override yet). With no volume, or no file
	// (first boot), the seeded defaults stand alone.
	fn load(volume: u64) -> Config {
		let mut config: Config = Config::seeded();
		config.volume = volume;
		if volume == 0 {
			return config;
		}
		for (key, value) in read_tree(volume) {
			match config.entries.iter_mut().find(|e| e.key == key) {
				Some(entry) => entry.value = value,
				None => config.entries.push(ConfigEntry { key, value }),
			}
		}
		config
	}

	// Write the whole tree through to the volume (the write-through of every SET).
	// Best-effort: with no volume the tree is in-memory by design, and a failed
	// write (a read-only test volume) keeps serving the in-memory value.
	fn persist(&self) {
		if self.volume == 0 {
			return;
		}
		let mut bytes: Vec<u8> = Vec::new();
		bytes.extend_from_slice(TREE_MAGIC);
		bytes.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
		for e in &self.entries {
			bytes.extend_from_slice(&(e.key.len() as u16).to_le_bytes());
			bytes.extend_from_slice(e.key.as_bytes());
			bytes.extend_from_slice(&(e.value.len() as u16).to_le_bytes());
			bytes.extend_from_slice(e.value.as_bytes());
		}
		let data: proto::codec::Buffer = match unsafe { make_buffer(&bytes) } {
			Some(b) => b,
			None => return,
		};
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let _ = client.write(TREE_PATH, &data);
	}
}

// Read the persisted tree back: open + map `vol://system/config.tree` and decode
// its entries. Empty when the file does not exist (first boot), the magic is wrong
// (a future format bumps it), or a record is truncated (the rest is dropped - the
// seeded defaults cover the loss).
fn read_tree(volume: u64) -> Vec<(String, String)> {
	let mut client = volume::Client::new(ChannelTransport { chan: volume });
	let opts: OpenOpts = OpenOpts { path: String::from(TREE_PATH), write: false, create: false };
	let result = match client.open(&opts) {
		Some(Ok(r)) if r.file != 0 => r,
		_ => return Vec::new(),
	};
	let mapped: u64 = match unsafe { map_object(result.file) } {
		Some(base) => base,
		None => {
			unsafe { close(result.file) };
			return Vec::new();
		}
	};
	let bytes: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, result.size as usize) };
	let mut entries: Vec<(String, String)> = Vec::new();
	if bytes.len() >= 12 && &bytes[..8] == TREE_MAGIC {
		let count: usize = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
		let mut at: usize = 12;
		for _ in 0..count {
			let Some((key, next)) = read_lp(bytes, at) else { break };
			let Some((value, next)) = read_lp(bytes, next) else { break };
			entries.push((key, value));
			at = next;
		}
	}
	unsafe {
		unmap_object(result.file);
		close(result.file);
	}
	entries
}

// One length-prefixed UTF-8 string ([len u16][bytes]) at `at`, and the offset past
// it. None when truncated or not UTF-8.
fn read_lp(bytes: &[u8], at: usize) -> Option<(String, usize)> {
	if at + 2 > bytes.len() {
		return None;
	}
	let len: usize = u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
	let end: usize = at + 2 + len;
	if end > bytes.len() {
		return None;
	}
	Some((String::from(core::str::from_utf8(&bytes[at + 2..end]).ok()?), end))
}

impl Service for Config {
	fn get(&mut self, key: String) -> Result<String, Error> {
		for e in &self.entries {
			if e.key == key {
				return Ok(e.value.clone());
			}
		}
		Err(Error::NotFound)
	}

	fn list(&mut self) -> Result<Vec<ConfigEntry>, Error> {
		Ok(self.entries.clone())
	}

	fn set(&mut self, entry: ConfigEntry) -> Result<(), Error> {
		match self.entries.iter_mut().find(|e| e.key == entry.key) {
			Some(e) => e.value = entry.value,
			None => self.entries.push(entry),
		}
		// Write-through: the set survives a service restart and a reboot.
		self.persist();
		Ok(())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"ConfigService: online", 0);
	}

	// 2. wait for the bootstrap deliveries: an optional "STORAGE" system-volume
	//    client (the persistence backing - absent in a scenario without storage, the
	//    tree then stays in-memory), then the serve channel clients reach us on. If
	//    the supervisor drops the bootstrap channel first (no clients this boot), we
	//    are done.
	let mut vol: u64 = 0;
	let service: u64 = loop {
		match unsafe { recv_blocking(bootstrap, &mut buf) } {
			Received::Message { len, handle } if len >= 7 && &buf[..7] == b"STORAGE" => vol = handle,
			Received::Message { len, handle } if len >= 5 && &buf[..5] == b"SERVE" && handle != 0 => break handle,
			Received::Message { .. } => {}
			Received::Closed => exit(),
		}
	};

	// 3. serve generated get/list/set requests until the client side closes, over
	//    the durable tree (the persisted nodes overlaid on the seeded defaults).
	let mut config: Config = Config::load(vol);
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: &mut u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { config::dispatch(&mut config, req, handle, out, reply_handle) });
	}
	exit();
}
