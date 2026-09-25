// The host suite: the marshaling both ways, every response refusal, the TPM2 table, the two transports against
// models of the platform profile's register sets, and the typed operations against a scripted TPM. What a real
// TPM answers is `tools/tpm-conformance` against swtpm and the kernel test against QEMU's two front-ends.

use crate::command::{Transport, check_response, retryable, session_response};
use crate::crb::{self, Crb};
use crate::fifo::{self, Fifo};
use crate::marshal::{Reader, Writer};
use crate::ops::{self, Tpm};
use crate::table::{self, Interface};
use crate::*;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

// A response: tag, size, code and a body.
fn response(tag: u16, code: u32, body: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(&tag.to_be_bytes());
	out.extend_from_slice(&((10 + body.len()) as u32).to_be_bytes());
	out.extend_from_slice(&code.to_be_bytes());
	out.extend_from_slice(body);
	out
}

// ------------------------------------------------------------------ marshaling

#[test]
fn a_command_is_big_endian_and_its_size_is_patched_in() {
	let mut writer = Writer::command(ST_NO_SESSIONS, CC_GET_RANDOM);
	writer.u16(16);
	let command = writer.finish().expect("a command within the bound");
	assert_eq!(command, vec![0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x7B, 0, 16]);
}

#[test]
fn a_command_past_the_bound_is_never_built() {
	let mut writer = Writer::command(ST_NO_SESSIONS, CC_GET_RANDOM);
	writer.bytes(&vec![0; MAX_MESSAGE]);
	assert!(writer.finish().is_none());
}

#[test]
fn a_reader_refuses_rather_than_reads_past_the_end() {
	let mut reader = Reader::new(&[0, 5, 1, 2, 3]);
	assert_eq!(reader.sized(), Err(Refused::Short), "a size of five over three bytes");
	let mut reader = Reader::new(&[0, 2, 7, 8, 9]);
	assert_eq!(reader.sized(), Ok(&[7u8, 8][..]));
	assert_eq!(reader.u16(), Err(Refused::Short));
	assert_eq!(reader.u8(), Ok(9));
	assert_eq!(Reader::new(&[1, 2, 3, 4, 5, 6, 7, 8]).u64(), Ok(0x0102_0304_0506_0708));
}

// ------------------------------------------------------------------ responses

#[test]
fn a_response_is_believed_only_after_its_header() {
	let good = response(ST_NO_SESSIONS, RC_SUCCESS, &[0, 2, 0xAA, 0xBB]);
	assert_eq!(check_response(&good, ST_NO_SESSIONS).map(|checked| checked.body), Ok(&[0u8, 2, 0xAA, 0xBB][..]));
	assert_eq!(check_response(&good[..9], ST_NO_SESSIONS), Err(Refused::Short), "shorter than a header");
	let mut lying = good.clone();
	lying[5] += 1;
	assert_eq!(check_response(&lying, ST_NO_SESSIONS), Err(Refused::Length), "a size that is not what arrived");
	assert_eq!(check_response(&good, ST_SESSIONS), Err(Refused::Tag), "a tag that is not the one sent");
	let error = response(ST_NO_SESSIONS, 0x0101, &[]);
	assert_eq!(check_response(&error, ST_SESSIONS).map(|checked| checked.code), Ok(0x0101), "an error has no sessions whatever the command had");
	let padded_error = response(ST_NO_SESSIONS, 0x0101, &[0]);
	assert_eq!(check_response(&padded_error, ST_NO_SESSIONS), Err(Refused::Tag), "and is ten bytes, nothing after them");
	let session_error = response(ST_SESSIONS, 0x0101, &[]);
	assert_eq!(check_response(&session_error, ST_SESSIONS), Err(Refused::Tag));
}

#[test]
fn only_the_warnings_that_mean_ask_again_are_retried() {
	assert!(retryable(RC_RETRY) && retryable(RC_YIELDED) && retryable(RC_TESTING));
	assert!(!retryable(RC_CANCELED) && !retryable(RC_INITIALIZE) && !retryable(0x9A2));
}

#[test]
fn a_session_response_is_read_by_its_parameter_size_and_its_authorization_walked() {
	// A handle, four bytes of parameters, and one password session's response area.
	let body = [0x80, 0, 0, 1, 0, 0, 0, 4, 1, 2, 3, 4, 0, 0, 1, 0, 0];
	let parsed = session_response(&body, 1).expect("a whole response");
	assert_eq!((parsed.handles, parsed.parameters), (vec![0x8000_0001], &[1u8, 2, 3, 4][..]));
	assert_eq!(session_response(&body[..body.len() - 1], 1).err(), Some(Refused::Short), "an authorization area cut short");
	assert_eq!(session_response(&[0, 0, 0, 9, 1], 0).err(), Some(Refused::Short), "parameters longer than what arrived");
}

// ------------------------------------------------------------------ structures

#[test]
fn a_selection_names_one_pcr_in_the_sha256_bank_and_only_that_one_is_accepted_back() {
	let mut writer = Writer::empty();
	ops::select(&mut writer, 16);
	let bytes = writer.into_bytes();
	assert_eq!(bytes, vec![0, 0, 0, 1, 0x00, 0x0B, 3, 0, 0, 1]);
	assert_eq!(ops::selected(&mut Reader::new(&bytes), 16), Ok(()));
	assert_eq!(ops::selected(&mut Reader::new(&bytes), 15), Err(Refused::Value), "another PCR");
	let mut sha1 = bytes.clone();
	sha1[5] = 0x04;
	assert_eq!(ops::selected(&mut Reader::new(&sha1), 16), Err(Refused::Value), "another bank");
}

#[test]
fn the_templates_say_what_each_key_may_do() {
	let storage = ops::storage_template();
	let attributes = u32::from_be_bytes([storage[4], storage[5], storage[6], storage[7]]);
	assert!(attributes & OA_RESTRICTED != 0 && attributes & OA_DECRYPT != 0 && attributes & OA_SIGN == 0, "a storage key decrypts and never signs");
	let signing = ops::signing_template();
	let attributes = u32::from_be_bytes([signing[4], signing[5], signing[6], signing[7]]);
	assert!(attributes & OA_RESTRICTED != 0 && attributes & OA_SIGN != 0 && attributes & OA_DECRYPT == 0, "a quoting key is restricted: it signs only what the TPM made");
	let sealed = ops::sealed_template(&[7; 32]);
	let attributes = u32::from_be_bytes([sealed[4], sealed[5], sealed[6], sealed[7]]);
	assert_eq!(attributes & (OA_USER_WITH_AUTH | OA_SENSITIVE_DATA_ORIGIN), 0, "no password opens a sealed secret, and the TPM does not make it up");
	assert_eq!(&sealed[8..10], &[0, 32], "its policy is the digest given");
}

fn ecc_public(x: &[u8], y: &[u8]) -> Vec<u8> {
	let mut writer = Writer::empty();
	writer.u16(ALG_ECC).u16(ALG_SHA256).u32(OA_SIGN | OA_RESTRICTED).sized(&[]);
	writer.u16(ALG_NULL).u16(ALG_ECDSA).u16(ALG_SHA256).u16(ECC_NIST_P256).u16(ALG_NULL);
	writer.sized(x).sized(y);
	writer.into_bytes()
}

#[test]
fn a_public_key_gives_its_point_and_nothing_else() {
	let public = ecc_public(&[1; 32], &[2; 32]);
	assert_eq!(ops::ecc_point(&public), Ok((vec![1; 32], vec![2; 32])));
	assert_eq!(ops::ecc_point(&ecc_public(&[1; 31], &[2; 32])), Err(Refused::Value), "a short coordinate");
	let mut trailing = public.clone();
	trailing.push(0);
	assert_eq!(ops::ecc_point(&trailing), Err(Refused::Value), "bytes after the point");
}

fn attestation(magic: u32, kind: u16, nonce: &[u8], pcr: u32, digest: &[u8]) -> Vec<u8> {
	let mut writer = Writer::empty();
	writer.u32(magic).u16(kind).sized(&[0x00, 0x0B, 9, 9]).sized(nonce);
	writer.bytes(&[0; 8]).u32(1).u32(0).u8(1).bytes(&[0; 8]);
	ops::select(&mut writer, pcr);
	writer.sized(digest);
	writer.into_bytes()
}

#[test]
fn a_quote_is_believed_only_for_this_nonce_and_this_pcr() {
	let digest = [5u8; 32];
	let good = attestation(GENERATED_VALUE, ST_ATTEST_QUOTE, b"nonce", 16, &digest);
	assert_eq!(ops::attested(&good, b"nonce", 16), Ok(digest.to_vec()));
	assert_eq!(ops::attested(&good, b"other", 16), Err(Refused::Value), "another nonce: a replayed quote");
	assert_eq!(ops::attested(&good, b"nonce", 15), Err(Refused::Value), "another PCR");
	assert_eq!(ops::attested(&attestation(0x1234_5678, ST_ATTEST_QUOTE, b"nonce", 16, &digest), b"nonce", 16), Err(Refused::Value), "not the TPM's magic");
	assert_eq!(ops::attested(&attestation(GENERATED_VALUE, 0x8014, b"nonce", 16, &digest), b"nonce", 16), Err(Refused::Value), "a certification, not a quote");
}

// ------------------------------------------------------------------ the TPM2 table

fn tpm2_table(control: u64, method: u32) -> Vec<u8> {
	let mut table = vec![0u8; 52];
	table[0..4].copy_from_slice(b"TPM2");
	table[4..8].copy_from_slice(&52u32.to_le_bytes());
	table[8] = 4;
	table[40..48].copy_from_slice(&control.to_le_bytes());
	table[48..52].copy_from_slice(&method.to_le_bytes());
	let sum = table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	table[9] = 0u8.wrapping_sub(sum);
	table
}

#[test]
fn the_table_says_which_interface_and_where() {
	assert_eq!(table::discover(&tpm2_table(0, table::START_METHOD_FIFO)), Ok(Interface::Fifo { base: table::FIFO_BASE }));
	assert_eq!(table::discover(&tpm2_table(0xFED4_0040, table::START_METHOD_CRB)), Ok(Interface::Crb { base: 0xFED4_0000 }));
	assert_eq!(table::discover(&tpm2_table(0xFED4_0080, table::START_METHOD_CRB)), Err(table::Refusal::Layout), "a control area where the profile puts none");
	for method in [table::START_METHOD_ACPI, table::START_METHOD_CRB_ACPI, table::START_METHOD_CRB_SMC, table::START_METHOD_FIFO_I2C] {
		assert_eq!(table::discover(&tpm2_table(0xFED4_0040, method)), Err(table::Refusal::Unsupported(method)));
	}
	let mut corrupt = tpm2_table(0, table::START_METHOD_FIFO);
	corrupt[20] ^= 1;
	assert!(matches!(table::discover(&corrupt), Err(table::Refusal::Table(_))), "a checksum that does not add up");
}

// ------------------------------------------------------------------ the FIFO interface, against a model

// A FIFO TPM as the platform profile describes it: a locality to request, commandReady, a FIFO that takes the
// command in bursts with Expect set until the size its header declared has arrived, tpmGo, and the response read
// back in bursts. `answer` is what it says to any command.
struct FifoModel {
	burst: usize,
	granted: bool,
	ready: bool,
	command: Vec<u8>,
	answer: Vec<u8>,
	out: VecDeque<u8>,
	executed: bool,
	// The command as it was when tpmGo was written.
	carried: Vec<u8>,
	// Faults to model: stop expecting after this many bytes, never finish, and an answer longer than it says.
	stop_after: Option<usize>,
	never_finishes: bool,
	cancelled: bool,
	relinquished: u32,
	clock: u64,
}

impl FifoModel {
	fn new(answer: Vec<u8>, burst: usize) -> FifoModel {
		FifoModel { burst, granted: false, ready: false, command: Vec::new(), answer, out: VecDeque::new(), executed: false, carried: Vec::new(), stop_after: None, never_finishes: false, cancelled: false, relinquished: 0, clock: 0 }
	}

	fn expected(&self) -> Option<usize> {
		(self.command.len() >= 6).then(|| u32::from_be_bytes([self.command[2], self.command[3], self.command[4], self.command[5]]) as usize)
	}

	fn expects(&self) -> bool {
		if self.stop_after.is_some_and(|stop| self.command.len() >= stop) {
			return false;
		}
		self.expected().is_none_or(|size| self.command.len() < size)
	}
}

impl Registers for FifoModel {
	fn read8(&mut self, offset: usize) -> u8 {
		match offset {
			fifo::ACCESS => fifo::ACCESS_VALID | if self.granted { fifo::ACCESS_ACTIVE_LOCALITY } else { 0 },
			fifo::DATA_FIFO => self.out.pop_front().unwrap_or(0xFF),
			_ => 0xFF,
		}
	}

	fn write8(&mut self, offset: usize, value: u8) {
		match offset {
			fifo::ACCESS if value == fifo::ACCESS_REQUEST_USE => self.granted = true,
			fifo::ACCESS if value == fifo::ACCESS_ACTIVE_LOCALITY => {
				self.granted = false;
				self.relinquished += 1;
			}
			fifo::DATA_FIFO if self.ready => self.command.push(value),
			_ => {}
		}
	}

	fn read32(&mut self, offset: usize) -> u32 {
		match offset {
			fifo::INTERFACE_ID => fifo::INTERFACE_FIFO,
			fifo::STS => {
				// `stsValid` IS CLEAR UNTIL RECEPTION STARTS, as QEMU's FIFO has it: ready, with no byte taken yet,
				// the TPM has nothing to say about Expect.
				let mut status = if self.ready && !self.executed && self.command.is_empty() { 0 } else { fifo::STS_VALID };
				if self.ready && !self.executed {
					status |= fifo::STS_COMMAND_READY;
					if self.expects() {
						status |= fifo::STS_EXPECT;
					}
					status |= (self.burst as u32) << 8;
				}
				if self.executed && !self.out.is_empty() {
					status |= fifo::STS_DATA_AVAIL | (self.burst.min(self.out.len()) as u32) << 8;
				}
				status
			}
			_ => 0xFFFF_FFFF,
		}
	}

	fn write32(&mut self, offset: usize, value: u32) {
		if offset != fifo::STS {
			return;
		}
		if value & fifo::STS_COMMAND_READY != 0 {
			self.ready = true;
			self.executed = false;
			self.command.clear();
			self.out.clear();
		}
		if value & fifo::STS_GO != 0 && !self.never_finishes {
			self.carried = self.command.clone();
			self.executed = true;
			self.out = self.answer.iter().copied().collect();
		}
		if value & fifo::STS_COMMAND_CANCEL != 0 {
			self.cancelled = true;
		}
	}

	fn now_ms(&mut self) -> u64 {
		self.clock
	}

	fn pause(&mut self) {
		self.clock += 1;
	}
}

fn random_command() -> Vec<u8> {
	let mut writer = Writer::command(ST_NO_SESSIONS, CC_GET_RANDOM);
	writer.u16(4);
	writer.finish().unwrap()
}

#[test]
fn a_fifo_carries_a_command_in_the_bursts_the_tpm_names_and_gives_the_locality_back() {
	let answer = response(ST_NO_SESSIONS, RC_SUCCESS, &[0, 4, 1, 2, 3, 4]);
	for burst in [1, 3, 64] {
		let mut transport = Fifo::new(FifoModel::new(answer.clone(), burst)).expect("a FIFO interface");
		let mut out = Vec::new();
		transport.execute(&random_command(), &mut out, 100).expect("carried");
		assert_eq!(out, answer, "the response, whole, with a burst of {burst}");
		assert_eq!(transport.registers.carried, random_command(), "and the command, whole");
		assert_eq!((transport.registers.granted, transport.registers.relinquished), (false, 1), "the locality given back");
	}
}

#[test]
fn a_fifo_that_stops_expecting_early_fails_the_command_and_is_put_back() {
	let mut model = FifoModel::new(response(ST_NO_SESSIONS, RC_SUCCESS, &[]), 4);
	model.stop_after = Some(8);
	let mut transport = Fifo::new(model).unwrap();
	assert_eq!(transport.execute(&random_command(), &mut Vec::new(), 100), Err(TransportError::Expect));
	assert_eq!(transport.registers.relinquished, 1, "the locality given back on the failure path too");
}

#[test]
fn a_command_that_runs_past_its_duration_is_cancelled() {
	let mut model = FifoModel::new(response(ST_NO_SESSIONS, RC_SUCCESS, &[]), 64);
	model.never_finishes = true;
	let mut transport = Fifo::new(model).unwrap();
	assert_eq!(transport.execute(&random_command(), &mut Vec::new(), 50), Err(TransportError::TimedOut));
	assert!(transport.registers.cancelled, "commandCancel was written");
}

#[test]
fn a_response_that_says_it_is_longer_than_the_bound_is_refused_before_it_is_read() {
	let mut huge = response(ST_NO_SESSIONS, RC_SUCCESS, &[]);
	huge[2..6].copy_from_slice(&((MAX_MESSAGE + 1) as u32).to_be_bytes());
	let mut transport = Fifo::new(FifoModel::new(huge, 64)).unwrap();
	assert_eq!(transport.execute(&random_command(), &mut Vec::new(), 100), Err(TransportError::Length));
}

#[test]
fn a_crb_is_not_driven_as_a_fifo() {
	struct Crbish;
	impl Registers for Crbish {
		fn read8(&mut self, _: usize) -> u8 {
			0x80
		}
		fn write8(&mut self, _: usize, _: u8) {}
		fn read32(&mut self, offset: usize) -> u32 {
			if offset == fifo::INTERFACE_ID { fifo::INTERFACE_CRB } else { 0 }
		}
		fn write32(&mut self, _: usize, _: u32) {}
		fn now_ms(&mut self) -> u64 {
			0
		}
		fn pause(&mut self) {}
	}
	assert!(Fifo::new(Crbish).is_err());
}

// ------------------------------------------------------------------ the CRB interface, against a model

// A CRB TPM: a locality to request, cmdReady, a command and a response buffer at the addresses its registers
// name, and a start the TPM clears when the response is in.
struct CrbModel {
	memory: Vec<u8>,
	answer: Vec<u8>,
	granted: bool,
	started: bool,
	never_finishes: bool,
	cancel_written: Vec<u32>,
	relinquished: bool,
	clock: u64,
	command_at: u64,
	response_at: u64,
	// The command as it was when the start was written; the response may be written over it, as QEMU's is.
	carried: Vec<u8>,
}

const CRB_BASE: u64 = 0xFED4_0000;

impl CrbModel {
	fn new(answer: Vec<u8>) -> CrbModel {
		CrbModel { memory: vec![0; 0x1000], answer, granted: false, started: false, never_finishes: false, cancel_written: Vec::new(), relinquished: false, clock: 0, command_at: CRB_BASE + 0x80, response_at: CRB_BASE + 0x80, carried: Vec::new() }
	}

	fn command(&self) -> Vec<u8> {
		let at = (self.command_at - CRB_BASE) as usize;
		let size = u32::from_be_bytes([self.memory[at + 2], self.memory[at + 3], self.memory[at + 4], self.memory[at + 5]]) as usize;
		self.memory[at..at + size].to_vec()
	}
}

impl Registers for CrbModel {
	fn read8(&mut self, offset: usize) -> u8 {
		self.memory[offset]
	}

	fn write8(&mut self, offset: usize, value: u8) {
		self.memory[offset] = value;
	}

	fn read32(&mut self, offset: usize) -> u32 {
		match offset {
			crb::INTF_ID => fifo::INTERFACE_CRB,
			crb::LOC_STATE => crb::LOC_STATE_VALID | if self.granted { crb::LOC_STATE_ASSIGNED } else { 0 },
			crb::LOC_STS => u32::from(self.granted),
			crb::CTRL_REQ => 0,
			crb::CTRL_STS => 0,
			crb::CTRL_START => u32::from(self.started),
			crb::CTRL_CMD_SIZE | crb::CTRL_RSP_SIZE => 0xF80,
			crb::CTRL_CMD_LADDR => self.command_at as u32,
			crb::CTRL_CMD_HADDR => (self.command_at >> 32) as u32,
			crb::CTRL_RSP_ADDR => self.response_at as u32,
			x if x == crb::CTRL_RSP_ADDR + 4 => (self.response_at >> 32) as u32,
			_ => 0,
		}
	}

	fn write32(&mut self, offset: usize, value: u32) {
		match offset {
			crb::LOC_CTRL if value & crb::LOC_CTRL_REQUEST != 0 => self.granted = true,
			crb::LOC_CTRL if value & crb::LOC_CTRL_RELINQUISH != 0 => {
				self.granted = false;
				self.relinquished = true;
			}
			crb::CTRL_START if value == 1 => {
				self.carried = self.command();
				if self.never_finishes {
					self.started = true;
				} else {
					let at = (self.response_at - CRB_BASE) as usize;
					let answer = self.answer.clone();
					self.memory[at..at + answer.len()].copy_from_slice(&answer);
				}
			}
			crb::CTRL_CANCEL => {
				self.cancel_written.push(value);
				if value == 1 {
					self.started = false;
				}
			}
			_ => {}
		}
	}

	fn now_ms(&mut self) -> u64 {
		self.clock
	}

	fn pause(&mut self) {
		self.clock += 1;
	}
}

#[test]
fn a_crb_carries_a_command_through_the_buffers_it_names() {
	let answer = response(ST_NO_SESSIONS, RC_SUCCESS, &[0, 4, 9, 8, 7, 6]);
	let mut transport = Crb::new(CrbModel::new(answer.clone()), CRB_BASE, 0x1000).expect("a CRB");
	let mut out = Vec::new();
	transport.execute(&random_command(), &mut out, 100).expect("carried");
	assert_eq!(out, answer);
	assert_eq!(transport.registers.carried, random_command());
	assert!(transport.registers.relinquished);
}

#[test]
fn a_buffer_outside_the_region_or_over_its_registers_is_refused() {
	for at in [CRB_BASE + 0x1000, CRB_BASE + 0x40, CRB_BASE - 0x100] {
		let mut model = CrbModel::new(response(ST_NO_SESSIONS, RC_SUCCESS, &[]));
		model.command_at = at;
		let mut transport = Crb::new(model, CRB_BASE, 0x1000).unwrap();
		assert_eq!(transport.execute(&random_command(), &mut Vec::new(), 100), Err(TransportError::Buffer), "a command buffer at {at:#x}");
	}
}

#[test]
fn a_crb_start_that_does_not_clear_is_cancelled_and_the_cancel_cleared() {
	let mut model = CrbModel::new(response(ST_NO_SESSIONS, RC_SUCCESS, &[]));
	model.never_finishes = true;
	let mut transport = Crb::new(model, CRB_BASE, 0x1000).unwrap();
	assert_eq!(transport.execute(&random_command(), &mut Vec::new(), 50), Err(TransportError::TimedOut));
	assert_eq!(transport.registers.cancel_written, vec![1, 0], "set, then cleared");
}

// ------------------------------------------------------------------ the typed operations, against a script

// A TPM that answers from a script and records what it was sent.
struct Scripted {
	answers: VecDeque<Vec<u8>>,
	sent: Vec<Vec<u8>>,
}

impl Transport for Scripted {
	fn execute(&mut self, command: &[u8], response: &mut Vec<u8>, _duration_ms: u64) -> Result<(), TransportError> {
		self.sent.push(command.to_vec());
		response.extend_from_slice(&self.answers.pop_front().ok_or(TransportError::NotReady)?);
		Ok(())
	}
}

fn scripted(answers: Vec<Vec<u8>>) -> Tpm<Scripted> {
	Tpm::new(Scripted { answers: answers.into_iter().collect(), sent: Vec::new() })
}

fn code_of(command: &[u8]) -> u32 {
	u32::from_be_bytes([command[6], command[7], command[8], command[9]])
}

#[test]
fn a_tpm_that_already_started_is_started() {
	let mut tpm = scripted(vec![response(ST_NO_SESSIONS, RC_INITIALIZE, &[])]);
	assert_eq!(tpm.startup(), Ok(()));
	let mut tpm = scripted(vec![response(ST_NO_SESSIONS, 0x0101, &[])]);
	assert_eq!(tpm.startup(), Err(Error::Tpm(0x0101)));
}

#[test]
fn random_bytes_are_asked_for_in_digest_sized_pieces_and_a_retry_is_asked_again() {
	let piece = |count: usize| response(ST_NO_SESSIONS, RC_SUCCESS, &[&(count as u16).to_be_bytes()[..], &vec![0x5A; count]].concat());
	let mut tpm = scripted(vec![response(ST_NO_SESSIONS, RC_RETRY, &[]), piece(32), piece(8)]);
	assert_eq!(tpm.random(40), Ok(vec![0x5A; 40]));
	assert_eq!(tpm.transport.sent.len(), 3, "the retry was asked again, and forty bytes were two pieces");
	let mut tpm = scripted(vec![piece(33)]);
	assert_eq!(tpm.random(32), Err(Error::Malformed(Refused::Value)), "more than was asked for");
	assert_eq!(scripted(vec![]).random(MAX_RANDOM_PLUS_ONE), Err(Error::Bounds));
}

const MAX_RANDOM_PLUS_ONE: usize = ops::MAX_RANDOM + 1;

#[test]
fn a_pcr_read_is_one_digest_for_the_pcr_asked_about() {
	let mut body = Writer::empty();
	body.u32(7);
	ops::select(&mut body, 16);
	body.u32(1).sized(&[3; 32]);
	let mut tpm = scripted(vec![response(ST_NO_SESSIONS, RC_SUCCESS, &body.into_bytes())]);
	assert_eq!(tpm.pcr_read(16), Ok([3; 32]));
	let mut other = Writer::empty();
	other.u32(7);
	ops::select(&mut other, 15);
	other.u32(1).sized(&[3; 32]);
	let mut tpm = scripted(vec![response(ST_NO_SESSIONS, RC_SUCCESS, &other.into_bytes())]);
	assert_eq!(tpm.pcr_read(16), Err(Error::Malformed(Refused::Value)), "another PCR's value is not this one's");
}

#[test]
fn only_the_pcrs_locality_zero_may_extend_are_extended() {
	for pcr in 17..=22 {
		assert_eq!(scripted(vec![]).pcr_extend(pcr, &[0; 32]), Err(Error::Locality), "PCR {pcr} belongs to the dynamic root of trust");
	}
	assert_eq!(scripted(vec![]).pcr_extend(24, &[0; 32]), Err(Error::Locality));
	let mut tpm = scripted(vec![response(ST_SESSIONS, RC_SUCCESS, &[0, 0, 0, 0, 0, 0, 1, 0, 0])]);
	assert_eq!(tpm.pcr_extend(16, &[9; 32]), Ok(()));
	let sent = &tpm.transport.sent[0];
	assert_eq!((code_of(sent), &sent[10..14]), (CC_PCR_EXTEND, &[0u8, 0, 0, 16][..]), "PCR 16 is the handle");
	assert_eq!(&sent[14..18], &[0, 0, 0, 9], "and a password session authorizes it");
}

#[test]
fn an_unseal_the_policy_refuses_is_named_and_everything_it_loaded_is_flushed() {
	// CreatePrimary, Load, StartAuthSession (after its GetRandom), PolicyPCR, the nonce's GetRandom, then Unseal
	// refused by the policy - and the three flushes: the session, the object, the primary.
	let primary = response(ST_SESSIONS, RC_SUCCESS, &[&[0x80, 0, 0, 1][..], &[0, 0, 0, 2, 0, 0], &[0, 0, 1, 0, 0]].concat());
	let loaded = response(ST_SESSIONS, RC_SUCCESS, &[&[0x80, 0, 0, 2][..], &[0, 0, 0, 2, 0, 0], &[0, 0, 1, 0, 0]].concat());
	let random = |count: usize| response(ST_NO_SESSIONS, RC_SUCCESS, &[&(count as u16).to_be_bytes()[..], &vec![1; count]].concat());
	let session = response(ST_NO_SESSIONS, RC_SUCCESS, &[&[0x03, 0, 0, 0][..], &[0, 32], &[2; 32]].concat());
	let ok = response(ST_NO_SESSIONS, RC_SUCCESS, &[]);
	let refused = response(ST_NO_SESSIONS, 0x0000_099D, &[]);
	let mut tpm = scripted(vec![primary, loaded, random(32), session, ok.clone(), random(32), refused, ok.clone(), ok.clone(), ok]);
	let sealed = ops::Sealed { pcr: 16, public: vec![1], private: vec![2] };
	assert_eq!(tpm.unseal(&sealed), Err(Error::PolicyRefused));
	let flushed: Vec<Vec<u8>> = tpm.transport.sent.iter().filter(|command| code_of(command) == CC_FLUSH_CONTEXT).map(|command| command[10..14].to_vec()).collect();
	assert_eq!(flushed, vec![vec![0x03, 0, 0, 0], vec![0x80, 0, 0, 2], vec![0x80, 0, 0, 1]], "the session, the object and the primary, each flushed");
}
