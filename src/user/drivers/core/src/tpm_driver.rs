// driver.tpm - the TPM 2.0 driver.
//
// WHAT IT BINDS. The platform device the firmware describes: the `TPM2` table's on x86_64 - its start method's
// one locality-0 page - and a `tcg,tpm-tis-mmio` node on the device-tree ports, cut to the same page. Its one
// resource is that page, mapped here; no interrupt (both transports poll) and no DMA (a TPM masters nothing).
// Which interface the page is, it says itself: the interface-identifier register reads CRB or FIFO.
//
// WHAT IT DOES AT EVERY START, in order: Startup(CLEAR) where the firmware left that to the OS, the self-test
// once, the leftovers a killed predecessor left loaded flushed - it is the TPM's one owner, so whatever is loaded
// when it starts belongs to no one - the owner hierarchy's state and the TPM's identity read, one line saying all
// of it, and only then ONE `tpm` provider published. A failure before the publication is a failed bind.
//
// WHAT IT SERVES. `tpm-device` to TpmService alone, ONE OPERATION AT A TIME, WHOLE, in the order received - this
// process is single-threaded and runs each to its end before it reads the next request, because an operation
// interleaved between another's `CreatePrimary` and its `FlushContext` would exhaust the objects the first one
// needs. It decides nothing about who may do what; the bounds are `src/tpm`'s own, and nothing it sends is a
// command a caller chose.
//
// THE SLEEP. Before a sleep that cuts the TPM's power it sends Shutdown(STATE), so the firmware's Startup(STATE) at
// the wake restores PCRs 0 to 15 - without it that Startup fails, the firmware falls back to Startup(CLEAR) and extends
// an error separator into PCRs 0 to 7, and every secret sealed to them stays shut until the next boot; a TPM that does
// not take it refuses the sleep. The resume sends no Startup when the firmware has already started the TPM - which is
// asked, not assumed - and Startup(STATE) where it has not. A suspend to idle keeps the TPM powered and sends nothing.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use proto::system::{BytesAnswer, DeviceDescription, Error, InterfaceKind, Outcome, Quote, QuoteAnswer, Status, tpm_device};
use rt::*;
use tpm::command::Transport;
use tpm::crb::Crb;
use tpm::fifo::{self, Fifo};
use tpm::ops::{Sealed, Tpm};

// The locality-0 page: all of the TPM this driver is given.
const PAGE: usize = 0x1000;

// THE TPM'S REGISTERS, in this process's mapping of the page. Volatile, because they are the TPM's and not
// memory; the clock is the monotonic nanosecond one, in milliseconds.
struct Window {
	base: u64,
}

impl tpm::Registers for Window {
	fn read8(&mut self, offset: usize) -> u8 {
		// SAFETY: `offset` is a register inside the one page this process mapped at `base`, which is the TPM's.
		unsafe { core::ptr::read_volatile((self.base + (offset % PAGE) as u64) as *const u8) }
	}

	fn write8(&mut self, offset: usize, value: u8) {
		// SAFETY: as `read8`.
		unsafe { core::ptr::write_volatile((self.base + (offset % PAGE) as u64) as *mut u8, value) }
	}

	fn read32(&mut self, offset: usize) -> u32 {
		// SAFETY: as `read8`; every 32-bit register is at a multiple of four.
		unsafe { core::ptr::read_volatile((self.base + (offset % PAGE) as u64) as *const u32) }
	}

	fn write32(&mut self, offset: usize, value: u32) {
		// SAFETY: as `read32`.
		unsafe { core::ptr::write_volatile((self.base + (offset % PAGE) as u64) as *mut u32, value) }
	}

	fn now_ms(&mut self) -> u64 {
		clock_ns() / 1_000_000
	}

	fn pause(&mut self) {
		yield_now();
	}
}

// Whichever interface the page is.
enum Interface {
	Fifo(Fifo<Window>),
	Crb(Crb<Window>),
}

impl Transport for Interface {
	fn execute(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), tpm::TransportError> {
		match self {
			Interface::Fifo(fifo) => fifo.execute(command, response, duration_ms),
			Interface::Crb(crb) => crb.execute(command, response, duration_ms),
		}
	}
}

struct Driver {
	tpm: Tpm<Interface>,
	kind: InterfaceKind,
}

// What the TPM's answer was, in the contract's words: a response code is the TPM's own, a PCR the library will
// not touch at locality 0 is `pcr-not-allowed`, and an interface or an answer that failed is a `fault`.
fn outcome(error: tpm::Error) -> (Outcome, u32) {
	match error {
		tpm::Error::Tpm(code) => (Outcome::Tpm, code),
		tpm::Error::PolicyRefused => (Outcome::PolicyRefused, 0),
		tpm::Error::Locality => (Outcome::PcrNotAllowed, 0),
		tpm::Error::Bounds => (Outcome::Bounds, 0),
		tpm::Error::Transport(_) | tpm::Error::Malformed(_) => (Outcome::Fault, 0),
	}
}

fn bytes(result: Result<Vec<u8>, tpm::Error>) -> BytesAnswer {
	match result {
		Ok(bytes) => BytesAnswer { outcome: Outcome::Done, code: 0, bytes },
		Err(error) => {
			let (outcome, code) = outcome(error);
			BytesAnswer { outcome, code, bytes: Vec::new() }
		}
	}
}

// Four characters of a property word, printable or not at all.
fn text(bytes: &[u8]) -> alloc::string::String {
	bytes.iter().filter(|byte| byte.is_ascii_graphic() || **byte == b' ').map(|byte| *byte as char).collect()
}

impl tpm_device::Service for Driver {
	fn describe(&mut self) -> Result<DeviceDescription, Error> {
		let described = self.tpm.identity().and_then(|identity| self.tpm.hierarchy().map(|hierarchy| (identity, hierarchy)));
		Ok(match described {
			Ok((identity, hierarchy)) => DeviceDescription { outcome: Outcome::Done, code: 0, interface: self.kind, manufacturer: text(&identity.manufacturer), vendor: text(&identity.vendor), firmware_1: identity.firmware.0, firmware_2: identity.firmware.1, owner_auth_set: hierarchy.owner_auth_set, owner_enabled: hierarchy.owner_enabled },
			Err(error) => {
				let (outcome, code) = outcome(error);
				DeviceDescription { outcome, code, interface: self.kind, manufacturer: alloc::string::String::new(), vendor: alloc::string::String::new(), firmware_1: 0, firmware_2: 0, owner_auth_set: false, owner_enabled: false }
			}
		})
	}

	fn random(&mut self, count: u32) -> Result<BytesAnswer, Error> {
		Ok(bytes(self.tpm.random(count as usize)))
	}

	fn pcr_read(&mut self, pcr: u32) -> Result<BytesAnswer, Error> {
		Ok(bytes(self.tpm.pcr_read(pcr).map(|digest| digest.to_vec())))
	}

	fn pcr_extend(&mut self, pcr: u32, digest: Vec<u8>) -> Result<Status, Error> {
		let Ok(digest) = <[u8; tpm::SHA256_LEN]>::try_from(digest.as_slice()) else { return Ok(Status { outcome: Outcome::Bounds, code: 0 }) };
		Ok(match self.tpm.pcr_extend(pcr, &digest) {
			Ok(()) => Status { outcome: Outcome::Done, code: 0 },
			Err(error) => {
				let (outcome, code) = outcome(error);
				Status { outcome, code }
			}
		})
	}

	fn seal(&mut self, pcr: u32, secret: Vec<u8>) -> Result<BytesAnswer, Error> {
		Ok(bytes(self.tpm.seal(&secret, pcr).map(|sealed| sealed.encode())))
	}

	fn unseal(&mut self, sealed: Vec<u8>) -> Result<BytesAnswer, Error> {
		let Ok(sealed) = Sealed::decode(&sealed) else { return Ok(BytesAnswer { outcome: Outcome::Bounds, code: 0, bytes: Vec::new() }) };
		Ok(bytes(self.tpm.unseal(&sealed)))
	}

	fn quote(&mut self, pcr: u32, nonce: Vec<u8>) -> Result<QuoteAnswer, Error> {
		Ok(match self.tpm.quote(&nonce, pcr) {
			Ok(quote) => QuoteAnswer { outcome: Outcome::Done, code: 0, quote: Some(Quote { attest: quote.attest, signature_r: quote.r, signature_s: quote.s, point_x: quote.x, point_y: quote.y, pcr_digest: quote.pcr_digest }) },
			Err(error) => {
				let (outcome, code) = outcome(error);
				QuoteAnswer { outcome, code, quote: None }
			}
		})
	}
}

// A number in hexadecimal, for the start line.
fn hex(value: u32, out: &mut Vec<u8>) {
	for shift in (0..8).rev() {
		out.push(b"0123456789abcdef"[((value >> (shift * 4)) & 0xf) as usize]);
	}
}

fn decimal(value: usize, out: &mut Vec<u8>) {
	let mut digits = [0u8; 20];
	let mut at = digits.len();
	let mut rest = value;
	loop {
		at -= 1;
		digits[at] = b'0' + (rest % 10) as u8;
		rest /= 10;
		if rest == 0 {
			break;
		}
	}
	out.extend_from_slice(&digits[at..]);
}

// SAID, AND THEN A FAILED BIND: nothing is published for a TPM this driver could not bring up.
fn refuse(bootstrap: u64, bind: &common::Bind, what: &[u8], error: Option<tpm::Error>, code: driver_protocol::DriverFailureCode) -> ! {
	print(b"driver.tpm: ");
	print(what);
	if let Some(error) = error {
		let (outcome, value) = outcome(error);
		match outcome {
			Outcome::Tpm => {
				let mut line = Vec::new();
				line.extend_from_slice(b" - the TPM answered 0x");
				hex(value, &mut line);
				print(&line);
			}
			_ => print(b" - its interface failed or its answer could not be read"),
		}
	}
	print(b"\n");
	common::failed(bootstrap, bind, code)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let failure = driver_protocol::DriverFailureCode::ResourceUnusable;
	// THE ONE PAGE: the row's first range, which the claim minted as `device`.
	let Some(range) = bind.info.platform.mmio().first().copied() else { refuse(bootstrap, &bind, b"its row names no register page", None, failure) };
	if resources.device == 0 || (range.len as usize) < PAGE {
		refuse(bootstrap, &bind, b"it was not handed the TPM's register page", None, failure);
	}
	let mapped: u64 = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) };
	if sys_is_err(mapped) {
		refuse(bootstrap, &bind, b"the TPM's register page could not be mapped", None, failure);
	}
	let mut window = Window { base: mapped };
	// WHICH INTERFACE, AS THE TPM SAYS: its interface identifier reads CRB or FIFO.
	let crb = tpm::Registers::read32(&mut window, fifo::INTERFACE_ID) & 0xF == fifo::INTERFACE_CRB;
	let (interface, kind) = if crb {
		match Crb::new(window, range.base, PAGE) {
			Ok(crb) => (Interface::Crb(crb), InterfaceKind::Crb),
			Err(_) => refuse(bootstrap, &bind, b"the page is not a CRB interface it says it is", None, driver_protocol::DriverFailureCode::UnsupportedDevice),
		}
	} else {
		match Fifo::new(window) {
			Ok(fifo) => (Interface::Fifo(fifo), InterfaceKind::Fifo),
			Err(_) => refuse(bootstrap, &bind, b"the page is neither a CRB nor a FIFO interface", None, driver_protocol::DriverFailureCode::UnsupportedDevice),
		}
	};
	let device = resources.device;
	let mut driver = Driver { tpm: Tpm::new(interface), kind };
	let not_responding = driver_protocol::DriverFailureCode::DeviceNotResponding;
	if let Err(error) = driver.tpm.startup() {
		refuse(bootstrap, &bind, b"Startup(CLEAR) failed", Some(error), not_responding);
	}
	if let Err(error) = driver.tpm.self_test() {
		refuse(bootstrap, &bind, b"the self-test failed", Some(error), not_responding);
	}
	let flushed = match driver.tpm.flush_leftovers() {
		Ok(flushed) => flushed,
		Err(error) => refuse(bootstrap, &bind, b"what a predecessor left loaded could not be flushed", Some(error), not_responding),
	};
	let hierarchy = match driver.tpm.hierarchy() {
		Ok(hierarchy) => hierarchy,
		Err(error) => refuse(bootstrap, &bind, b"the owner hierarchy's state could not be read", Some(error), not_responding),
	};
	let identity = match driver.tpm.identity() {
		Ok(identity) => identity,
		Err(error) => refuse(bootstrap, &bind, b"the TPM's identity could not be read", Some(error), not_responding),
	};
	// ONE LINE: the interface, the manufacturer, the firmware version, the owner hierarchy and the leftovers.
	let mut line: Vec<u8> = Vec::new();
	line.extend_from_slice(b"driver.tpm: online - ");
	line.extend_from_slice(if crb { b"CRB" } else { b"FIFO" });
	line.extend_from_slice(b" interface, manufacturer ");
	line.extend_from_slice(text(&identity.manufacturer).trim_end().as_bytes());
	line.extend_from_slice(b", firmware ");
	hex(identity.firmware.0, &mut line);
	line.push(b'.');
	hex(identity.firmware.1, &mut line);
	line.extend_from_slice(if hierarchy.owner_usable() {
		b", owner hierarchy usable"
	} else if hierarchy.owner_auth_set {
		b", owner hierarchy has an authorization value - seal, unseal and quote unavailable"
	} else {
		b", owner hierarchy disabled - seal, unseal and quote unavailable"
	});
	line.extend_from_slice(b", ");
	decimal(flushed, &mut line);
	line.extend_from_slice(b" leftover(s) flushed");
	let Some((mine, far)) = channel() else { refuse(bootstrap, &bind, b"no channel for its provider", None, driver_protocol::DriverFailureCode::OutOfMemory) };
	if !common::online(bootstrap, &bind, &line, &[(driver_protocol::provider::TPM, far)]) {
		exit();
	}
	let mut serving = common::Serving::from_offers(&[(0, mine)]);
	let mut buf = alloc::vec![0u8; 4096];
	let mut reply = alloc::vec![0u8; 4096];
	common::takes_sleep();
	loop {
		match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], 0) {
			None => {
				// NOTHING IS IN FLIGHT WHEN THE STOP IS READ - every operation runs whole before the next request
				// is - and a TPM masters nothing, so the device is quiet by construction.
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, device, true);
				}
				exit();
			}
			// THE SLEEP, between two operations.
			Some(None) => {
				if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { driver: &mut driver, serving: &mut serving }) {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, device, true);
					}
					exit();
				}
			}
			Some(Some(common::ProviderReady::Connected(_))) | Some(Some(common::ProviderReady::Device(_))) => {}
			Some(Some(common::ProviderReady::Consumer(index))) => {
				let chan = serving.at(index);
				if !serve(&mut driver, chan, &mut buf, &mut reply) {
					let token = serving.close_at(index);
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
	}
}

// THE SLEEP - see the head of this file.
struct Sleep<'a> {
	driver: &'a mut Driver,
	serving: &'a mut common::Serving,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		if request.state.loses_power() {
			if let Err(error) = self.driver.tpm.shutdown_state() {
				let mut line: Vec<u8> = Vec::from(&b"driver.tpm: the sleep is refused - Shutdown(STATE) failed ("[..]);
				hex(outcome(error).1, &mut line);
				line.extend_from_slice(b"), and without it the wake would clear PCRs 0 to 7\n");
				print(&line);
				return driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 };
			}
			print(b"driver.tpm: suspended - Shutdown(STATE) sent\n");
		}
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, lost_power: bool) -> bool {
		if !lost_power {
			// The TPM kept its power: after an unwound Shutdown(STATE) it runs on, and nothing is sent.
			return true;
		}
		match self.driver.tpm.started() {
			Ok(true) => print(b"driver.tpm: resumed - the firmware started the TPM with its saved state; no Startup is sent\n"),
			Ok(false) => match self.driver.tpm.startup_state() {
				Ok(()) => print(b"driver.tpm: resumed - Startup(STATE) restored what Shutdown(STATE) saved\n"),
				Err(_) => {
					print(b"driver.tpm: resumed - Startup(STATE) was refused, so the TPM starts afresh (CLEAR) and its PCRs are reset\n");
					if self.driver.tpm.startup().is_err() {
						return false;
					}
				}
			},
			Err(_) => {
				print(b"driver.tpm: the TPM does not answer after the sleep\n");
				return false;
			}
		}
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

// EVERY REQUEST WAITING ON ONE CONNECTION, each run whole before the next is read. False when the connection
// closed or sent what the contract does not carry.
fn serve(driver: &mut Driver, chan: u64, buf: &mut [u8], reply: &mut [u8]) -> bool {
	loop {
		let (len, mut handles) = match try_recv_caps(chan, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return true,
			PolledCaps::Closed => return false,
		};
		let mut reply_handles = wire::Handles::new();
		let written = tpm_device::dispatch(driver, &buf[..len], &mut handles, reply, &mut reply_handles);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		let Some(written) = written else { return false };
		if !send_caps_blocking(chan, &reply[..written], reply_handles.as_slice()) {
			for &leftover in reply_handles.as_slice() {
				close(leftover);
			}
			return false;
		}
	}
}
