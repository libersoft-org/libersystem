// HibernationService - THE IMAGE COMPONENT: one program writes a hibernation image and restores it, because the key the
// image is sealed with opens only for the component that sealed it.
//
// WHAT IT HOLDS, each filled by ServiceManager at every start: the Hibernation privilege (the snapshot's pages read, and
// memory replaced); the hibernation area on the system disk, through the system volume's StorageService; a `tpm-seal`
// grant minted for this component; and the restore's door.
//
// AT EVERY START, after it has reported in: THE IMAGE FOUND AT MOUNT, if the area holds one. Its header is read, its
// key-encryption key unsealed (PCR 4, the loader as the firmware measured it), the header authenticated and the image key
// unwrapped; the image must be this system image's, for this hardware. Every chunk's frames are read first and handed to
// the kernel, which keeps every page of the image out of the frames they go to; then every chunk is read, authenticated
// and decrypted into the kernel, and a chunk whose frames differ from what the first pass read is a modified image. The
// header is invalidated before anything irreversible - an image is restored at most once - the restore's door stops every
// binding of this boot, and memory is replaced. ANY REFUSAL - modified, another system image, other hardware, a key that
// does not unseal (the loader changed), an image that does not fit - invalidates the header too, gives the kernel back
// what it held, and lets the boot go on: the system volume's held writes go, and ServiceManager hands LogService its
// journal.
//
// WHAT IT SERVES, on the control channel ServiceManager holds for it (`hibernation-image`): `status` - set up where the
// area is at least as large as memory and a TPM seals; `write-image` - the kernel's snapshot sealed, encrypted and
// written, then given back; `discard-image` - the header invalidated, when a machine runs on after writing an image.
//
// THE FORMAT AND ITS CRYPTOGRAPHY are `service_logic::hibernation`'s.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, HibernationStatus, Outcome, hibernation_area, hibernation_image, hibernation_restore, tpm};
use rt::*;
use service_logic::hibernation::{self, CHUNK_HEAD, CHUNK_PAGES, HEADER_BYTES, Header, ImageKey, KEK, PAGE, Refusal, STATE_EMPTY, STATE_IMAGE};

include!(concat!(env!("OUT_DIR"), "/roles_hibernation_service.rs"));

// THE PCR THE KEY IS SEALED TO: the loader, as the firmware measured it.
const PCR: u32 = 4;
// How long one call to the area, the TPM or the restore's door may take.
const CALL_TICKS: u64 = 30 * TICKS_PER_SECOND;
// HOW LONG A BOOT WAITS FOR ITS TPM: TpmService is up before this component, and the TPM's driver is bound when
// DeviceManager's drivers are - not necessarily yet. A TPM that is `unavailable` for this long is none.
const TPM_WAIT_TICKS: u64 = 60 * TICKS_PER_SECOND;

fn say(text: &str) {
	let line = format!("HibernationService: {text}\n");
	print(line.as_bytes());
}

struct Component {
	privilege: u64,
	area: u64,
	tpm: u64,
	restore: u64,
	// What became of the image found at this boot: "none", "restored" never answers, or why it was refused.
	last_image: String,
}

// A MEMORY OBJECT HOLDING `bytes`, handed over whole: what the area's `write` takes.
fn buffer_of(bytes: &[u8]) -> Option<u64> {
	unsafe {
		let object: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, bytes.len() as u64, 0, 0, 0);
		if sys_is_err(object) {
			return None;
		}
		let Some(base) = map_object(object) else {
			close(object);
			return None;
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), base as *mut u8, bytes.len());
		unmap_object(object);
		Some(object)
	}
}

// A BUFFER'S BYTES, copied out and the buffer given back.
fn bytes_of(handle: u64, len: u64) -> Option<Vec<u8>> {
	unsafe {
		let base = map_object(handle)?;
		let mut out = Vec::new();
		if out.try_reserve_exact(len as usize).is_err() {
			unmap_object(handle);
			close(handle);
			return None;
		}
		out.extend_from_slice(core::slice::from_raw_parts(base as *const u8, len as usize));
		unmap_object(handle);
		close(handle);
		Some(out)
	}
}

// THE MEMORY THIS MACHINE HAS, in bytes: every RAM region of the boot memory map.
fn memory_bytes() -> u64 {
	let mut total: u64 = 0;
	let mut index: u64 = 0;
	loop {
		let mut region = MemmapRegion { base: 0, length: 0, kind: 0, _pad: 0 };
		// SAFETY: a description, read and not acted on.
		let count = unsafe { memmap_get(index, &mut region) };
		if count < 0 || index >= count as u64 {
			break;
		}
		if matches!(region.kind, MEMMAP_USABLE | MEMMAP_BOOTLOADER | MEMMAP_KERNEL | MEMMAP_BOOTLOADER_RECLAIMABLE | MEMMAP_ACPI_RECLAIMABLE | MEMMAP_ACPI_NVS) {
			total = total.saturating_add(region.length);
		}
		index += 1;
	}
	total
}

impl Component {
	fn area(&self) -> hibernation_area::Client<ChannelTransport> {
		hibernation_area::Client::with_deadline(ChannelTransport { chan: self.area }, clock().saturating_add(CALL_TICKS))
	}

	fn tpm(&self) -> tpm::Client<ChannelTransport> {
		tpm::Client::with_deadline(ChannelTransport { chan: self.tpm }, clock().saturating_add(CALL_TICKS))
	}

	fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>, String> {
		if self.area == 0 {
			return Err(String::from("no hibernation area is served to this component"));
		}
		match self.area().read(&offset, &(length as u32)) {
			Some(Ok(buffer)) => bytes_of(buffer.handle, buffer.len).filter(|bytes| bytes.len() >= length).ok_or_else(|| String::from("the area's answer could not be read")),
			Some(Err(error)) => Err(format!("the area refused a read at {offset} ({error:?})")),
			None => Err(String::from("the area did not answer a read")),
		}
	}

	fn write(&self, offset: u64, bytes: &[u8]) -> Result<(), String> {
		let Some(buffer) = buffer_of(bytes) else { return Err(String::from("no memory for a write to the area")) };
		match self.area().write(&offset, &proto::codec::Buffer { handle: buffer, len: bytes.len() as u64 }, &(bytes.len() as u32)) {
			Some(Ok(())) => Ok(()),
			Some(Err(error)) => Err(format!("the area refused a write at {offset} ({error:?})")),
			None => Err(String::from("the area did not answer a write")),
		}
	}

	fn flush(&self) -> Result<(), String> {
		match self.area().flush() {
			Some(Ok(())) => Ok(()),
			Some(Err(error)) => Err(format!("the area's flush was refused ({error:?})")),
			None => Err(String::from("the area did not answer its flush")),
		}
	}

	// THE HEADER INVALIDATED: its magic, its version and the empty state - what no image is - and the device flushed.
	fn invalidate(&self) -> Result<(), String> {
		let mut block = [0u8; HEADER_BYTES];
		block[..8].copy_from_slice(&hibernation::MAGIC);
		block[8..12].copy_from_slice(&hibernation::VERSION.to_le_bytes());
		block[12..16].copy_from_slice(&STATE_EMPTY.to_le_bytes());
		self.write(0, &block)?;
		self.flush()
	}

	// ------------------------------------------------------------------ the status

	fn status(&self) -> HibernationStatus {
		let memory = memory_bytes();
		let (present, bytes) = match (self.area != 0).then(|| self.area().describe()).flatten() {
			Some(Ok(info)) => (info.present, info.bytes),
			_ => (false, 0),
		};
		let why = if self.privilege == 0 {
			String::from("the kernel gave no hibernation privilege")
		} else if !present {
			String::from("the system disk has no hibernation partition")
		} else if bytes < memory {
			format!("the hibernation partition holds {bytes} bytes, less than the {memory} of memory")
		} else if self.tpm == 0 {
			String::from("no TPM seals the image's key - its grant was not minted")
		} else {
			// WHETHER THE TPM SEALS, asked of TpmService: a TPM bound, and its owner hierarchy usable.
			match self.tpm().info() {
				Some(Ok(answer)) if answer.outcome == Outcome::Done && answer.info.as_ref().is_some_and(|info| info.sealing) => String::new(),
				Some(Ok(answer)) if answer.outcome == Outcome::Done => String::from("the TPM's owner hierarchy has an authorization value or is disabled, so it seals nothing"),
				Some(Ok(answer)) if answer.outcome == Outcome::Unavailable => String::from("no TPM is bound to seal the image's key"),
				Some(Ok(answer)) => format!("the TPM answered {:?} (code {:#x})", answer.outcome, answer.code),
				Some(Err(error)) => format!("TpmService refused the question ({error:?})"),
				None => String::from("TpmService did not answer"),
			}
		};
		HibernationStatus { set_up: why.is_empty(), why, partition_bytes: bytes, memory_bytes: memory, last_image: self.last_image.clone() }
	}

	// ------------------------------------------------------------------ the write

	fn write_image(&self) -> Result<(), String> {
		let mut info = SnapshotInfo { pages: 0, context: [0; SNAPSHOT_CONTEXT], system: [0; 32], hardware: [0; 32] };
		let answer = snapshot_info(self.privilege, &mut info);
		if answer != 0 {
			return Err(format!("the kernel holds no snapshot ({answer})"));
		}
		let area_bytes = match self.area().describe() {
			Some(Ok(area)) if area.present => area.bytes,
			_ => return Err(String::from("the hibernation area is gone")),
		};
		if hibernation::image_bytes(info.pages) > area_bytes {
			return Err(format!("the image of {} pages does not fit the partition's {area_bytes} bytes", info.pages));
		}
		// THE KEYS, FRESH: the key-encryption key sealed, the image key wrapped under it.
		let mut kek = [0u8; KEK];
		let mut image_bytes = [0u8; 32];
		let mut nonce = [0u8; 8];
		if random_get(&mut kek) != KEK || random_get(&mut image_bytes) != 32 || random_get(&mut nonce) != 8 {
			return Err(String::from("the kernel gave no random bytes for the keys"));
		}
		let sealed = match self.tpm().seal(&PCR, &kek) {
			Some(Ok(answer)) if answer.outcome == Outcome::Done => answer.bytes,
			Some(Ok(answer)) => return Err(format!("the TPM did not seal the key ({:?}, code {:#x})", answer.outcome, answer.code)),
			Some(Err(error)) => return Err(format!("TpmService refused the seal ({error:?})")),
			None => return Err(String::from("TpmService did not answer the seal")),
		};
		let image = ImageKey::from_bytes(&image_bytes);
		// THE CHUNKS, each read from the kernel, sealed and written behind its head.
		let chunks = info.pages.div_ceil(CHUNK_PAGES as u64);
		let mut read: Vec<u8> = alloc::vec![0u8; CHUNK_PAGES * (8 + PAGE)];
		for index in 0..chunks {
			let pages = hibernation::chunk_pages(info.pages, index);
			let first = index * CHUNK_PAGES as u64;
			let answer = snapshot_read(self.privilege, first, pages as u64, &mut read);
			if answer != 0 {
				return Err(format!("the kernel refused the snapshot's pages from {first} ({answer})"));
			}
			let frames: Vec<u64> = (0..pages).map(|at| u64::from_le_bytes(read[at * 8..at * 8 + 8].try_into().unwrap_or([0; 8]))).collect();
			let mut record: Vec<u8> = alloc::vec![0u8; CHUNK_HEAD + pages * PAGE];
			record[CHUNK_HEAD..].copy_from_slice(&read[pages * 8..pages * 8 + pages * PAGE]);
			let head = hibernation::seal_chunk(&image, index, &frames, &mut record[CHUNK_HEAD..]).map_err(|refusal| format!("chunk {index} could not be sealed ({refusal:?})"))?;
			record[..CHUNK_HEAD].copy_from_slice(&head);
			self.write(hibernation::chunk_offset(index), &record)?;
		}
		// THE HEADER LAST, and flushed: until it is on the disk, nothing here is an image.
		let header = Header::new(&kek, &sealed, nonce, &image, info.pages, clock_rtc(), info.system, info.hardware, info.context).map_err(|refusal| format!("the header could not be made ({refusal:?})"))?;
		self.write(0, &header.encode(STATE_IMAGE))?;
		self.flush()?;
		say(&format!("the image is written - {} pages in {chunks} chunks, its key sealed to PCR {PCR}", info.pages));
		Ok(())
	}

	// ------------------------------------------------------------------ the restore

	// THE IMAGE FOUND AT MOUNT, restored - or why not. A restore that happens does not return.
	fn restore(&mut self) -> Result<(), String> {
		let block = self.read(0, HEADER_BYTES)?;
		let header = match Header::decode(&block) {
			Ok(header) => header,
			Err(Refusal::NoImage) => return Err(String::from("none")),
			Err(refusal) => return Err(String::from(refusal.text())),
		};
		say(&format!("an image of {} pages is on the hibernation partition - it is checked", header.pages));
		// ITS KEY, OPENED ONLY FOR THIS COMPONENT AND ONLY WHILE PCR 4 HOLDS WHAT IT DID.
		if self.tpm == 0 {
			return Err(String::from("no TPM seal grant was minted, so its key cannot be unsealed"));
		}
		let until = clock().saturating_add(TPM_WAIT_TICKS);
		let unsealed = loop {
			let answer = self.tpm().unseal(&header.sealed);
			match &answer {
				Some(Ok(answer)) if answer.outcome == Outcome::Unavailable && clock() < until => sleep_until(clock().saturating_add(TICKS_PER_SECOND / 10)),
				_ => break answer,
			}
		};
		let kek: [u8; KEK] = match unsealed {
			Some(Ok(answer)) if answer.outcome == Outcome::Done && answer.bytes.len() == KEK => answer.bytes[..].try_into().map_err(|_| String::from("the unsealed key has another length"))?,
			Some(Ok(answer)) if answer.outcome == Outcome::PolicyRefused => return Err(String::from(Refusal::KeyNotUnsealed.text())),
			Some(Ok(answer)) => return Err(format!("its key did not unseal ({:?}, code {:#x})", answer.outcome, answer.code)),
			Some(Err(error)) => return Err(format!("TpmService refused the unseal ({error:?})")),
			None => return Err(String::from("TpmService did not answer the unseal")),
		};
		let image = header.open(&kek).map_err(|refusal| String::from(refusal.text()))?;
		let fingerprint = system_fingerprint().ok_or_else(|| String::from("the kernel gave no fingerprint"))?;
		header.fits(&fingerprint.system, &fingerprint.hardware).map_err(|refusal| String::from(refusal.text()))?;
		// EVERY FRAME, FIRST: the kernel keeps every page of the image out of the frames they go to.
		let chunks = header.chunks();
		let mut frames: Vec<u64> = Vec::new();
		frames.try_reserve_exact(header.pages as usize).map_err(|_| String::from("no memory for the image's frame list"))?;
		for index in 0..chunks {
			let head = self.read(hibernation::chunk_offset(index), CHUNK_HEAD)?;
			let pages = hibernation::chunk_pages(header.pages, index);
			for at in 0..pages {
				frames.push(u64::from_le_bytes(head[at * 8..at * 8 + 8].try_into().unwrap_or([0; 8])));
			}
		}
		let answer = restore_begin(self.privilege, &frames, &header.context);
		if answer != 0 {
			return Err(format!("the kernel refused the image's frames ({answer}) - they are not this machine's RAM, or memory cannot hold the image beside them"));
		}
		// EVERY CHUNK: authenticated, decrypted, and held to the frames the first pass read.
		for index in 0..chunks {
			let pages = hibernation::chunk_pages(header.pages, index);
			let mut record = self.read(hibernation::chunk_offset(index), CHUNK_HEAD + pages * PAGE)?;
			let (head, data) = record.split_at_mut(CHUNK_HEAD);
			let read_frames = hibernation::open_chunk(&image, index, pages, head, data).map_err(|refusal| format!("chunk {index}: {}", refusal.text()))?;
			let first = index as usize * CHUNK_PAGES;
			if read_frames[..] != frames[first..first + pages] {
				return Err(format!("chunk {index}: {}", Refusal::Modified.text()));
			}
			let answer = restore_write(self.privilege, first as u64, data);
			if answer != 0 {
				return Err(format!("the kernel refused chunk {index}'s pages ({answer})"));
			}
		}
		// AT MOST ONCE: the header invalidated before anything is stopped.
		self.invalidate()?;
		let _ = self.area().verdict(&true);
		say(&format!("the image is authenticated and in the kernel - {} pages; every binding is stopped and memory replaced", header.pages));
		match hibernation_restore::Client::with_deadline(ChannelTransport { chan: self.restore }, clock().saturating_add(2 * CALL_TICKS)).prepare_replacement() {
			Some(Ok(())) => {}
			other => say(&format!("the restore's door answered {other:?} - memory is replaced all the same")),
		}
		let answer = restore_commit(self.privilege, false);
		// ONLY WHEN IT DID NOT HAPPEN.
		Err(format!("the kernel could not replace memory ({answer})"))
	}

	// AT A START: the image found at mount restored, or refused and the boot let go on.
	fn at_start(&mut self) {
		let found = match (self.area != 0).then(|| self.area().describe()).flatten() {
			Some(Ok(info)) => info.present && info.image_at_mount,
			_ => false,
		};
		if found {
			match self.restore() {
				Ok(()) => {}
				Err(why) => {
					if why != "none" {
						say(&format!("the image is refused, and the machine boots fresh - {why}"));
					}
					if let Err(error) = self.invalidate() {
						say(&format!("the refused image could not be invalidated - {error}"));
					}
					let _ = restore_commit(self.privilege, true);
					self.last_image = if why == "none" { String::from("none") } else { format!("refused: {why}") };
				}
			}
		}
		if self.area != 0 {
			let _ = self.area().verdict(&false);
		}
		if self.restore != 0 {
			let _ = hibernation_restore::Client::with_deadline(ChannelTransport { chan: self.restore }, clock().saturating_add(CALL_TICKS)).boot_continues();
		}
	}
}

// ------------------------------------------------------------------ the control channel

struct Control<'a> {
	component: &'a mut Component,
}

impl hibernation_image::Service for Control<'_> {
	fn status(&mut self) -> Result<HibernationStatus, Error> {
		Ok(self.component.status())
	}

	fn write_image(&mut self) -> Result<(), Error> {
		let written = self.component.write_image();
		// THE SNAPSHOT GIVEN BACK, written or not.
		let _ = snapshot_release(self.component.privilege);
		match written {
			Ok(()) => Ok(()),
			Err(why) => {
				say(&format!("the image was not written - {why}"));
				Err(Error::Io)
			}
		}
	}

	fn discard_image(&mut self) -> Result<(), Error> {
		match self.component.invalidate() {
			Ok(()) => {
				say("the image is discarded - the machine runs on");
				Ok(())
			}
			Err(why) => {
				say(&format!("the image could not be discarded - {why}"));
				Err(Error::Io)
			}
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let mut component = Component { privilege: roles[0], area: roles[1], tpm: roles[2], restore: roles[3], last_image: String::from("none") };
	send_blocking(bootstrap, b"HibernationService: online", 0);
	component.at_start();
	let mut buf = alloc::vec![0u8; 4096];
	let mut reply = alloc::vec![0u8; 4096];
	loop {
		let (len, mut handles) = match recv_caps_blocking(bootstrap, &mut buf) {
			ReceivedCaps::Message { len, handles } => (len, handles),
			ReceivedCaps::Closed => exit(),
		};
		let mut reply_handles = wire::Handles::new();
		let written = hibernation_image::dispatch(&mut Control { component: &mut component }, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
		for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
			close(leftover);
		}
		if let Some(written) = written {
			let _ = try_send(bootstrap, &reply[..written], 0);
		}
	}
}
