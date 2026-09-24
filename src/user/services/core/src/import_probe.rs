// import_probe - MediaImportService's real client, for the kernel's responder scenario.
//
// It holds `media-import`, a second connection of its own for the cross-client check, and a destination on a
// volume the harness serves - no device authority of any kind. The harness plays the camera and decides what
// it reports and when it goes; this program lists, pages, reads and writes, and says what it saw. Each mode is
// one step of the scenario, run as its own process.
//
// THE DESTINATION IS TRANSACTIONAL. Imported bytes go into a volume writer, which publishes nothing until it
// commits, and it commits only on a `complete` ending. Any other ending aborts it, so the file keeps its
// previous contents - which the harness reads back.
//
// THE OBJECTS ARE THE RESPONDER'S: `object_byte(handle, n)` is the byte it serves, and the metadata below is
// what it describes - the same formulas, stated twice on purpose.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, ImportCause, ImportDevice, ImportEntry, ImportEnumeration, ImportInterpretation, ImportObject, ImportObjectId, ImportRead, ImportStorage, ImportTransferState, ImportTransferStatus, LaunchContext, WriterMode, import_cursor, import_transfer, media_import, volume, writer};
use rt::*;
use services::capability_names::*;

// The responder's two storages and its known objects.
const CARD: u32 = 0x0001_0001;
const OVERFLOW: u32 = 0x0002_0001;
const OBJECTS: u32 = 40_000;
const OVERFLOW_OBJECTS: u32 = 65_537;
const FOLDER: u32 = 1;
const PHOTO: u32 = 2;
const PHOTO_BYTES: u64 = 12_288;
const EMPTY: u32 = 3;

fn object_byte(handle: u32, n: u64) -> u8 {
	((n as u32).wrapping_mul(31).wrapping_add(handle.wrapping_mul(7)).wrapping_add((n >> 9) as u32)) as u8
}

fn object_size(handle: u32) -> u64 {
	match handle {
		FOLDER | EMPTY => 0,
		PHOTO => PHOTO_BYTES,
		_ => 64 + u64::from(handle % 32),
	}
}

fn fail(mode: &str, why: &str) -> ! {
	print(format!("import-probe: FAIL {mode}: {why}\n").as_bytes());
	exit();
}

struct Probe {
	mode: &'static str,
	import: u64,
	other: u64,
	storage: u64,
	// The directory the destination files are named in: the launch's working directory. A volume client
	// takes whole `vol://` paths and nothing relative.
	cwd: String,
}

fn import_client(chan: u64) -> media_import::Client<ChannelTransport> {
	media_import::Client::with_deadline(ChannelTransport { chan }, clock() + 500)
}

impl Probe {
	fn fail(&self, why: &str) -> ! {
		fail(self.mode, why)
	}

	// A call that may meet a device busy with another transaction, or this client's own cursor or transfer
	// that it closed a moment ago: closing is a channel going away, which the service sees on its next pass, and
	// until then the one-per-client bound still counts it. Asked again until neither is so - and a slot that is
	// never given back still fails, when the time runs out.
	fn patiently<T>(&self, mut call: impl FnMut() -> Option<Result<T, Error>>) -> Result<T, Error> {
		let deadline = clock() + 1000;
		loop {
			match call() {
				Some(Err(Error::Again | Error::Exhausted)) if clock() < deadline => sleep_until(clock() + 1),
				Some(result) => return result,
				None => self.fail("a call was not answered"),
			}
		}
	}

	fn device(&self) -> ImportDevice {
		let deadline = clock() + 1000;
		loop {
			let devices = match import_client(self.import).devices() {
				Some(Ok(devices)) => devices,
				other => self.fail(&format!("the devices could not be listed: {other:?}")),
			};
			if let Some(device) = devices.into_iter().find(|device| device.model == "Responder" && device.usable && !device.busy) {
				return device;
			}
			if clock() >= deadline {
				self.fail("the responder was never listed usable and idle");
			}
			sleep_until(clock() + 2);
		}
	}

	fn storages(&self, device: &ImportDevice) -> Vec<ImportStorage> {
		match self.patiently(|| import_client(self.import).storages(&device.id)) {
			Ok(storages) => storages,
			Err(error) => self.fail(&format!("the storages could not be listed: {error:?}")),
		}
	}

	fn card(&self) -> ImportStorage {
		let device = self.device();
		self.storages(&device).into_iter().find(|storage| storage.id.storage == CARD).unwrap_or_else(|| self.fail("the card storage is missing"))
	}

	fn enumerate(&self, storage: &ImportStorage) -> (u64, u32) {
		match self.patiently(|| import_client(self.import).open_enumeration(&storage.id, &None)) {
			Ok(ImportEnumeration::Opened(grant)) => (grant.cursor, grant.count),
			other => self.fail(&format!("the card could not be enumerated: {other:?}")),
		}
	}

	fn page(&self, cursor: u64) -> Result<Vec<ImportEntry>, Error> {
		self.patiently(|| import_cursor::Client::with_deadline(ChannelTransport { chan: cursor }, clock() + 500).next()).map(|page| page.entries)
	}

	fn object(&self, entries: &[ImportEntry], handle: u32) -> ImportObject {
		entries
			.iter()
			.find_map(|entry| match entry {
				ImportEntry::Object(object) if object.id.handle == handle => Some(object.clone()),
				_ => None,
			})
			.unwrap_or_else(|| self.fail(&format!("object {handle} was not on its page")))
	}

	fn open(&self, object: &ImportObjectId) -> Result<u64, Error> {
		self.patiently(|| import_client(self.import).open_read(object))
	}

	fn read(&self, transfer: u64) -> ImportRead {
		match import_transfer::Client::with_deadline(ChannelTransport { chan: transfer }, clock() + 1000).read() {
			Some(Ok(read)) => read,
			other => self.fail(&format!("a read failed: {other:?}")),
		}
	}

	fn writer(&self, name: &str) -> u64 {
		let path = format!("{}/{name}", self.cwd.trim_end_matches('/'));
		match volume::Client::with_deadline(ChannelTransport { chan: self.storage }, clock() + 500).open_writer(&path, &WriterMode::Replace) {
			Some(Ok(writer)) => writer,
			other => self.fail(&format!("the destination could not be opened: {other:?}")),
		}
	}
}

fn write_client(chan: u64) -> writer::Client<ChannelTransport> {
	writer::Client::with_deadline(ChannelTransport { chan }, clock() + 500)
}

// Limits, the device and its storages, three pages of exact metadata, the over-limit storage, another client's
// identity, and an operation the interface does not have.
fn browse(probe: &Probe) -> ! {
	let limits = match import_client(probe.import).limits() {
		Some(Ok(limits)) => limits,
		other => probe.fail(&format!("the limits were not advertised: {other:?}")),
	};
	if (limits.providers, limits.clients, limits.cursors, limits.transfers, limits.max_handles, limits.page_records, limits.page_bytes, limits.data_budget) != (8, 16, 8, 8, 65_536, 2, 8192, 4 * 1024 * 1024) {
		probe.fail(&format!("the advertised limits are not the service's: {limits:?}"));
	}
	let device = probe.device();
	if (device.manufacturer.as_str(), device.serial.as_str()) != ("Liber", "0001") {
		probe.fail("the device information was not the responder's");
	}
	let storages = probe.storages(&device);
	let ids: Vec<u32> = storages.iter().map(|storage| storage.id.storage).collect();
	if ids != [CARD, OVERFLOW] || storages[0].label != "CARD" {
		probe.fail(&format!("the storages were not the responder's two: {ids:x?}"));
	}
	let (cursor, count) = probe.enumerate(&storages[0]);
	if count != OBJECTS {
		probe.fail(&format!("the snapshot held {count} handles, not {OBJECTS}"));
	}
	// THREE PAGES, two records each, every field exactly as the responder described it.
	let mut seen = 0u32;
	for page in 0..3 {
		let entries = probe.page(cursor).unwrap_or_else(|error| probe.fail(&format!("page {page} failed: {error:?}")));
		if entries.len() != 2 {
			probe.fail(&format!("page {page} held {} entries", entries.len()));
		}
		for entry in &entries {
			let ImportEntry::Object(object) = entry else { probe.fail("a small record was refused as unrepresentable") };
			seen += 1;
			let handle = object.id.handle;
			if handle != seen {
				probe.fail(&format!("handle {handle} arrived where {seen} was due"));
			}
			let (format, name) = if handle == FOLDER { (0x3001, String::from("DCIM")) } else { (0x3801, format!("IMG_{handle:05}.JPG")) };
			if object.format != format || !object.format_known || object.filename != name || object.size != Some(object_size(handle)) || object.interpretation != ImportInterpretation::Complete || object.original.is_empty() {
				probe.fail(&format!("object {handle}'s record is not what the responder described: {object:?}"));
			}
			let captured = object.captured.as_ref().unwrap_or_else(|| probe.fail("a capture time was missing"));
			if (captured.year, captured.month, captured.day, captured.hour, captured.minute, captured.second, captured.utc_offset_minutes) != (2026, 9, 21, 10, 15, (handle % 60) as u8, Some(0)) {
				probe.fail(&format!("object {handle}'s capture time is wrong: {captured:?}"));
			}
			let parent = object.parent.as_ref().map(|parent| parent.handle);
			if parent != if handle == FOLDER { None } else { Some(FOLDER) } {
				probe.fail(&format!("object {handle}'s parent is wrong: {parent:?}"));
			}
		}
	}
	// ONE CURSOR PER CLIENT: a second is refused while this one is open.
	if !matches!(import_client(probe.import).open_enumeration(&storages[0].id, &None), Some(Err(Error::Exhausted))) {
		probe.fail("a second cursor was admitted to one client");
	}
	close(cursor);
	// THE OVER-LIMIT STORAGE: an explicit answer with its count, and nothing kept.
	match probe.patiently(|| import_client(probe.import).open_enumeration(&storages[1].id, &None)) {
		Ok(ImportEnumeration::OverLimit(count)) if count == OVERFLOW_OBJECTS => {}
		other => probe.fail(&format!("the over-limit storage was not refused with its count: {other:?}")),
	}
	// ANOTHER CLIENT'S IDENTITY is not this client's to use.
	if !matches!(import_client(probe.other).storages(&device.id), Some(Err(Error::Denied))) {
		probe.fail("an identity issued to one client was accepted from another");
	}
	// AN OPERATION THE INTERFACE DOES NOT HAVE ends the connection that sent it; nothing reaches the device.
	let mut request = Vec::new();
	request.extend_from_slice(&11u16.to_le_bytes());
	request.extend_from_slice(&7u32.to_le_bytes());
	request.extend_from_slice(&PHOTO.to_le_bytes());
	send_blocking(probe.import, &request, 0);
	let mut buf = [0u8; 16];
	if !matches!(recv_blocking(probe.import, &mut buf), Received::Closed) {
		probe.fail("an unknown operation was answered rather than refused");
	}
	print(format!("import-probe: PASS browse {count} handles, three pages exact\n").as_bytes());
	exit();
}

// A change the device reports mid-enumeration stales the cursor and every object named before it.
fn stale(probe: &Probe) -> ! {
	let storage = probe.card();
	let (cursor, _) = probe.enumerate(&storage);
	let entries = probe.page(cursor).unwrap_or_else(|error| probe.fail(&format!("the first page failed: {error:?}")));
	let photo = probe.object(&entries, PHOTO);
	print(b"import-probe: paging\n");
	let deadline = clock() + 1000;
	loop {
		match probe.page(cursor) {
			Err(Error::Stale) => break,
			Ok(_) if clock() < deadline => sleep_until(clock() + 2),
			other => probe.fail(&format!("the cursor was not stale after the change: {other:?}")),
		}
	}
	close(cursor);
	if !matches!(probe.open(&photo.id), Err(Error::Stale)) {
		probe.fail("an object named before the change was still readable");
	}
	// Named again under the new epoch, it is readable.
	let storage = probe.card();
	let (cursor, count) = probe.enumerate(&storage);
	let fresh = probe.object(&probe.page(cursor).unwrap_or_else(|error| probe.fail(&format!("the fresh page failed: {error:?}"))), PHOTO);
	close(cursor);
	if count != OBJECTS || fresh.id.device.content_epoch == photo.id.device.content_epoch {
		probe.fail("the fresh snapshot was not taken under a new content epoch");
	}
	print(b"import-probe: PASS stale\n");
	exit();
}

// Read one object whole into a writer, committing only on a complete ending.
fn import_into(probe: &Probe, object: &ImportObject, path: &str) -> ImportTransferStatus {
	let transfer = probe.open(&object.id).unwrap_or_else(|error| probe.fail(&format!("the object could not be opened: {error:?}")));
	let destination = probe.writer(path);
	let mut expected_offset = 0u64;
	let status = loop {
		match probe.read(transfer) {
			ImportRead::Chunk(chunk) => {
				if chunk.offset != expected_offset || chunk.bytes.is_empty() || chunk.bytes.len() > 4096 {
					probe.fail(&format!("a chunk at {} of {} bytes, where {expected_offset} was due", chunk.offset, chunk.bytes.len()));
				}
				if chunk.bytes.iter().enumerate().any(|(at, byte)| *byte != object_byte(object.id.handle, chunk.offset + at as u64)) {
					probe.fail("a chunk's bytes are not the object's");
				}
				if !matches!(write_client(destination).write(&chunk.bytes), Some(Ok(_))) {
					probe.fail("the destination refused a chunk");
				}
				expected_offset += chunk.bytes.len() as u64;
			}
			ImportRead::End(status) => break status,
		}
	};
	if status.state == ImportTransferState::Complete {
		if !matches!(write_client(destination).commit(), Some(Ok(length)) if length == status.delivered) {
			probe.fail("the destination did not commit the complete object");
		}
	} else if !matches!(write_client(destination).abort(), Some(Ok(()))) {
		probe.fail("the destination did not abort");
	}
	close(destination);
	close(transfer);
	status
}

// The 12 kB photo, whole, into the destination - and the zero-byte object, which needs its final response too.
fn import(probe: &Probe) -> ! {
	let storage = probe.card();
	let (cursor, _) = probe.enumerate(&storage);
	let entries = probe.page(cursor).unwrap_or_else(|error| probe.fail(&format!("the page failed: {error:?}")));
	let photo = probe.object(&entries, PHOTO);
	let empty = probe.object(&probe.page(cursor).unwrap_or_else(|error| probe.fail(&format!("the second page failed: {error:?}"))), EMPTY);
	close(cursor);
	let status = import_into(probe, &photo, "photo.jpg");
	if status.state != ImportTransferState::Complete || status.delivered != PHOTO_BYTES || status.expected != PHOTO_BYTES || status.cause.is_some() {
		probe.fail(&format!("the photo did not end complete: {status:?}"));
	}
	let status = import_into(probe, &empty, "empty.bin");
	if status.state != ImportTransferState::Complete || status.delivered != 0 {
		probe.fail(&format!("the zero-byte object did not end complete: {status:?}"));
	}
	print(format!("import-probe: PASS import {} bytes complete\n", PHOTO_BYTES).as_bytes());
	exit();
}

// The device is withdrawn after the first chunk: the transfer ends partial with the count, and the destination
// keeps its previous contents.
fn removal(probe: &Probe) -> ! {
	let storage = probe.card();
	let (cursor, _) = probe.enumerate(&storage);
	let photo = probe.object(&probe.page(cursor).unwrap_or_else(|error| probe.fail(&format!("the page failed: {error:?}"))), PHOTO);
	close(cursor);
	let transfer = probe.open(&photo.id).unwrap_or_else(|error| probe.fail(&format!("the photo could not be opened: {error:?}")));
	let destination = probe.writer("keep.jpg");
	let ImportRead::Chunk(first) = probe.read(transfer) else { probe.fail("the first read was not a chunk") };
	if first.offset != 0 || first.bytes.is_empty() {
		probe.fail("the first chunk was not at the start");
	}
	if !matches!(write_client(destination).write(&first.bytes), Some(Ok(_))) {
		probe.fail("the destination refused the first chunk");
	}
	print(b"import-probe: first chunk\n");
	let deadline = clock() + 1000;
	let status = loop {
		match probe.read(transfer) {
			ImportRead::End(status) => break status,
			ImportRead::Chunk(_) if clock() < deadline => probe.fail("a chunk arrived after the device was withdrawn"),
			ImportRead::Chunk(_) => probe.fail("the transfer never ended"),
		}
	};
	if status.state != ImportTransferState::Partial || status.cause != Some(ImportCause::Removed) || status.delivered != first.bytes.len() as u64 {
		probe.fail(&format!("the withdrawn transfer did not end partial with its count: {status:?}"));
	}
	if !matches!(write_client(destination).abort(), Some(Ok(()))) {
		probe.fail("the destination did not abort");
	}
	close(destination);
	// STATUS STAYS until the transfer is closed.
	if !matches!(import_transfer::Client::with_deadline(ChannelTransport { chan: transfer }, clock() + 500).status(), Some(Ok(kept)) if kept == status) {
		probe.fail("the terminal status was not kept");
	}
	close(transfer);
	print(format!("import-probe: PASS removal partial {} of {} bytes\n", status.delivered, status.expected).as_bytes());
	exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let Some(context) = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) else { exit() };
	let storage = recv_tagged(bootstrap, &mut buf, CAP_STORAGE).unwrap_or(0);
	let granted = recv_tagged(bootstrap, &mut buf, CAP_MEDIA_IMPORT).unwrap_or(0);
	let mode: &'static str = match context.arguments.as_str() {
		"browse" => "browse",
		"stale" => "stale",
		"import" => "import",
		"removal" => "removal",
		_ => fail("import", "no such mode"),
	};
	if granted == 0 || storage == 0 {
		fail(mode, "the import or destination grant was not delivered");
	}
	// THE SECOND CLIENT CONTEXT, which only the harness hands over, and only for the step that uses it.
	let other = if mode == "browse" { recv_tagged(bootstrap, &mut buf, b"IMPORT_OTHER").unwrap_or(0) } else { 0 };
	let probe = Probe { mode, import: granted, other, storage, cwd: context.cwd.clone() };
	match mode {
		"browse" => browse(&probe),
		"stale" => stale(&probe),
		"import" => import(&probe),
		_ => removal(&probe),
	}
}
