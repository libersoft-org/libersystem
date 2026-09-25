// TPM 2.0 BELOW THE BINDING: how a TPM is found, how a command reaches it and comes back, and the handful of
// typed operations anything above it may ask for.
//
// WHY A LIBRARY AND NOT A DRIVER. A PC's TPM is ACPI-described MMIO at a fixed address - the `TPM2` table and
// an `MSFT0101` node - and not a PCI function, so a driver process has nothing to claim until a firmware-node
// device identity exists; and the operations have no consumer until measured boot or an identity service does.
// Both are decisions and neither is this crate's. What every binding and every consumer will share is here:
// discovery, the two standard transports, bounded commands and validated responses, and operations typed
// narrowly enough that nothing above ever holds a raw command stream.
//
// THREE RULES, because they are the three ways a TPM conversation goes wrong quietly:
//   - A RESPONSE IS BELIEVED ONLY AS FAR AS ITS HEADER, and its header only after it is checked: the tag is the
//     one that was asked for, the size is what arrived and within the bound, and a non-success code is an
//     answer of exactly ten bytes. Every field after it is read through a bounded reader that refuses rather
//     than reads past the end.
//   - EVERY INTEGER ON THIS WIRE IS BIG-ENDIAN. The ACPI table beside it is little-endian, and the transports'
//     registers are little-endian; a helper that knew only one byte order would be right in two places of three.
//   - NOTHING IS PASSED THROUGH. The operations below build their own commands; there is no function that
//     sends caller-supplied bytes, so the only commands that can reach a TPM through this crate are the ones
//     written here.

#![no_std]

extern crate alloc;

pub mod command;
pub mod crb;
pub mod fifo;
pub mod marshal;
pub mod ops;
pub mod table;

#[cfg(test)]
mod tests;

/// The largest command or response this crate builds or accepts. A CRB's buffer in QEMU is 3968 bytes and a
/// PC Client TPM's maximum is 4096; nothing an operation here sends or expects comes near either.
pub const MAX_MESSAGE: usize = 3968;

/// Structure tags.
pub const ST_NO_SESSIONS: u16 = 0x8001;
pub const ST_SESSIONS: u16 = 0x8002;
pub const ST_ATTEST_QUOTE: u16 = 0x8018;
/// What a TPM writes at the head of everything it signs, so that nothing else it signs can pass for it.
pub const GENERATED_VALUE: u32 = 0xff54_4347;

/// Command codes.
pub const CC_CREATE_PRIMARY: u32 = 0x131;
pub const CC_SELF_TEST: u32 = 0x143;
pub const CC_STARTUP: u32 = 0x144;
pub const CC_CREATE: u32 = 0x153;
pub const CC_LOAD: u32 = 0x157;
pub const CC_QUOTE: u32 = 0x158;
pub const CC_UNSEAL: u32 = 0x15E;
pub const CC_FLUSH_CONTEXT: u32 = 0x165;
pub const CC_START_AUTH_SESSION: u32 = 0x176;
pub const CC_GET_RANDOM: u32 = 0x17B;
pub const CC_PCR_READ: u32 = 0x17E;
pub const CC_POLICY_PCR: u32 = 0x17F;
pub const CC_PCR_EXTEND: u32 = 0x182;
pub const CC_POLICY_GET_DIGEST: u32 = 0x189;

/// Permanent handles.
pub const RH_OWNER: u32 = 0x4000_0001;
pub const RH_NULL: u32 = 0x4000_0007;
pub const RS_PW: u32 = 0x4000_0009;

/// Algorithms and the one curve.
pub const ALG_AES: u16 = 0x0006;
pub const ALG_KEYEDHASH: u16 = 0x0008;
pub const ALG_SHA256: u16 = 0x000B;
pub const ALG_NULL: u16 = 0x0010;
pub const ALG_ECDSA: u16 = 0x0018;
pub const ALG_ECC: u16 = 0x0023;
pub const ALG_CFB: u16 = 0x0043;
pub const ECC_NIST_P256: u16 = 0x0003;
pub const SHA256_LEN: usize = 32;

/// Session types.
pub const SE_POLICY: u8 = 0x01;
pub const SE_TRIAL: u8 = 0x03;

/// Object attributes.
pub const OA_FIXED_TPM: u32 = 1 << 1;
pub const OA_FIXED_PARENT: u32 = 1 << 4;
pub const OA_SENSITIVE_DATA_ORIGIN: u32 = 1 << 5;
pub const OA_USER_WITH_AUTH: u32 = 1 << 6;
pub const OA_NO_DA: u32 = 1 << 10;
pub const OA_RESTRICTED: u32 = 1 << 16;
pub const OA_DECRYPT: u32 = 1 << 17;
pub const OA_SIGN: u32 = 1 << 18;

/// Response codes this crate acts on.
pub const RC_SUCCESS: u32 = 0;
/// Startup when the TPM has already started: not an error to a caller that only wanted it started.
pub const RC_INITIALIZE: u32 = 0x100;
/// Warnings that mean "ask again".
pub const RC_YIELDED: u32 = 0x908;
pub const RC_CANCELED: u32 = 0x909;
pub const RC_TESTING: u32 = 0x90A;
pub const RC_RETRY: u32 = 0x922;
/// A format-one error's number for a policy that does not hold.
pub const RC_POLICY_FAIL: u32 = 0x01D;
/// The warning for a PCR that changed after a policy session recorded it.
pub const RC_PCR_CHANGED: u32 = 0x928;

/// What went wrong, from the register to the response code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	/// The transport could not carry the command or bring its response back.
	Transport(TransportError),
	/// A response that is not one: a header or a field that does not parse within its bounds.
	Malformed(Refused),
	/// The TPM answered with this response code.
	Tpm(u32),
	/// An unseal the policy refused: the PCRs are not what the secret was sealed to.
	PolicyRefused,
	/// A PCR this locality may not extend, or one that does not exist.
	Locality,
	/// A request past this crate's bounds.
	Bounds,
}

/// Why a transport failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
	/// The locality was not granted within the interface's timeout.
	Locality,
	/// The interface did not become ready for a command.
	NotReady,
	/// The TPM stopped taking the command before it was whole, or wanted more than there was.
	Expect,
	/// The command ran past its duration and was cancelled.
	TimedOut,
	/// The TPM reported a fatal error, or its interface is not the one it claimed.
	Fault,
	/// The TPM named a buffer outside the region this transport may touch, or one too small.
	Buffer,
	/// A response longer than the bound, or shorter than its own header says.
	Length,
}

/// Why a response was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
	/// Shorter than a header, or than a field it declares.
	Short,
	/// A size field that disagrees with what arrived, or past the bound.
	Length,
	/// A tag that is not the one the command was sent with.
	Tag,
	/// A value where the specification allows only others.
	Value,
}

impl From<TransportError> for Error {
	fn from(error: TransportError) -> Error {
		Error::Transport(error)
	}
}

impl From<Refused> for Error {
	fn from(refused: Refused) -> Error {
		Error::Malformed(refused)
	}
}

/// Whether a response code is a format-one code (bit 7) carrying `number` in its low six bits.
pub fn format_one_is(code: u32, number: u32) -> bool {
	code & 0x80 != 0 && code & 0x3f == number
}

/// A TPM interface's registers, as whoever mapped them reaches them - a driver's MMIO mapping, a kernel test's
/// window, a host test's model - with the clock the interface's timeouts are measured against.
pub trait Registers {
	fn read8(&mut self, offset: usize) -> u8;
	fn write8(&mut self, offset: usize, value: u8);
	fn read32(&mut self, offset: usize) -> u32;
	fn write32(&mut self, offset: usize, value: u32);
	/// Milliseconds on a clock that only moves forward.
	fn now_ms(&mut self) -> u64;
	/// Wait a little before the next poll.
	fn pause(&mut self);
}

/// The platform profile's timeouts, in milliseconds: A for a locality, B for a state change the TPM makes on
/// its own time, C for one it makes at once, D for a burst count to become non-zero.
pub const TIMEOUT_A_MS: u64 = 750;
pub const TIMEOUT_B_MS: u64 = 2_000;
pub const TIMEOUT_C_MS: u64 = 200;
pub const TIMEOUT_D_MS: u64 = 30;

/// Poll `done` until it holds or `timeout_ms` passes. Whether it held.
pub fn wait_for<R: Registers>(registers: &mut R, timeout_ms: u64, mut done: impl FnMut(&mut R) -> bool) -> bool {
	let start = registers.now_ms();
	loop {
		if done(registers) {
			return true;
		}
		if registers.now_ms().saturating_sub(start) > timeout_ms {
			return done(registers);
		}
		registers.pause();
	}
}
