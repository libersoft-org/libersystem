//! WHAT WENT WRONG, by name: a malformed table, a bound a method ran past, a region the host refused. An
//! evaluation that fails is aborted and reported, never retried.

use alloc::string::String;
use core::fmt;

use crate::host::HostError;

/// The bound an evaluation ran past.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bound {
	/// More opcodes than one evaluation may run.
	Steps,
	/// Longer than one evaluation may take.
	Time,
	/// Method calls nested deeper than the bound.
	Calls,
	/// Expressions nested deeper than the bound.
	Nesting,
	/// More memory than one evaluation may allocate.
	Memory,
	/// One loop ran more iterations than the bound.
	Loop,
	/// Packages nested deeper than the bound.
	PackageDepth,
	/// More tables than the namespace holds.
	Tables,
	/// More nodes than the namespace holds.
	Nodes,
}

impl Bound {
	pub fn name(&self) -> &'static str {
		match self {
			Bound::Steps => "the opcode bound",
			Bound::Time => "the time bound",
			Bound::Calls => "the call-depth bound",
			Bound::Nesting => "the nesting bound",
			Bound::Memory => "the memory bound",
			Bound::Loop => "the loop bound",
			Bound::PackageDepth => "the package-depth bound",
			Bound::Tables => "the table bound",
			Bound::Nodes => "the namespace bound",
		}
	}
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
	/// The bytes are not AML: what was wrong, and the offset in the code being read.
	Malformed(&'static str, usize),
	/// An opcode this interpreter does not know.
	UnknownOpcode(u16, usize),
	/// A name that resolves to nothing.
	NotFound(String),
	/// An operand of the wrong type.
	Type(String),
	/// A bound was reached.
	Bound(Bound),
	/// The host refused or failed a region access.
	Region(HostError),
	/// A region space no handler serves, or an operation this interpreter refuses (`Unload`, a GPIO write).
	Refused(String),
	/// A table that is not one: bad length, bad checksum, the wrong signature.
	BadTable(&'static str),
	/// `Fatal` was executed.
	Fatal { kind: u8, code: u32, argument: u64 },
	/// An index past the end of a package, buffer or string.
	Index(u64),
	/// Division by zero.
	DivideByZero,
	/// `Break` or `Continue` outside a loop, `Return` outside a method.
	BadControl(&'static str),
}

impl From<HostError> for Error {
	fn from(error: HostError) -> Error {
		Error::Region(error)
	}
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Error::Malformed(what, at) => write!(f, "malformed AML at offset {at:#x}: {what}"),
			Error::UnknownOpcode(op, at) => write!(f, "unknown opcode {op:#x} at offset {at:#x}"),
			Error::NotFound(name) => write!(f, "{name} is not in the namespace"),
			Error::Type(what) => write!(f, "wrong operand type: {what}"),
			Error::Bound(bound) => write!(f, "aborted at {}", bound.name()),
			Error::Region(HostError::Refused(why)) => write!(f, "a region access was refused: {why}"),
			Error::Region(HostError::Unavailable(why)) => write!(f, "a region access had no handler: {why}"),
			Error::Region(HostError::Failed(why)) => write!(f, "a region access failed: {why}"),
			Error::Refused(why) => write!(f, "refused: {why}"),
			Error::BadTable(why) => write!(f, "not a loadable table: {why}"),
			Error::Fatal { kind, code, argument } => write!(f, "Fatal({kind:#x}, {code:#x}, {argument:#x})"),
			Error::Index(index) => write!(f, "index {index} is past the end"),
			Error::DivideByZero => write!(f, "division by zero"),
			Error::BadControl(what) => write!(f, "{what}"),
		}
	}
}

/// THE BOUNDS ONE EVALUATION RUNS UNDER, and the namespace's own.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
	/// Opcodes one evaluation may run.
	pub steps: u64,
	/// Milliseconds one evaluation may take, by the host's clock - `Sleep` included.
	pub time_ms: u64,
	/// Method calls nested.
	pub calls: usize,
	/// Expressions nested within one method.
	pub nesting: usize,
	/// Bytes one evaluation may allocate.
	pub memory: usize,
	/// Iterations of one `While`.
	pub loop_iterations: u64,
	/// Packages nested.
	pub package_depth: usize,
	/// Tables loaded, the DSDT included.
	pub tables: usize,
	/// Namespace nodes.
	pub nodes: usize,
}

impl Default for Limits {
	fn default() -> Limits {
		Limits { steps: 4_000_000, time_ms: 5_000, calls: 32, nesting: 128, memory: 4 << 20, loop_iterations: 0xFFFF, package_depth: 16, tables: 256, nodes: 65_536 }
	}
}
