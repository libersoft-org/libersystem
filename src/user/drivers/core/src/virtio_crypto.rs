// driver.virtio-crypto - the host's symmetric cipher engine (virtio device 20), served as the `crypto-device` contract.
//
// AN OFFLOAD A CONSUMER MAY CHOOSE, AND NOTHING THE SYSTEM RELIES ON. A virtual machine's engine is the host's: it sees
// every key and every byte that passes, so nothing here is safer than the software the system already has, which stays
// the correctness baseline. This driver makes the engine reachable through a typed contract and holds it to that.
//
// ONLY WHAT THE CONTRACT NAMES: AES-CBC, keys of 16, 24 or 32 bytes, one IV with every request, at most one page of
// data. The device may offer more; the rest is left unnamed rather than half served.
//
// A KEY CROSSES ONCE. `open` hands it to the device in the control queue's request; the driver zeroes its own copies -
// the request it decoded, the message it received and the page the device read - as soon as the device has answered,
// and no operation returns it. The plaintext and ciphertext pages are zeroed after every request as well.
//
// A SESSION IS ITS CONNECTION'S. Each connection holds at most eight, numbered from one and never renumbered while it
// lives; another connection naming one of them is refused, and every session a connection held is destroyed at the
// device when it goes. A sleep that cuts the power ends every session the device held, so a consumer opens again.
//
// SYNCHRONOUS, ONE REQUEST AT A TIME: a request is submitted and waited for before the next message is read, so nothing
// is in flight between two of them - which is what makes the stop and the sleep simple.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use drivers::crypto::{self, Config, Refusal, Session, Sessions};
use drivers::virtio::{Queue, Virtio};
use proto::generated::liber::crypto_device::v1::{Cipher, CryptoLimits, CryptoOutput, Direction, crypto_device};
use proto::system::Error;
use rt::*;

// The control page's layout: the request, the key the device reads, what it writes back.
const CONTROL_REQUEST: u64 = 0;
const CONTROL_KEY: u64 = 128;
const CONTROL_INPUT: u64 = 256;
const CONTROL_STATUS: u64 = 512;

// The data header page's: the request, the IV, the status byte.
const DATA_REQUEST: u64 = 0;
const DATA_IV: u64 = 128;
const DATA_STATUS: u64 = 256;

// The one data queue this driver uses.
const DATA_QUEUE: u16 = 0;

const PAGE: u64 = 4096;

// One DMA page: its address here and the device's.
#[derive(Clone, Copy)]
struct Page {
	virt: u64,
	phys: u64,
}

impl Page {
	fn write(&self, at: u64, bytes: &[u8]) {
		// SAFETY: this driver's own DMA page, and every offset written lies inside it.
		unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), (self.virt + at) as *mut u8, bytes.len()) };
	}

	fn read(&self, at: u64, len: usize) -> &[u8] {
		// SAFETY: as above; the device has given the chain back before this is read.
		unsafe { core::slice::from_raw_parts((self.virt + at) as *const u8, len) }
	}

	fn byte(&self, at: u64) -> u8 {
		// SAFETY: as above.
		unsafe { ((self.virt + at) as *const u8).read_volatile() }
	}

	fn set_byte(&self, at: u64, value: u8) {
		// SAFETY: as above.
		unsafe { ((self.virt + at) as *mut u8).write_volatile(value) };
	}

	fn zero(&self, at: u64, len: usize) {
		// SAFETY: as above.
		crypto::zeroize(unsafe { core::slice::from_raw_parts_mut((self.virt + at) as *mut u8, len) });
	}
}

// THE ENGINE: its two queues, its four pages and what its configuration offers.
struct Engine {
	data: Queue,
	control: Queue,
	config: Config,
	control_page: Page,
	header_page: Page,
	source: Page,
	destination: Page,
}

impl Engine {
	// ONE SESSION CREATED at the device, with the key zeroed from the page once the device has answered.
	fn create(&mut self, algo: u32, encrypt: bool, key: &[u8]) -> Result<u64, Error> {
		let request = crypto::create_session(algo, key.len() as u32, if encrypt { crypto::OP_ENCRYPT } else { crypto::OP_DECRYPT }, u32::from(DATA_QUEUE));
		self.control_page.write(CONTROL_REQUEST, &request);
		self.control_page.write(CONTROL_KEY, key);
		self.control_page.zero(CONTROL_INPUT, crypto::SESSION_INPUT_LEN);
		let phys = self.control_page.phys;
		let chain: [(u64, u32, bool); 3] = [
			(phys + CONTROL_REQUEST, crypto::REQUEST_LEN as u32, false),
			(phys + CONTROL_KEY, key.len() as u32, false),
			(phys + CONTROL_INPUT, crypto::SESSION_INPUT_LEN as u32, true),
		];
		let mut used = [0u32; 1];
		let submitted = self.control.submit_chains(&[&chain], None, &mut used);
		self.control_page.zero(CONTROL_KEY, crypto::MAX_KEY);
		submitted.map_err(|_| Error::Io)?;
		let (id, status) = crypto::session_input(self.control_page.read(CONTROL_INPUT, crypto::SESSION_INPUT_LEN)).ok_or(Error::Io)?;
		match status {
			crypto::STATUS_OK => Ok(id),
			other => Err(device_error(other)),
		}
	}

	// ONE SESSION DESTROYED at the device.
	fn destroy(&mut self, session: Session) -> Result<(), Error> {
		let request = crypto::destroy_session(session.algo, session.device_id, u32::from(DATA_QUEUE));
		self.control_page.write(CONTROL_REQUEST, &request);
		self.control_page.set_byte(CONTROL_STATUS, 0xFF);
		let phys = self.control_page.phys;
		let chain: [(u64, u32, bool); 2] = [(phys + CONTROL_REQUEST, crypto::REQUEST_LEN as u32, false), (phys + CONTROL_STATUS, 1, true)];
		let mut used = [0u32; 1];
		self.control.submit_chains(&[&chain], None, &mut used).map_err(|_| Error::Io)?;
		match u32::from(self.control_page.byte(CONTROL_STATUS)) {
			crypto::STATUS_OK => Ok(()),
			other => Err(device_error(other)),
		}
	}

	// ONE REQUEST on a session: the IV and the source out, the destination and the status back, both pages zeroed after.
	fn run(&mut self, session: Session, iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
		let opcode = if session.encrypt { crypto::CIPHER_ENCRYPT } else { crypto::CIPHER_DECRYPT };
		let len = data.len() as u32;
		self.header_page.write(DATA_REQUEST, &crypto::cipher_request(opcode, session.algo, session.device_id, iv.len() as u32, len, len));
		self.header_page.write(DATA_IV, iv);
		self.header_page.set_byte(DATA_STATUS, 0xFF);
		self.source.write(0, data);
		let header = self.header_page.phys;
		let chain: [(u64, u32, bool); 5] = [
			(header + DATA_REQUEST, crypto::REQUEST_LEN as u32, false),
			(header + DATA_IV, iv.len() as u32, false),
			(self.source.phys, len, false),
			(self.destination.phys, len, true),
			(header + DATA_STATUS, 1, true),
		];
		let mut used = [0u32; 1];
		let submitted = self.data.submit_chains(&[&chain], None, &mut used);
		let status = u32::from(self.header_page.byte(DATA_STATUS));
		let mut out = Vec::new();
		let result = match submitted {
			Err(_) => Err(Error::Io),
			Ok(()) if status != crypto::STATUS_OK => Err(device_error(status)),
			Ok(()) => match out.try_reserve_exact(data.len()) {
				Ok(()) => {
					out.extend_from_slice(self.destination.read(0, data.len()));
					Ok(())
				}
				Err(_) => Err(Error::Exhausted),
			},
		};
		self.source.zero(0, data.len());
		self.destination.zero(0, data.len());
		result.map(|()| out)
	}
}

fn device_error(status: u32) -> Error {
	match status {
		crypto::STATUS_NOTSUPP => Error::Unsupported,
		crypto::STATUS_INVSESS => Error::NotFound,
		crypto::STATUS_NOSPC => Error::Exhausted,
		crypto::STATUS_BADMSG => Error::Invalid,
		_ => Error::Io,
	}
}

fn refusal(refusal: Refusal) -> Error {
	match refusal {
		Refusal::Unoffered | Refusal::TooLong => Error::Unsupported,
		Refusal::Key | Refusal::Iv | Refusal::Data => Error::Invalid,
	}
}

fn algo_of(cipher: Cipher) -> u32 {
	match cipher {
		Cipher::AesCbc => crypto::AES_CBC,
	}
}

// ONE CONNECTION'S VIEW: the engine, and the sessions this connection holds.
struct Connection<'a> {
	engine: &'a mut Engine,
	sessions: &'a mut Sessions,
}

impl crypto_device::Service for Connection<'_> {
	fn limits(&mut self) -> Result<CryptoLimits, Error> {
		let mut ciphers = Vec::new();
		for (cipher, algo) in [(Cipher::AesCbc, crypto::AES_CBC)] {
			if self.engine.config.offers(algo) {
				ciphers.try_reserve(1).map_err(|_| Error::Exhausted)?;
				ciphers.push(cipher);
			}
		}
		Ok(CryptoLimits { ciphers, max_key_len: self.engine.config.max_key() as u32, max_request: self.engine.config.max_request() as u32, max_sessions: crypto::MAX_SESSIONS as u32 })
	}

	fn open(&mut self, cipher: Cipher, direction: Direction, mut key: Vec<u8>) -> Result<u32, Error> {
		let algo = algo_of(cipher);
		let outcome = (|| {
			if !self.engine.config.offers(algo) {
				return Err(refusal(Refusal::Unoffered));
			}
			crypto::check_key(key.len(), &self.engine.config).map_err(refusal)?;
			if !self.sessions.has_room() {
				return Err(Error::Exhausted);
			}
			let encrypt = matches!(direction, Direction::Encrypt);
			let device_id = self.engine.create(algo, encrypt, &key)?;
			match self.sessions.insert(Session { device_id, algo, encrypt }) {
				Some(number) => Ok(number),
				None => {
					// Room was there a moment ago and is not now only if the numbers ran out: the device's session goes.
					let _ = self.engine.destroy(Session { device_id, algo, encrypt });
					Err(Error::Exhausted)
				}
			}
		})();
		crypto::zeroize(&mut key);
		outcome
	}

	fn apply(&mut self, session: u32, iv: Vec<u8>, data: Vec<u8>) -> Result<CryptoOutput, Error> {
		let held = self.sessions.get(session).ok_or(Error::NotFound)?;
		crypto::check_request(iv.len(), data.len(), &self.engine.config).map_err(refusal)?;
		self.engine.run(held, &iv, &data).map(|data| CryptoOutput { data })
	}

	fn close(&mut self, session: u32) -> Result<(), Error> {
		let held = self.sessions.remove(session).ok_or(Error::NotFound)?;
		self.engine.destroy(held)
	}
}

// THE SESSIONS OF EVERY CONNECTION, by the connection's channel: the serving table reorders its entries when one
// closes, so its index is no name for a connection.
struct Clients {
	entries: [(u64, Sessions); common::MAX_PROVIDER_CLIENTS],
}

impl Clients {
	fn new() -> Clients {
		Clients { entries: [const { (0, Sessions::new()) }; common::MAX_PROVIDER_CLIENTS] }
	}

	fn of(&mut self, channel: u64) -> Option<&mut Sessions> {
		if let Some(at) = self.entries.iter().position(|(held, _)| *held == channel) {
			return Some(&mut self.entries[at].1);
		}
		let at = self.entries.iter().position(|(held, _)| *held == 0)?;
		self.entries[at] = (channel, Sessions::new());
		Some(&mut self.entries[at].1)
	}

	// A connection that has gone: its sessions handed back for destruction.
	fn take(&mut self, channel: u64) -> Option<Sessions> {
		let at = self.entries.iter().position(|(held, _)| *held == channel)?;
		Some(core::mem::replace(&mut self.entries[at], (0, Sessions::new())).1)
	}

	// THE DRIVER ENDING, stopped or not: every connection's sessions destroyed at the device, as a connection that goes
	// has its own destroyed. A reset is not known to close a backend's sessions - and their keys - and a driver started
	// after this one has no ids to destroy them with.
	fn destroy_all(&mut self, engine: &mut Engine) {
		for (_, sessions) in self.entries.iter_mut() {
			for session in sessions.drain() {
				let _ = engine.destroy(session);
			}
		}
	}

	// After a sleep that cut the power: the device holds no session, so no connection does either.
	fn forget_all(&mut self) -> usize {
		let mut forgotten = 0;
		for (_, sessions) in self.entries.iter_mut() {
			forgotten += sessions.drain().count();
		}
		forgotten
	}
}

// Serve one request on connection `index`; false when the consumer has gone.
fn serve(engine: &mut Engine, clients: &mut Clients, serving: &common::Serving, index: usize, buf: &mut [u8], reply: &mut [u8]) -> bool {
	let channel = serving.at(index);
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let Some(sessions) = clients.of(channel) else {
		crypto::zeroize(&mut buf[..len]);
		return false;
	};
	let mut reply_handles = wire::Handles::new();
	let mut view = Connection { engine, sessions };
	let written = crypto_device::dispatch(&mut view, &buf[..len], &mut handles, reply, &mut reply_handles);
	// THE MESSAGE AN `open` CAME IN still holds its key: zeroed whatever the request was.
	crypto::zeroize(&mut buf[..len]);
	if let Some(written) = written {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
		crypto::zeroize(&mut reply[..written]);
	}
	true
}

// A consumer has gone: every session it held destroyed at the device, and the manager told.
fn part(engine: &mut Engine, clients: &mut Clients, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, index: usize) -> bool {
	let channel = serving.at(index);
	if let Some(mut sessions) = clients.take(channel) {
		for session in sessions.drain() {
			let _ = engine.destroy(session);
		}
	}
	let token = serving.close_at(index);
	common::disconnected(bootstrap, bind, token)
}

struct Sleep<'a> {
	device: &'a Virtio,
	engine: &'a mut Engine,
	clients: &'a mut Clients,
	serving: &'a mut common::Serving,
	stopped: bool,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		match self.device.sleep(request) {
			Some(stopped) => {
				self.stopped = stopped;
				driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
			}
			None => driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 },
		}
	}

	fn resume(&mut self, lost_power: bool) -> bool {
		let woke = self.device.wake(&mut [&mut self.engine.data, &mut self.engine.control], lost_power, core::mem::take(&mut self.stopped));
		if woke && lost_power && self.clients.forget_all() != 0 {
			print(b"driver.virtio-crypto: the sleep cut the power and the device holds no session now - each consumer opens again\n");
		}
		woke
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

fn page(device: &Virtio) -> Page {
	// SAFETY: the bound device's capability; a page the device may read and write.
	match unsafe { dma_buffer_for(device.capability, PAGE) } {
		Some((_, virt, phys)) => Page { virt, phys },
		None => exit(),
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let mut bytes = [0u8; crypto::CONFIG_LEN as usize];
		for (at, byte) in bytes.iter_mut().enumerate() {
			*byte = device.config_read(at as u64);
		}
		let Some(config) = Config::parse(&bytes) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice) };
		if !config.usable() || !config.offers(crypto::AES_CBC) {
			print(b"driver.virtio-crypto: the engine is not ready, or offers no cipher this driver names - it is not driven\n");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
		}
		let Some(data) = device.setup_queue(DATA_QUEUE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let Some(control) = device.setup_queue(config.control_queue()) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let mut engine = Engine { data, control, config, control_page: page(&device), header_page: page(&device), source: page(&device), destination: page(&device) };
		device.driver_ok();
		let Some((mine, far)) = channel() else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-crypto", &device, b"aes-cbc");
		if !common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::CRYPTO, far)]) {
			exit();
		}
		let mut serving = common::Serving::from_offers(&[(0, mine)]);
		let mut clients = Clients::new();
		let mut buf = alloc::vec![0u8; 8192];
		let mut reply = alloc::vec![0u8; 8192];
		common::takes_sleep();
		loop {
			match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], 0) {
				None => {
					clients.destroy_all(&mut engine);
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
					}
					exit();
				}
				Some(None) => {
					if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { device: &device, engine: &mut engine, clients: &mut clients, serving: &mut serving, stopped: false }) {
						clients.destroy_all(&mut engine);
						if common::stop_requested() {
							common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
						}
						exit();
					}
				}
				Some(Some(common::ProviderReady::Connected(_))) | Some(Some(common::ProviderReady::Device(_))) => {}
				Some(Some(common::ProviderReady::Consumer(index))) => {
					if !serve(&mut engine, &mut clients, &serving, index, &mut buf, &mut reply) && !part(&mut engine, &mut clients, &mut serving, bootstrap, &bind, index) {
						exit();
					}
				}
			}
		}
	}
}
