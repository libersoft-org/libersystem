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
// The tree is durable: it loads from `vol://system/libexec/config_service/config.tree` at start - the
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
use ipc_client::{ChannelTransport, make_buffer};
use proto::system::config::{self, Service};
use proto::system::{ConfigEntry, Error, OpenOpts, volume};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/runtime_path.rs"));

// The persisted tree's format magic (a structured, versioned binary - never parsed
// text): the magic, a count, then per entry a length-prefixed key and value.
const TREE_MAGIC: &[u8; 8] = b"LSCFGTR1";

fn tree_path() -> &'static str {
	runtime_path("config-tree").expect("manifest config-tree path")
}

// The configuration tree, behind the generated Config contract. Keys are dotted
// paths (the tree path); the value is the node. `volume` is the persistence
// backing (0 = in-memory only).
struct Config {
	entries: Vec<ConfigEntry>,
	volume: u64,
	// WHETHER THE REQUEST BEING SERVED ARRIVED ON THE OWNER'S CONNECTION. Set per request from the
	// channel `serve_multi` reports, which is the only identity a server has for its callers - see
	// the `POLICYOWNER` role and `Config::set`.
	privileged: bool,
	// THE CONNECTION THE REQUEST BEING SERVED ARRIVED ON, and the ones that have been sealed.
	//
	// A sealed connection may read and may not write. It is the same per-request channel identity
	// `privileged` uses, pointed the other way: that one names the ONE caller allowed more than the
	// rest, this one names the callers allowed less. Both exist because authority here comes from
	// WHICH CONNECTION a request arrived on and from nothing else.
	//
	// Sealing is done by whoever GRANTS the connection - PermissionManager seals the client it mints
	// for a status tool before handing it over - so it is not a promise the holder makes about
	// itself, and there is no unseal.
	current: u64,
	sealed: Vec<u64>,
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
		Config { entries, volume: 0, privileged: false, current: 0, sealed: Vec::new() }
	}

	// The durable tree: the seeded defaults overlaid with whatever
	// `vol://system/libexec/config_service/config.tree` persisted - a set value wins over its default, a
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
	//
	// ANSWERS WHETHER THE BYTES REACHED THE VOLUME (2026-09-03). This was best-effort and silent, so
	// `set` returned `Ok(())` for a write that never landed - and a device policy an operator was
	// told had been stored was gone at the next boot. The in-memory tree is still updated either
	// way, because the live effect is real and a caller that has already been given it must not be
	// told the request failed outright; what changes is that the CALLER is told the record is not
	// durable, which is the one thing only this service knows.
	//
	// With no volume at all the tree is in-memory BY DESIGN - that is what a test fixture and the
	// pre-mount boot both are - so that case is a success and not a silent failure.
	fn persist(&self) -> bool {
		if self.volume == 0 {
			return true;
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
			None => return false,
		};
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let tree_path = tree_path();
		let owner_directory = tree_path.rsplit_once('/').map(|(directory, _)| directory).expect("manifest config-tree parent");
		let artifact_directory = owner_directory.rsplit_once('/').map(|(directory, _)| directory).expect("manifest config-tree artifact parent");
		let _ = client.mkdir(artifact_directory);
		let _ = client.mkdir(owner_directory);
		matches!(client.write(tree_path, &data), Some(Ok(_)))
	}
}

// Read the persisted tree back: open + map `vol://system/libexec/config_service/config.tree` and decode
// its entries. Empty when the file does not exist (first boot), the magic is wrong
// (a future format bumps it), or a record is truncated (the rest is dropped - the
// seeded defaults cover the loss).
fn read_tree(volume: u64) -> Vec<(String, String)> {
	let mut client = volume::Client::new(ChannelTransport { chan: volume });
	let opts: OpenOpts = OpenOpts { path: String::from(tree_path()), write: false, create: false };
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

impl Config {
	// Whether the connection this request arrived on has been sealed.
	fn is_sealed(&self) -> bool {
		self.current != 0 && self.sealed.contains(&self.current)
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
		// THE RESERVED NAMESPACE IS DEVICEMANAGER'S, AND `set` NOW CHECKS THAT.
		//
		// `remove` refused every key outside this prefix and this checked nothing at all, so any
		// component holding `CAP_CONFIG` - the supervisor grants it to several - could overwrite or
		// delete a device's stored policy. Authority over a namespace comes from WHICH CONNECTION the
		// request arrived on; `privileged` is set per request from the served channel, and only the
		// seeded `POLICYOWNER` pair carries it.
		if entry.key.as_bytes().starts_with(DEVICE_POLICY_PREFIX.as_bytes()) && !self.privileged {
			return Err(Error::Denied);
		}
		// AND A SEALED CONNECTION WRITES NOTHING AT ALL. The prefix rule above bounds what an
		// ordinary client may overwrite to "everything outside the reserved namespace", which is
		// still authority over unrelated system configuration - too much for a component that was
		// granted a configuration client only to READ one record.
		if self.is_sealed() {
			return Err(Error::Denied);
		}
		match self.entries.iter_mut().find(|e| e.key == entry.key) {
			Some(e) => e.value = entry.value,
			None => self.entries.push(entry),
		}
		// Write-through: the set survives a service restart and a reboot - and when it does not,
		// the caller is told rather than left believing it does.
		if !self.persist() {
			unsafe { print(b"ConfigService: the tree could not be written through to its volume - the value is served from memory and will not survive a restart\n") };
			return Err(Error::Io);
		}
		Ok(())
	}

	// DELETE ONE KEY, AND ONLY UNDER THE RESERVED PREFIX.
	//
	// This exists for exactly one thing: an operator `enable` is the REMOVAL of a device's disable
	// record, not a third stored state - so a device that was never disabled and one that was
	// enabled again are the same device. A general delete would be a far wider authority than that
	// question needs, and `set ""` meaning absent would make an empty string a third state nobody
	// declared.
	fn remove(&mut self, key: alloc::string::String) -> Result<(), Error> {
		if !key.starts_with(DEVICE_POLICY_PREFIX) {
			return Err(Error::Denied);
		}
		// AND ONLY THE OWNER MAY REMOVE ONE. Refusing every key outside the prefix bounded WHAT could
		// be deleted and said nothing about WHO - so an `enable` from any `CAP_CONFIG` holder erased
		// a disable DeviceManager had stored.
		if !self.privileged || self.is_sealed() {
			return Err(Error::Denied);
		}
		let before = self.entries.len();
		self.entries.retain(|entry| entry.key != key);
		// A KEY THAT WAS NOT THERE IS NOT AN ERROR. `enable` on a device nobody disabled must
		// succeed: the caller is asking for a state, and the state is already what they asked for.
		if self.entries.len() != before && !self.persist() {
			unsafe { print(b"ConfigService: a removal could not be written through to its volume - the key is gone from memory and will come back at the next restart\n") };
			return Err(Error::Io);
		}
		Ok(())
	}

	// SEAL THE CONNECTION THIS ARRIVED ON. Idempotent, and there is no way back: a connection that
	// could be unsealed by its holder would be a promise rather than a restriction.
	fn seal(&mut self) -> Result<(), Error> {
		if self.current == 0 {
			return Err(Error::Denied);
		}
		if !self.sealed.contains(&self.current) {
			self.sealed.push(self.current);
		}
		Ok(())
	}
}

// The reserved namespace: the one prefix `remove` will touch, and the one `set` refuses to anybody
// but its owner.
//
// THE HOLE THIS COMMENT USED TO RECORD IS CLOSED. It said `set` checked nothing - true, and it was
// the whole of DeviceManager's persistent device policy, writable by any of the several components
// ServiceManager grants `CAP_CONFIG` to - and that it could not be fixed until ConfigService could
// tell its callers apart. It can: `serve_multi_seeded` passes the channel a request arrived on, the
// supervisor mints ONE serve root of its own for this, and `privileged` is that comparison. Both
// verbs consult it now.
//
// AND IT IS ASKED OF THE RUNNING SERVICE, which is the part a comment cannot carry: the rule was
// written twice here and was wrong both times, so
// `kernel.volume_layout.the_reserved_device_policy_namespace_answers_only_its_owner` sends the same
// well-formed policy write down an ordinary connection and down the owner's, and reads the record
// back to say which answer was true. WRITING is what the prefix reserves - an ordinary connection
// may still READ a stored policy, because seeing that a device is disabled is not disabling one.
const DEVICE_POLICY_PREFIX: &str = "device.policy.";

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
	// THE OWNER'S SERVE ROOT, minted by the supervisor like every other. It arrives BEFORE `SERVE`,
	// because the loop below ends on `SERVE` - see the role's comment in the manifest.
	let mut owner_server: u64 = 0;
	let service: u64 = loop {
		match unsafe { recv_blocking(bootstrap, &mut buf) } {
			Received::Message { len, handle } if len >= 7 && &buf[..7] == b"STORAGE" => vol = handle,
			Received::Message { len, handle } if len >= 11 && &buf[..11] == b"POLICYOWNER" => owner_server = handle,
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
	// THE OWNER'S CONNECTION, SEEDED SO ITS CHANNEL IS KNOWABLE.
	//
	// A connection minted on demand from the ordinary root is indistinguishable from every other, so
	// no server can tell which of its callers is allowed the reserved namespace. This one is a serve
	// root of its own: the supervisor minted the pair and gave this end to this program, and seeding
	// it into the serve set is what makes its channel value the identity `Config::set` compares
	// against. The supervisor keeps the client end and hands it to DeviceManager.
	unsafe {
		let seed: [u64; 1] = [owner_server];
		let seeded: &[u64] = if owner_server != 0 { &seed } else { &[] };
		serve_multi_seeded(service, seeded, &mut request, &mut reply, |chan, req, handle, out, reply_handle| -> Option<usize> {
			config.privileged = owner_server != 0 && chan == owner_server;
			config.current = chan;
			config::dispatch(&mut config, req, handle, out, reply_handle)
		});
	}
	exit();
}
