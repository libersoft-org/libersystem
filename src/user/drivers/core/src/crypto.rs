// VIRTIO-CRYPTO (virtio 1.2, device 20): the requests the device takes for symmetric ciphers, what its
// configuration says it offers, and the sessions one connection may hold - the parts of the driver a host can test.
//
// TWO KINDS OF QUEUE. Sessions are created and destroyed on the control queue - the last queue, numbered after
// every data queue - and requests run on a data queue. Every request is three parts at least: a 72-byte header
// the device reads, what it is about (a key, an IV and the source), and what the device writes back (the
// destination and a status). The layouts are the specification's non-revision-1 ones, which is what a device
// that does not offer `VIRTIO_CRYPTO_F_REVISION_1` takes - and this driver does not ask for it.
//
// ONLY WHAT THE CONTRACT NAMES. AES in CBC mode, keys of 16, 24 or 32 bytes, one IV per request: the device may offer
// more, and the rest is left unnamed rather than half supported.

// THE SERVICE AND THE OPERATIONS, as `VIRTIO_CRYPTO_OPCODE(service, op)` makes them.
pub const SERVICE_CIPHER: u32 = 0;
pub const CIPHER_CREATE_SESSION: u32 = (SERVICE_CIPHER << 8) | 0x02;
pub const CIPHER_DESTROY_SESSION: u32 = (SERVICE_CIPHER << 8) | 0x03;
pub const CIPHER_ENCRYPT: u32 = (SERVICE_CIPHER << 8) | 0x00;
pub const CIPHER_DECRYPT: u32 = (SERVICE_CIPHER << 8) | 0x01;

// The cipher this driver names, by the device's numbering - which is also its bit in `cipher_algo_l`.
pub const AES_CBC: u32 = 3;

// A session's direction, and its kind: a plain cipher, with no hash chained to it.
pub const OP_ENCRYPT: u32 = 1;
pub const OP_DECRYPT: u32 = 2;
pub const SYM_OP_CIPHER: u32 = 1;

// The device's answers.
pub const STATUS_OK: u32 = 0;
pub const STATUS_ERR: u32 = 1;
pub const STATUS_BADMSG: u32 = 2;
pub const STATUS_NOTSUPP: u32 = 3;
pub const STATUS_INVSESS: u32 = 4;
pub const STATUS_NOSPC: u32 = 5;

// The configuration says the engine is ready.
pub const STATUS_HW_READY: u32 = 1 << 0;

// Every request header is this long, control and data alike.
pub const REQUEST_LEN: usize = 72;
// What the device writes back for a session it created: its id and a status.
pub const SESSION_INPUT_LEN: usize = 16;

// THE BOUNDS THIS DRIVER HOLDS EVERY REQUEST TO: one page of source and one of destination, a 16-byte block and
// its IV, and the sessions one connection may hold.
pub const BLOCK: usize = 16;
pub const MAX_DATA: usize = 4096;
pub const MAX_KEY: usize = 32;
pub const MAX_SESSIONS: usize = 8;

// The configuration's layout: the offsets of what this driver reads.
pub const CONFIG_STATUS: u64 = 0;
pub const CONFIG_MAX_DATAQUEUES: u64 = 4;
pub const CONFIG_SERVICES: u64 = 8;
pub const CONFIG_CIPHER_ALGO_L: u64 = 12;
pub const CONFIG_MAX_CIPHER_KEY_LEN: u64 = 36;
pub const CONFIG_MAX_SIZE: u64 = 48;
pub const CONFIG_LEN: u64 = 56;

// WHAT THE DEVICE SAYS IT IS, read from its configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
	pub status: u32,
	pub max_dataqueues: u32,
	pub services: u32,
	pub cipher_algo_l: u32,
	pub max_cipher_key_len: u32,
	pub max_size: u64,
}

impl Config {
	/// The configuration from its bytes, in the order the specification lays them out.
	pub fn parse(bytes: &[u8]) -> Option<Config> {
		if (bytes.len() as u64) < CONFIG_LEN {
			return None;
		}
		let u32_at = |at: u64| u32::from_le_bytes(bytes[at as usize..at as usize + 4].try_into().unwrap_or([0; 4]));
		let u64_at = |at: u64| u64::from_le_bytes(bytes[at as usize..at as usize + 8].try_into().unwrap_or([0; 8]));
		Some(Config { status: u32_at(CONFIG_STATUS), max_dataqueues: u32_at(CONFIG_MAX_DATAQUEUES), services: u32_at(CONFIG_SERVICES), cipher_algo_l: u32_at(CONFIG_CIPHER_ALGO_L), max_cipher_key_len: u32_at(CONFIG_MAX_CIPHER_KEY_LEN), max_size: u64_at(CONFIG_MAX_SIZE) })
	}

	/// The engine is ready and serves ciphers on at least one data queue.
	pub fn usable(&self) -> bool {
		self.status & STATUS_HW_READY != 0 && self.services & (1 << SERVICE_CIPHER) != 0 && self.max_dataqueues >= 1
	}

	/// Whether the device offers `algo`.
	pub fn offers(&self, algo: u32) -> bool {
		algo < 32 && self.cipher_algo_l & (1 << algo) != 0
	}

	/// The control queue's index: after every data queue.
	pub fn control_queue(&self) -> u16 {
		u16::try_from(self.max_dataqueues).unwrap_or(u16::MAX)
	}

	/// The longest key both the device and this driver take.
	pub fn max_key(&self) -> usize {
		(self.max_cipher_key_len as usize).min(MAX_KEY)
	}

	/// The most bytes one request may carry: the device's bound when it states one, and this driver's page.
	pub fn max_request(&self) -> usize {
		if self.max_size == 0 { MAX_DATA } else { (self.max_size.min(MAX_DATA as u64)) as usize }
	}
}

fn put32(out: &mut [u8], at: usize, value: u32) {
	out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut [u8], at: usize, value: u64) {
	out[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// A CIPHER SESSION'S CREATION: the control header (opcode, algorithm, flag, queue), then the cipher's
/// parameters - algorithm, key length, direction - and the session's kind after the 48 bytes its union takes.
/// The key follows as its own buffer and is not part of this.
pub fn create_session(algo: u32, key_len: u32, op: u32, queue: u32) -> [u8; REQUEST_LEN] {
	let mut out = [0u8; REQUEST_LEN];
	put32(&mut out, 0, CIPHER_CREATE_SESSION);
	put32(&mut out, 4, algo);
	put32(&mut out, 12, queue);
	put32(&mut out, 16, algo);
	put32(&mut out, 20, key_len);
	put32(&mut out, 24, op);
	put32(&mut out, 16 + 48, SYM_OP_CIPHER);
	out
}

/// A SESSION'S DESTRUCTION: the control header and the session's id.
pub fn destroy_session(algo: u32, session: u64, queue: u32) -> [u8; REQUEST_LEN] {
	let mut out = [0u8; REQUEST_LEN];
	put32(&mut out, 0, CIPHER_DESTROY_SESSION);
	put32(&mut out, 4, algo);
	put32(&mut out, 12, queue);
	put64(&mut out, 16, session);
	out
}

/// ONE CIPHER REQUEST: the operation header (opcode, algorithm, session), the lengths of the IV, the source and the
/// destination, and the request's kind after the 40 bytes its union takes.
pub fn cipher_request(opcode: u32, algo: u32, session: u64, iv_len: u32, src_len: u32, dst_len: u32) -> [u8; REQUEST_LEN] {
	let mut out = [0u8; REQUEST_LEN];
	put32(&mut out, 0, opcode);
	put32(&mut out, 4, algo);
	put64(&mut out, 8, session);
	put32(&mut out, 24, iv_len);
	put32(&mut out, 28, src_len);
	put32(&mut out, 32, dst_len);
	put32(&mut out, 24 + 40, SYM_OP_CIPHER);
	out
}

/// What the device wrote back for a session it was asked to create: its id, and its status.
pub fn session_input(bytes: &[u8]) -> Option<(u64, u32)> {
	if bytes.len() < SESSION_INPUT_LEN {
		return None;
	}
	Some((u64::from_le_bytes(bytes[0..8].try_into().ok()?), u32::from_le_bytes(bytes[8..12].try_into().ok()?)))
}

// WHY A REQUEST IS REFUSED BEFORE IT REACHES THE DEVICE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A key that is not 16, 24 or 32 bytes, or longer than the device takes.
	Key,
	/// An IV that is not one block.
	Iv,
	/// Data that is not whole blocks, or no data at all.
	Data,
	/// More than one request may carry.
	TooLong,
	/// A cipher the device does not offer.
	Unoffered,
}

/// A key the session may be opened with.
pub fn check_key(len: usize, config: &Config) -> Result<(), Refusal> {
	if !matches!(len, 16 | 24 | 32) || len > config.max_key() {
		return Err(Refusal::Key);
	}
	Ok(())
}

/// One request's IV and data: one block of IV, and whole blocks of data.
pub fn check_request(iv_len: usize, data_len: usize, config: &Config) -> Result<(), Refusal> {
	if iv_len != BLOCK {
		return Err(Refusal::Iv);
	}
	if data_len == 0 || data_len % BLOCK != 0 {
		return Err(Refusal::Data);
	}
	if data_len > config.max_request() {
		return Err(Refusal::TooLong);
	}
	Ok(())
}

// ONE SESSION A CONNECTION HOLDS: the device's id for it, and what it was opened for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Session {
	pub device_id: u64,
	pub algo: u32,
	pub encrypt: bool,
}

// THE SESSIONS ONE CONNECTION HOLDS, by the number the connection knows each by. Bounded, and numbered from one,
// so a zero is never a session; a number is not reused while the connection lives, so a late request naming a
// closed session cannot reach a newer one.
pub struct Sessions {
	slots: [Option<(u32, Session)>; MAX_SESSIONS],
	next: u32,
}

impl Default for Sessions {
	fn default() -> Self {
		Sessions::new()
	}
}

impl Sessions {
	pub const fn new() -> Sessions {
		Sessions { slots: [None; MAX_SESSIONS], next: 1 }
	}

	/// Room for one more.
	pub fn has_room(&self) -> bool {
		self.slots.iter().any(Option::is_none)
	}

	/// The session recorded, under a number of its own - None when the connection holds as many as it may, or
	/// has used every number.
	pub fn insert(&mut self, session: Session) -> Option<u32> {
		let slot = self.slots.iter_mut().find(|slot| slot.is_none())?;
		let number = self.next;
		self.next = self.next.checked_add(1)?;
		*slot = Some((number, session));
		Some(number)
	}

	pub fn get(&self, number: u32) -> Option<Session> {
		self.slots.iter().flatten().find(|(held, _)| *held == number).map(|(_, session)| *session)
	}

	/// The session taken out, for its destruction at the device.
	pub fn remove(&mut self, number: u32) -> Option<Session> {
		let slot = self.slots.iter_mut().find(|slot| slot.is_some_and(|(held, _)| held == number))?;
		slot.take().map(|(_, session)| session)
	}

	/// Every session taken out, for a connection that has gone.
	pub fn drain(&mut self) -> impl Iterator<Item = Session> + '_ {
		self.slots.iter_mut().filter_map(|slot| slot.take().map(|(_, session)| session))
	}

	pub fn len(&self) -> usize {
		self.slots.iter().flatten().count()
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
}

/// Overwrite `bytes` with zeros in a way the compiler may not drop: key material is not left behind in a
/// buffer that outlives its use.
pub fn zeroize(bytes: &mut [u8]) {
	for byte in bytes.iter_mut() {
		// SAFETY: a valid, aligned, exclusively borrowed byte.
		unsafe { core::ptr::write_volatile(byte, 0) };
	}
	core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests;
