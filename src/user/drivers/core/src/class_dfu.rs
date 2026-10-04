// The USB DFU class side of driver.xhci: a firmware-upgrade target on the bus, published as the `admin-executor`
// provider named `org.libersystem.admin-dfu` - the slot AdminService routes `firmware-download` to.
//
// NOTHING MUTATES WITHOUT A PERSON. This side prepares, revalidates and executes, and does nothing else: an image
// reaches the device only through `execute`, on AdminService's own connection, for an operation a person
// confirmed - and the operation it confirmed is the one frozen here, the image COPIED into this process when it
// was prepared and its SHA-256 taken of that copy, so nothing the requester can still write changes what goes to
// the device.
//
// THE TARGET IS THE LIVE BINDING, NOT A NAME. The descriptor names the device by where it is - its root port,
// route, interface - and by vendor and product, and carries the attachment's generation: a device unplugged and
// plugged in again is another generation, and every preparation against the old one is void. A requester's
// selector is resolved to this identity or refused; a product string is never a selector.
//
// AT MOST ONE ATTEMPT, AND ITS OUTCOME HONESTLY. The start guard admits an operation once. A download the device
// refused is `failed`; one whose end was not observed - the device went silent, or left - is `outcome-unknown`,
// never retried here and never described as rolled back, because DFU gives a host no rollback to promise.
//
// A RUNTIME DEVICE IS FOLLOWED INTO DFU MODE, and only by the operation that sent it there. Its confirmed
// execution sends DFU_DETACH; the device then leaves the bus by itself or waits for the reset this controller
// gives its port, and comes back as another attachment. The operation - its image, the executor's state and the
// answer AdminService is waiting for - is handed to the controller as the device leaves, the publication held with
// it, and adopted by a device that comes back in DFU mode where the runtime device was, with its serial number
// (`dfu::same_device`). That device downloads the image and answers; one that does not come back in time is
// answered `failed` - nothing of the image was sent - and the publication is withdrawn.
//
// AN UPLOAD - `firmware-upload`, a backup asked for and confirmed like a download - reads the firmware OUT of a target
// that declares `bitCanUpload`, followed into DFU mode the same way. Its blocks come back until one is short; a device
// that sends past the bound the person confirmed is told to abort and nothing it sent is kept; one that stops
// answering part-way leaves the upload incomplete, and nothing is kept either. A complete image is handed over
// READ-ONLY with its length and SHA-256, through `execute-read`, for the requester to write where it was named.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use proto::system::{AdminAction, AdminDescriptor, AdminImage, AdminPrepared, AdminRead, AdminResult, Error, admin_executor};
use rt::*;

use crate::classes::{self, Carry, Module};
use crate::usb_hid::Hids;
use crate::{KIND_DFU, REQ_GET_DESCRIPTOR, UsbDevice, Xhci, control_in_req, control_nodata, control_out_req, r8};
use drivers::admin_operation::{MAX_PAYLOAD, Operations, Refusal};
use drivers::common;
use drivers::dfu::{self, Binding};
use drivers::usb_class::ClassKind;

const REQUEST_BYTES: usize = 512;
// How long a preparation stays startable: two minutes, for a person to read and decide.
const LIFETIME_TICKS: u64 = 12000;
// The longest one status poll is waited out, whatever the device asks for, and how many polls one step may take.
const MOST_POLL_TICKS: u64 = 100;
const MOST_POLLS: u32 = 200;

// A generation per attachment, across every DFU device this controller binds.
static ATTACHMENTS: AtomicU64 = AtomicU64::new(0);

pub struct Dfu {
	dev: UsbDevice,
	binding: Binding,
	generation: u64,
	target: String,
	/// What the device calls itself: the serial number a device that comes back after a detach must repeat.
	serial: Option<String>,
	operations: Operations,
	buf: Vec<u8>,
	/// The download a detach is carrying into DFU mode, from the moment the detach is sent until the device leaves.
	detaching: Option<Box<Carried>>,
}

/// What a detach is carrying into DFU mode to do there.
enum Job {
	/// The confirmed image, to download.
	Download(Vec<u8>),
	/// A read of the firmware, up to the confirmed bound.
	Upload(u32),
}

/// WHAT A DETACH CARRIES INTO DFU MODE: where the runtime device was and what it called itself, the executor's state
/// with the started operation in it, what the operation does there, and the execution waiting on the answer.
pub struct Carried {
	port: u32,
	route: u32,
	serial: Option<String>,
	target: String,
	operations: Operations,
	job: Job,
	chan: u64,
	corr: u32,
	deadline: u64,
}

impl Carry for Carried {
	fn place(&self) -> (u32, u32) {
		(self.port, self.route)
	}

	fn deadline(&self) -> u64 {
		self.deadline
	}

	// NOTHING OF THE IMAGE WAS SENT, OR READ: the device took a detach and did not come back as itself, so the answer
	// is a failure and not an unknown outcome.
	fn expire(&mut self) {
		let mut line: common::Bounded<160> = common::Bounded::new();
		line.push(b"driver.xhci: ");
		line.push(self.target.as_bytes());
		match self.job {
			Job::Download(_) => {
				line.push(b" did not come back in DFU mode - its download failed before a byte was sent\n");
				print(line.as_bytes());
				answer(self.chan, self.corr, Ok(AdminResult::Failed));
			}
			Job::Upload(_) => {
				line.push(b" did not come back in DFU mode - nothing of its firmware was read\n");
				print(line.as_bytes());
				answer_read(self.chan, self.corr, Ok(AdminRead::Failed));
			}
		}
	}

	fn as_any(&mut self) -> &mut dyn core::any::Any {
		self
	}
}

// An execution's answer, sent when the download it started is over rather than when it was asked for.
fn answer(chan: u64, corr: u32, result: Result<AdminResult, Error>) {
	let mut frame = Vec::from(corr.to_le_bytes());
	let encoded = match result {
		Ok(value) => value.encode_vec().map(|bytes| (1u8, bytes)),
		Err(error) => error.encode_vec().map(|bytes| (0u8, bytes)),
	};
	let Some((tag, bytes)) = encoded else { return };
	frame.push(tag);
	frame.extend_from_slice(&bytes);
	let _ = send_blocking(chan, &frame, 0);
}

// AN EXECUTE-READ'S ANSWER, sent when the upload is over: what it read travels as the frame's one capability.
fn answer_read(chan: u64, corr: u32, result: Result<AdminRead, Error>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		wire::Sink::u32(&mut writer, corr)?;
		match &result {
			Ok(value) => {
				wire::Sink::u8(&mut writer, 1)?;
				value.write(&mut writer)
			}
			Err(error) => {
				wire::Sink::u8(&mut writer, 0)?;
				error.write(&mut writer)
			}
		}
	})();
	let (bytes, handles) = writer.into_message();
	if encoded.is_some() && send_caps_blocking(chan, &bytes, handles.as_slice()) {
		return;
	}
	for &handle in handles.as_slice() {
		close(handle);
	}
}

// The device's serial number, as a string - `None` when it has none or it does not read.
unsafe fn serial_of(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<String> {
	unsafe {
		let mut hids = Hids::new();
		let received = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, 0x0100, 0, 18)?;
		if received < 18 {
			return None;
		}
		let index = r8(dev.data_virt + 16);
		if index == 0 {
			return None;
		}
		let received = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, 0x0300 | index as u16, 0x0409, 130)?.min(130);
		let declared = (r8(dev.data_virt) as u32).min(received);
		if declared < 2 || r8(dev.data_virt + 1) != 3 {
			return None;
		}
		let units: Vec<u16> = (1..declared / 2).map(|at| u16::from_le_bytes([r8(dev.data_virt + at as u64 * 2), r8(dev.data_virt + at as u64 * 2 + 1)])).collect();
		String::from_utf16(&units).ok()
	}
}

/// Select a bound target's setting. The device comes back on failure.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Dfu, UsbDevice> {
	if !classes::select(hc, &mut dev, binding.config_value, binding.interface, binding.alternate) {
		print(b"driver.xhci: a DFU target refused its configuration or its setting\n");
		return Err(dev);
	}
	let generation = ATTACHMENTS.fetch_add(1, Ordering::Relaxed) + 1;
	let target = format!("dfu:port{}.{:x}/if{}/{:04x}:{:04x}", dev.port, dev.route, binding.interface, dev.vendor, dev.product);
	let serial = unsafe { serial_of(hc, &mut dev) };
	let mut line: common::Bounded<192> = common::Bounded::new();
	line.push(b"driver.xhci: DFU target bound - ");
	line.push(target.as_bytes());
	line.push(match binding.mode {
		dfu::Mode::Dfu => b", in DFU mode, ".as_slice(),
		dfu::Mode::Runtime if binding.will_detach() => b", in runtime mode (it leaves the bus by itself when detached), ",
		dfu::Mode::Runtime => b", in runtime mode (it waits for a reset when detached), ",
	});
	line.decimal(binding.transfer_size as u64);
	line.push(b"-byte blocks\n");
	print(line.as_bytes());
	Ok(Dfu { dev, binding, generation, target, serial, operations: Operations::new(generation), buf: alloc::vec![0u8; REQUEST_BYTES], detaching: None })
}

fn refused(refusal: Refusal) -> Error {
	match refusal {
		Refusal::Bounds => Error::Invalid,
		Refusal::NotFound => Error::NotFound,
		Refusal::Busy => Error::Again,
		Refusal::Stale | Refusal::Expired => Error::Stale,
		Refusal::Cancelled | Refusal::Started => Error::Denied,
	}
}

// The requester's bytes, copied out of its object: exactly `length` of them, from an object at least that long, and
// at most the bound.
fn copy_payload(payload: u64, length: u32) -> Option<Vec<u8>> {
	let length = length as usize;
	if length == 0 || length > MAX_PAYLOAD || object_info(payload).is_none_or(|info| (info.size as usize) < length) {
		return None;
	}
	let addr = unsafe { map_object(payload) }?;
	let copied = unsafe { core::slice::from_raw_parts(addr as *const u8, length) }.to_vec();
	unmap_object(payload);
	Some(copied)
}

impl Dfu {
	// A RUNTIME TARGET'S CONFIRMED EXECUTION: the start guard, the image or the bound, the detach - and the answer left
	// for the device the detach turns this one into. `reads` for an `execute-read`, whose operation must be an upload,
	// as an `execute`'s must be a download.
	#[allow(clippy::too_many_arguments)]
	fn detach(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64, corr: u32, operation: u64, epoch: u64, reads: bool) {
		let refuse = |chan: u64, corr: u32, error: Error| if reads { answer_read(chan, corr, Err(error)) } else { answer(chan, corr, Err(error)) };
		let started = match self.operations.start(operation, epoch, self.generation, clock()) {
			Ok(started) => started,
			Err(refusal) => return refuse(chan, corr, refused(refusal)),
		};
		let job = match (reads, upload_of(started)) {
			(true, Some(bound)) => Job::Upload(bound),
			(false, None) => match dfu::image(&started.payload, self.dev.vendor, self.dev.product) {
				Ok(image) => Job::Download(image.to_vec()),
				Err(_) => return refuse(chan, corr, Error::Invalid),
			},
			_ => return refuse(chan, corr, Error::Invalid),
		};
		// THE DEVICE IS TOLD HOW LONG TO WAIT FOR ITS RESET: its own timeout, and no more than a second of it.
		if !self.request(hc, hids, dfu::REQ_DETACH, self.binding.detach_timeout_ms.min(1000)) {
			return if reads { answer_read(chan, corr, Ok(AdminRead::Failed)) } else { answer(chan, corr, Ok(AdminResult::Failed)) };
		}
		let deadline = clock() + self.binding.return_window_ms().div_ceil(10);
		let operations = core::mem::replace(&mut self.operations, Operations::new(self.generation));
		let what: &[u8] = if matches!(job, Job::Upload(_)) { b" was detached - its confirmed upload follows it into DFU mode\n" } else { b" was detached - its confirmed download follows it into DFU mode\n" };
		self.detaching = Some(Box::new(Carried { port: self.dev.port, route: self.dev.route, serial: self.serial.clone(), target: self.target.clone(), operations, job, chan, corr, deadline }));
		// A DEVICE THAT WAITS FOR ITS RESET GETS ONE: the controller takes this port's devices down and enumerates it
		// again, which resets it - and it comes back in DFU mode.
		if !self.binding.will_detach() {
			hc.redo_port = Some(self.dev.port);
		}
		let mut line: common::Bounded<160> = common::Bounded::new();
		line.push(b"driver.xhci: ");
		line.push(self.target.as_bytes());
		line.push(what);
		print(line.as_bytes());
	}

	// A selector as this target: its exact identity, or its vendor and product - never a display name.
	fn resolves(&self, selector: &str) -> bool {
		selector == self.target || selector == format!("dfu:{:04x}:{:04x}", self.dev.vendor, self.dev.product)
	}

	fn get_status(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Option<dfu::Status> {
		let received = control_in_req(hc, hids, &mut self.dev, dfu::RT_CLASS_INTERFACE_IN, dfu::REQ_GETSTATUS, 0, self.binding.interface as u16, 6)?;
		let bytes: Vec<u8> = (0..received.min(6) as u64).map(|at| unsafe { r8(self.dev.data_virt + at) }).collect();
		dfu::status(&bytes)
	}

	fn request(&mut self, hc: &mut Xhci, hids: &mut Hids, request: u8, value: u16) -> bool {
		control_nodata(hc, hids, &mut self.dev, dfu::RT_CLASS_INTERFACE_OUT, request, value, self.binding.interface as u16).is_some()
	}

	// Poll until the device leaves the states `busy` names, waiting what it asks between polls: its final status, or
	// `None` when it stopped answering.
	fn settle(&mut self, hc: &mut Xhci, hids: &mut Hids, busy: &[u8]) -> Option<dfu::Status> {
		for _ in 0..MOST_POLLS {
			let status = self.get_status(hc, hids)?;
			if status.status != dfu::STATUS_OK || !busy.contains(&status.state) {
				return Some(status);
			}
			sleep_until(clock() + (status.poll_timeout_ms as u64).div_ceil(10).clamp(1, MOST_POLL_TICKS));
		}
		None
	}

	// THE DOWNLOAD. Idle first, then every block with the device's own status after it, then the empty block that
	// starts manifestation, and its status to the end.
	fn download(&mut self, hc: &mut Xhci, hids: &mut Hids, image: &[u8]) -> AdminResult {
		let Some(status) = self.get_status(hc, hids) else { return AdminResult::Failed };
		if status.state == dfu::STATE_ERROR {
			self.request(hc, hids, dfu::REQ_CLRSTATUS, 0);
		} else if status.state != dfu::STATE_IDLE {
			self.request(hc, hids, dfu::REQ_ABORT, 0);
		}
		if self.get_status(hc, hids).map(|status| status.state) != Some(dfu::STATE_IDLE) {
			return AdminResult::Failed;
		}
		let block_size = self.binding.transfer_size as usize;
		let mut blocks: u16 = 0;
		for chunk in image.chunks(block_size) {
			unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), self.dev.data_virt as *mut u8, chunk.len()) };
			// FROM HERE ON SOMETHING MAY HAVE BEEN WRITTEN, so an end that is not seen is not a failure.
			if control_out_req(hc, hids, &mut self.dev, dfu::RT_CLASS_INTERFACE_OUT, dfu::REQ_DNLOAD, blocks, self.binding.interface as u16, chunk.len() as u16).is_none() {
				return AdminResult::OutcomeUnknown;
			}
			match self.settle(hc, hids, &[dfu::STATE_DNLOAD_SYNC, dfu::STATE_DNBUSY]) {
				Some(status) if status.status == dfu::STATUS_OK && status.state == dfu::STATE_DNLOAD_IDLE => {}
				Some(_) => {
					self.request(hc, hids, dfu::REQ_CLRSTATUS, 0);
					return AdminResult::Failed;
				}
				None => return AdminResult::OutcomeUnknown,
			}
			blocks = blocks.wrapping_add(1);
		}
		if !self.request(hc, hids, dfu::REQ_DNLOAD, blocks) {
			return AdminResult::OutcomeUnknown;
		}
		match self.settle(hc, hids, &[dfu::STATE_MANIFEST_SYNC, dfu::STATE_MANIFEST]) {
			// Tolerant devices come back to idle; the others wait for the reset that runs the new image, and either way
			// the image is in.
			Some(status) if status.status == dfu::STATUS_OK && matches!(status.state, dfu::STATE_IDLE | dfu::STATE_MANIFEST_WAIT_RESET) => AdminResult::Completed,
			Some(_) => {
				self.request(hc, hids, dfu::REQ_CLRSTATUS, 0);
				AdminResult::Failed
			}
			// A DEVICE THAT DETACHES TO MANIFEST stops answering, and its end is not observed.
			None => AdminResult::OutcomeUnknown,
		}
	}
}

impl Dfu {
	// THE UPLOAD. Idle first, then block after block until a short one ends it - past `bound`, the device is told to
	// abort and nothing is kept - and the image handed over read-only, with its SHA-256.
	fn upload(&mut self, hc: &mut Xhci, hids: &mut Hids, bound: u32) -> AdminRead {
		// THE DEVICE IN DFU MODE SAYS WHETHER IT CAN BE READ, whatever its runtime self declared.
		if !self.binding.can_upload() {
			self.say(b" does not declare upload in DFU mode - nothing was read\n");
			return AdminRead::Failed;
		}
		let Some(status) = self.get_status(hc, hids) else { return AdminRead::Failed };
		if status.state == dfu::STATE_ERROR {
			self.request(hc, hids, dfu::REQ_CLRSTATUS, 0);
		} else if status.state != dfu::STATE_IDLE {
			self.request(hc, hids, dfu::REQ_ABORT, 0);
		}
		if self.get_status(hc, hids).map(|status| status.state) != Some(dfu::STATE_IDLE) {
			return AdminRead::Failed;
		}
		let block = self.binding.transfer_size as usize;
		let mut image: Vec<u8> = Vec::new();
		let mut number: u16 = 0;
		loop {
			let Some(received) = control_in_req(hc, hids, &mut self.dev, dfu::RT_CLASS_INTERFACE_IN, dfu::REQ_UPLOAD, number, self.binding.interface as u16, block as u16) else {
				// STOPPED ANSWERING, OR LEFT, PART-WAY: incomplete, and never retried.
				self.say(b"'s upload is incomplete - the device stopped answering part-way; nothing is handed over\n");
				return AdminRead::Failed;
			};
			let received = (received as usize).min(block);
			let step = dfu::upload_step(image.len(), received, block, bound);
			if step == dfu::Upload::PastBound {
				self.request(hc, hids, dfu::REQ_ABORT, 0);
				self.say(b" sent more than the bound confirmed - the upload is stopped and nothing is handed over\n");
				return AdminRead::Failed;
			}
			image.extend((0..received as u64).map(|at| unsafe { r8(self.dev.data_virt + at) }));
			if step == dfu::Upload::Done {
				break;
			}
			number = number.wrapping_add(1);
		}
		if image.is_empty() {
			self.say(b" sent an empty image - nothing is handed over\n");
			return AdminRead::Failed;
		}
		let Some(handed) = read_only_copy(&image) else { return AdminRead::Failed };
		let digest = bootproto::sha256::digest(&image);
		let mut line: common::Bounded<192> = common::Bounded::new();
		line.push(b"driver.xhci: a DFU upload read ");
		line.decimal(image.len() as u64);
		line.push(b" bytes in ");
		line.decimal(u64::from(number) + 1);
		line.push(b" block(s) from ");
		line.push(self.target.as_bytes());
		line.push(b"\n");
		print(line.as_bytes());
		AdminRead::Completed(AdminImage { image: handed, length: image.len() as u32, digest: digest.to_vec() })
	}

	fn say(&self, what: &[u8]) {
		let mut line: common::Bounded<192> = common::Bounded::new();
		line.push(b"driver.xhci: ");
		line.push(self.target.as_bytes());
		line.push(what);
		print(line.as_bytes());
	}
}

// THE IMAGE IN AN OBJECT OF ITS OWN, and a handle to it that can read and map it and do nothing else.
fn read_only_copy(image: &[u8]) -> Option<u64> {
	let object = memory_object_create(image.len() as u64);
	if object < 0 {
		return None;
	}
	let object = object as u64;
	let Some(addr) = (unsafe { map_object(object) }) else {
		close(object);
		return None;
	};
	unsafe { core::ptr::copy_nonoverlapping(image.as_ptr(), addr as *mut u8, image.len()) };
	unmap_object(object);
	let handed = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
	close(object);
	(handed > 0).then_some(handed as u64)
}

// AN UPLOAD'S BOUND, from its operation's parameters - a download has none, which is how the two are told apart once
// started; the preparation admitted each by its action.
fn upload_of(prepared: &drivers::admin_operation::Prepared) -> Option<u32> {
	dfu::upload_parameters(&prepared.parameters).map(|(bound, _)| bound)
}

struct View<'a> {
	dfu: &'a mut Dfu,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl admin_executor::Service for View<'_> {
	// THE PAYLOAD IS COPIED HERE AND ONLY HERE, from the object's own size and never past it.
	fn prepare(&mut self, action: AdminAction, target: String, parameters: Vec<u8>, payload_length: u32, payload: u64) -> Result<AdminPrepared, Error> {
		let copied = copy_payload(payload, payload_length);
		close(payload);
		if action == AdminAction::FirmwareUpload {
			return self.prepare_upload(target, parameters, payload_length, copied);
		}
		if action != AdminAction::FirmwareDownload {
			return Err(Error::Unsupported);
		}
		if !self.dfu.resolves(&target) {
			return Err(Error::NotFound);
		}
		if !self.dfu.binding.can_download() {
			return Err(Error::Unsupported);
		}
		// A PLAIN DOWNLOAD HAS NO PARAMETERS, and one that carried some would carry a meaning nobody confirmed.
		if !parameters.is_empty() {
			return Err(Error::Invalid);
		}
		let copied = copied.ok_or(Error::Invalid)?;
		// THE SUFFIX, WHERE THE IMAGE HAS ONE, is held against this target before anything is frozen.
		dfu::image(&copied, self.dfu.dev.vendor, self.dfu.dev.product).map_err(|_| Error::Invalid)?;
		let generation = self.dfu.generation;
		let epoch = self.dfu.operations.epoch;
		let prepared = self.dfu.operations.prepare(generation, &parameters, &copied, clock(), LIFETIME_TICKS).map_err(refused)?;
		let blocks = (prepared.payload.len() / self.dfu.binding.transfer_size as usize + 2) as u32;
		let descriptor = AdminDescriptor { version: 1, action, executor: String::from_utf8_lossy(driver_protocol::provider::USB_DFU_NAME).into_owned(), executor_epoch: epoch, target: self.dfu.target.clone(), target_generation: prepared.generation, parameters: prepared.parameters.clone(), payload_length, payload_digest: prepared.digest.to_vec() };
		Ok(AdminPrepared { operation: prepared.operation, descriptor, deadline_ms: 5000 + 2000 * blocks })
	}

	fn revalidate(&mut self, operation: u64) -> Result<(), Error> {
		self.dfu.operations.revalidate(operation, self.dfu.generation, clock()).map_err(refused)
	}

	fn execute(&mut self, operation: u64, epoch: u64) -> Result<AdminResult, Error> {
		let generation = self.dfu.generation;
		let started = self.dfu.operations.start(operation, epoch, generation, clock()).map_err(refused)?;
		// AN UPLOAD IS REDEEMED WITH `execute-read`, and nothing is written for one.
		if upload_of(started).is_some() {
			return Err(Error::Invalid);
		}
		let payload = started.payload.clone();
		let image = dfu::image(&payload, self.dfu.dev.vendor, self.dfu.dev.product).map_err(|_| Error::Invalid)?.to_vec();
		let result = self.dfu.download(self.hc, self.hids, &image);
		print(match result {
			AdminResult::Completed => b"driver.xhci: a DFU download completed and the device manifested it\n".as_slice(),
			AdminResult::Failed => b"driver.xhci: a DFU download failed - the device refused it\n",
			AdminResult::OutcomeUnknown => b"driver.xhci: a DFU download's end was not observed - it is not retried\n",
		});
		Ok(result)
	}

	fn cancel(&mut self, operation: u64) -> Result<(), Error> {
		self.dfu.operations.cancel(operation).map_err(refused)
	}

	fn execute_read(&mut self, operation: u64, epoch: u64) -> Result<AdminRead, Error> {
		let generation = self.dfu.generation;
		let started = self.dfu.operations.start(operation, epoch, generation, clock()).map_err(refused)?;
		let bound = upload_of(started).ok_or(Error::Invalid)?;
		Ok(self.dfu.upload(self.hc, self.hids, bound))
	}
}

impl View<'_> {
	// AN UPLOAD'S PREPARATION: a target this executor serves that declares upload, a bound and a file it can show, and
	// the payload the target's name - what the protected screen shows as the operation, frozen here like a download's.
	fn prepare_upload(&mut self, target: String, parameters: Vec<u8>, payload_length: u32, copied: Option<Vec<u8>>) -> Result<AdminPrepared, Error> {
		if !self.dfu.resolves(&target) {
			return Err(Error::NotFound);
		}
		if !self.dfu.binding.can_upload() {
			return Err(Error::Unsupported);
		}
		let (bound, _) = dfu::upload_parameters(&parameters).ok_or(Error::Invalid)?;
		let copied = copied.ok_or(Error::Invalid)?;
		if copied != target.as_bytes() {
			return Err(Error::Invalid);
		}
		let generation = self.dfu.generation;
		let epoch = self.dfu.operations.epoch;
		let prepared = self.dfu.operations.prepare(generation, &parameters, &copied, clock(), LIFETIME_TICKS).map_err(refused)?;
		let blocks = bound / u32::from(self.dfu.binding.transfer_size) + 2;
		let descriptor = AdminDescriptor { version: 1, action: AdminAction::FirmwareUpload, executor: String::from_utf8_lossy(driver_protocol::provider::USB_DFU_NAME).into_owned(), executor_epoch: epoch, target: self.dfu.target.clone(), target_generation: prepared.generation, parameters: prepared.parameters.clone(), payload_length, payload_digest: prepared.digest.to_vec() };
		Ok(AdminPrepared { operation: prepared.operation, descriptor, deadline_ms: 5000 + 2000 * blocks })
	}
}

impl Module for Dfu {
	fn kind(&self) -> ClassKind {
		ClassKind::Dfu
	}

	fn inventory(&self) -> u8 {
		KIND_DFU
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::ADMIN_EXECUTOR
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_DFU_NAME
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	fn start(&mut self, _hc: &mut Xhci) {}

	fn serve(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64, _bootstrap: u64, _bind: &common::Bind) -> bool {
		let mut buf = core::mem::take(&mut self.buf);
		let polled = try_recv_caps(chan, &mut buf);
		self.buf = buf;
		let (len, mut handles) = match polled {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return true,
			PolledCaps::Closed => return false,
		};
		let request = self.buf[..len].to_vec();
		// A RUNTIME TARGET'S EXECUTION IS ANSWERED LATER, by the device the detach turns it into - so it is taken
		// before the generated dispatch, which answers at once. Two words after the header: operation and epoch.
		if self.binding.mode == dfu::Mode::Runtime
			&& let Some((op @ (admin_executor::OP_EXECUTE | admin_executor::OP_EXECUTE_READ), corr)) = classes::correlation(&request)
		{
			for &handle in handles.as_slice() {
				close(handle);
			}
			let reads = op == admin_executor::OP_EXECUTE_READ;
			if request.len() != 6 + 16 || self.detaching.is_some() {
				let error = if request.len() != 22 { Error::Invalid } else { Error::Again };
				if reads {
					answer_read(chan, corr, Err(error))
				} else {
					answer(chan, corr, Err(error))
				}
				return true;
			}
			let word = |at: usize| u64::from_le_bytes(request[at..at + 8].try_into().unwrap_or([0; 8]));
			self.detach(hc, hids, chan, corr, word(6), word(14), reads);
			return true;
		}
		let mut reply = [0u8; 1024];
		let mut reply_handles = wire::Handles::new();
		let mut view = View { dfu: self, hc, hids };
		if let Some(written) = admin_executor::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn departed(&mut self, _hc: &mut Xhci, _hids: &mut Hids, _chan: u64) {}

	fn absorb(&mut self, _hc: &mut Xhci, _hids: &mut Hids, _pointer: u64, _status: u32, _control: u32) -> bool {
		false
	}

	fn release(&mut self, _hc: &mut Xhci) {}

	fn handoff(&mut self) -> Option<Box<dyn Carry>> {
		self.detaching.take().map(|carried| carried as Box<dyn Carry>)
	}

	// A DEVICE THAT CAME BACK where a detach left: adopted only if it is in DFU mode, where the runtime device was,
	// with its serial number. It takes the executor's state over - the epoch AdminService confirmed against goes on
	// - downloads the image the operation froze, and answers the execution that has been waiting.
	fn adopt(&mut self, hc: &mut Xhci, hids: &mut Hids, carried: &mut dyn Carry) -> bool {
		let Some(carried) = carried.as_any().downcast_mut::<Carried>() else { return false };
		let was = dfu::Identity { port: carried.port, route: carried.route, serial: carried.serial.as_deref() };
		let now = dfu::Identity { port: self.dev.port, route: self.dev.route, serial: self.serial.as_deref() };
		if self.binding.mode != dfu::Mode::Dfu || !dfu::same_device(&was, &now) {
			return false;
		}
		// AN UPLOAD CARRIED HERE IS READ, and answered with what it read.
		if let Job::Upload(bound) = carried.job {
			self.operations = core::mem::replace(&mut carried.operations, Operations::new(0));
			let result = self.upload(hc, hids, bound);
			let mut line: common::Bounded<192> = common::Bounded::new();
			line.push(b"driver.xhci: ");
			line.push(carried.target.as_bytes());
			line.push(b" came back in DFU mode as ");
			line.push(self.target.as_bytes());
			line.push(if matches!(result, AdminRead::Completed(_)) { b" and its firmware was read\n".as_slice() } else { b" and its firmware was not read\n" });
			print(line.as_bytes());
			answer_read(carried.chan, carried.corr, Ok(result));
			return true;
		}
		if !self.binding.can_download() {
			return false;
		}
		self.operations = core::mem::replace(&mut carried.operations, Operations::new(0));
		let Job::Download(image) = core::mem::replace(&mut carried.job, Job::Download(Vec::new())) else { return false };
		let result = self.download(hc, hids, &image);
		let mut line: common::Bounded<192> = common::Bounded::new();
		line.push(b"driver.xhci: ");
		line.push(carried.target.as_bytes());
		line.push(b" came back in DFU mode as ");
		line.push(self.target.as_bytes());
		line.push(match result {
			AdminResult::Completed => b" and manifested the image\n".as_slice(),
			AdminResult::Failed => b" and refused the image\n",
			AdminResult::OutcomeUnknown => b" and its download's end was not observed\n",
		});
		print(line.as_bytes());
		answer(carried.chan, carried.corr, Ok(result));
		true
	}
}
