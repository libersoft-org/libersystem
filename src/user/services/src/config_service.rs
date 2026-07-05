// ConfigService - the userspace typed configuration service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. ConfigService reports in, then waits for a "SERVE" message
// carrying the channel its clients reach it on. Over that channel clients speak the
// generated `liber:system` Config bindings: they GET a node by its dotted-path key,
// LIST the whole tree, or SET a node, receiving typed `config-entry` records that
// render as CLI / JSON on the client. Configuration is structured data - a typed
// tree, never parsed from text; a textual form would only ever be a representation
// of these nodes.
//
// The store is in memory, seeded with a few system defaults at start (persistence
// is a later phase). When the supervisor that started it drops the bootstrap
// channel, the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::config::{self, Service};
use proto::system::{ConfigEntry, Error};
use rt::*;

// The in-memory configuration tree, behind the generated Config contract. Keys are
// dotted paths (the tree path); the value is the node.
struct Config {
	entries: Vec<ConfigEntry>,
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
		entries.push(ConfigEntry { key: String::from("service.restart-budget"), value: String::from("3") });
		entries.push(ConfigEntry { key: String::from("service.watchdog-ticks"), value: String::from("100") });
		Config { entries }
	}
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
		for e in &mut self.entries {
			if e.key == entry.key {
				e.value = entry.value;
				return Ok(());
			}
		}
		self.entries.push(entry);
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

	// 2. wait for the serve channel clients reach us on. If the supervisor drops the
	//    bootstrap channel first (no clients this boot), we are done.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. serve generated get/list/set requests until the client side closes.
	let mut config: Config = Config::seeded();
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { config::dispatch(&mut config, req, handle, out, reply_handle) });
	}
	exit();
}
