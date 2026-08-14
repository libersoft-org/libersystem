// The host side of the `liber:vfs@1` / `liber:log@1` world: which imports are granted, and how a
// guest's `(ptr, len)` becomes a slice of its own memory.
//
// HERE RATHER THAN IN THE HOST, so it can be tested. `component_host` is a userspace binary built
// for `*-unknown-none`, so nothing inside it can run on the host, and the only coverage the world
// had was one kernel end-to-end path - which needs a boot per assertion and can only exercise the
// happy case. Everything here is a pure decision over bytes and types, which is exactly the part
// where the interesting failures are: an unknown import, a wrongly typed one, a pointer at the top
// of the address space, a length that leaves the memory.
//
// This crate is where it lives because its own description says what it is - a WebAssembly parser
// and interpreter FOR THE LIBERSYSTEM HOST - and because a crate under `src/user` would have to be
// a staged library with a manifest row and a place in the image, which this is not: it is compiled
// into the host binary.
//
// The IPC half stays in `component_host`: reading a granted file, writing one, emitting a log entry.
// That half needs services, and its errors are already carried by the status codes below.

use crate::interp::Value;
use crate::module::{FuncType, ValType};

// The imports the host recognizes, resolved by name AND signature. Anything else is refused at
// instantiation - the component reaches nothing the host did not explicitly wire to a granted
// service.
//
// NAMED AND VERSIONED, because this is a compatibility boundary. The world's whole identity used to
// be the strings `liber.read`, `liber.write` and `liber.log`, so the day a signature changed an old
// module and a new one would both import `liber.read` with nothing to tell them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorldFn {
	Read,
	Write,
	Log,
}

// The world's status codes, returned in place of a byte count. They are ABI: a guest built apart
// from this host must agree on them, and `src/sdk/src/status.rs` carries the same four.
//
// This exists because every failure used to be `0`. "The file is empty", "the volume is read-only"
// and "the service did not answer" were one answer, on the boundary where a capability system most
// needs them apart.
pub const STATUS_DENIED: i32 = -1;
pub const STATUS_FAULT: i32 = -2;
pub const STATUS_IO: i32 = -3;
pub const STATUS_UNSUPPORTED: i32 = -4;

// WHAT EACH SERVICE ERROR MEANS TO A GUEST, decided once and here rather than by whichever wildcard
// an adapter reached for. Both hosts implement `WorldServices` over the same `liber:base@1` error
// enum - `Denied`, `NotFound`, `Invalid`, `Again`, `Closed` - and until this was written down they
// answered the same failure with different statuses, which is the thing a version on an interface
// exists to make impossible.
//
//   Denied   -> Refused -> STATUS_DENIED. The only one that means "you may not". The grant does not
//               cover this and the guest should not retry.
//   NotFound -> Failed  -> STATUS_IO. THE HARD CASE, and the world has no "not there" to give it.
//               The guest never names a path: it asks for THE granted input or THE granted output,
//               and which file that is was decided by the host's manifest before the instance
//               existed. So a granted path that is not there is the HOST being misconfigured, not
//               the guest asking for something it may not have - and telling the guest DENIED would
//               blame it for a wiring mistake it cannot see, let alone fix. `STATUS_IO` says "this
//               did not work and it is not about your authority", which is exactly true here. The
//               day the world gains a path argument this decision has to be made again.
//   Invalid  -> Failed  -> STATUS_IO. This host asked the service wrongly. Nobody's business but
//               this host's, and certainly not a statement about the guest's grant.
//   Again    -> Failed  -> STATUS_IO. "Try later" - and a guest told DENIED will not.
//   Closed   -> Failed  -> STATUS_IO. The peer went away: the machine, not the grant.

// Why an import is not in the world - the two different answers `None` used to give at once.
//
// A caller that has to SAY which of the two happened had to re-derive the table to find out, and
// `src/tools/mkpackages` did exactly that: a second copy of the world's three rows, kept only so its
// two panics could name whether the import was unknown or wrongly typed. A compatibility boundary
// with two copies is a boundary that drifts, and the drift's failure mode is a package the
// build-time gate passes and the host refuses - which is the one thing that gate exists to prevent.
//
// So the distinction lives with the rule that makes it, and the gate reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportError {
	// No operation of this world answers to `module`.`field` at all. The guest is asking for
	// authority the host does not offer - a camera, somebody else's world, a version this host does
	// not implement.
	Unknown,
	// The name is the world's and the type is not. Carries what the world DECLARES, so a caller can
	// report what was expected and not only that something was wrong - a build-time gate saying
	// "wrong signature" and nothing else leaves the reader to go and find the world by hand.
	Signature { params: &'static [ValType], results: &'static [ValType] },
}

// Resolve one import to its world operation, BY NAME AND BY SIGNATURE. This is the whole authority
// surface: only these three names are wired, and only with the types the world declares.
//
// The signature was never consulted, and it matters more here than it would elsewhere because
// `Value::as_i32` converts `I64`, `F32` and `F64` as well as `I32` - so a module declaring
// `liber.read` with the wrong type did not get "incompatible import", it got a silent conversion at
// call time, on the boundary whose entire job is to be the place where the guest's word is not
// taken for anything.
//
// AN UNKNOWN IMPORT IS REFUSED AT INSTANTIATION, not tolerated until it is called. For a capability
// system the import list IS the manifest of requested authority: a module asking for `liber.camera`
// is asking for a camera, and "the call site might be unreachable" is not an answer to that.
pub fn resolve(module: &str, field: &str, signature: Option<&FuncType>) -> Result<WorldFn, ImportError> {
	let (op, params, results): (WorldFn, &'static [ValType], &'static [ValType]) = match (module, field) {
		// read(ptr: u32, max: u32) -> i32 (count, or a negative status)
		("liber:vfs@1", "read") => (WorldFn::Read, &[ValType::I32, ValType::I32], &[ValType::I32]),
		// write(ptr: u32, len: u32) -> i32 (count, or a negative status)
		("liber:vfs@1", "write") => (WorldFn::Write, &[ValType::I32, ValType::I32], &[ValType::I32]),
		// log(ptr: u32, len: u32) -> i32 (0, or a negative status)
		("liber:log@1", "log") => (WorldFn::Log, &[ValType::I32, ValType::I32], &[ValType::I32]),
		_ => return Err(ImportError::Unknown),
	};
	// A module whose type index points nowhere has no signature to check, which is not a reason to
	// admit it. It is a SIGNATURE failure rather than an unknown name, because the name was found:
	// the world offers this operation and this module did not say it wanted that shape of it.
	let Some(signature) = signature else { return Err(ImportError::Signature { params, results }) };
	if signature.params != params || signature.results != results {
		return Err(ImportError::Signature { params, results });
	}
	Ok(op)
}

// Resolve a (ptr, len) argument pair into a bounds-checked [ptr, end) memory window.
//
// A wasm32 address is a 32-BIT PATTERN, not a signed integer. Reading it as `i32 as usize`
// sign-extends anything at or above 0x8000_0000 into an enormous `usize`, which the bound below
// then refuses - so the ABI was silently capped at the low 2 GiB. Today's module has a small memory
// and cannot reach it; the conversion is still the wrong one.
//
// REFUSED RATHER THAN CLAMPED. `end` used to be `.min(mem_len)`, so a component asking to write
// three hundred bytes from an address near the top of its memory silently wrote fewer - the caller
// was told a count it could not distinguish from a short file. A window that does not fit is
// `Fault`, and the guest can say so.
pub fn window(args: &[Value], mem_len: usize) -> Option<(usize, usize)> {
	// BOTH ARGUMENTS, and typed. Missing ones defaulted to zero, which is a guess about a call that
	// should not have happened: the world's signature is `(i32, i32)` and `resolve` refuses an
	// import that declares anything else, so an import reaching here with the wrong arity is a
	// broken interpreter and `None` says so.
	//
	// And matched rather than converted. `Value::as_i32` turns `I64`, `F32` and `F64` into an `i32`
	// too, and there is no type validator below this - so a module may declare `(param i32 i32)`
	// correctly and push two `F64`s in a body nothing type-checked. Defence in depth, and it stays
	// correct after the validator lands.
	let [Value::I32(ptr), Value::I32(len)] = args[..] else { return None };
	let ptr: usize = ptr as u32 as usize;
	let len: usize = len as u32 as usize;
	let end: usize = ptr.checked_add(len)?;
	if end > mem_len {
		return None;
	}
	Some((ptr, end))
}

// Why a write did not happen, rather than whether it did.
//
// An `Option<usize>` collapsed three different things into `None` and the world turned all of them
// into `STATUS_DENIED`: a buffer the HOST could not create, a service that never answered, and a
// service that answered and refused. So a guest was told to stop asking when the truth might be
// that the host ran out of memory or that StorageService is gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteOutcome {
	Wrote(usize),
	// The volume said no, and the component can act on that.
	Refused,
	// The host could not make the request, or the service did not answer it. Not the volume's
	// doing, and not something the guest can fix by asking differently.
	Failed,
	// THIS HOST DOES NOT OFFER THIS OPERATION AT ALL - not "you may not", which is what `Refused`
	// means and what a host without a write grant used to answer. The difference is whether a
	// different grant would help: for `Refused` it would, and for this it would not, because there
	// is nothing behind the import in this host to grant.
	Unsupported,
}

// Why a read did not happen, rather than whether it did.
//
// THE SAME ARGUMENT `WriteOutcome` IS MADE OF, applied to the other two operations. The comment
// above it says an `Option<usize>` "collapsed three different things into `None`" - a buffer the
// host could not create, a service that never answered, and a service that answered and refused -
// and `read` and `log` in the same trait went on being `Option<usize>` and `bool`. The argument is
// about the trait, not about one method of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadOutcome {
	Read(usize),
	// The volume said no. THE GRANT DOES NOT COVER THIS, and nothing else - see the decision on
	// `Error::NotFound` under the status codes above: a granted path that is not there is the
	// host's misconfiguration and answers `Failed`, not this.
	Refused,
	// The host could not make the request, or the service did not answer it.
	Failed,
	// This host does not offer this operation at all. See `WriteOutcome::Unsupported`.
	Unsupported,
}

// The same for the log, which has no count to report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogOutcome {
	Logged,
	Refused,
	Failed,
	Unsupported,
}

// The two capabilities the world is wired to, as three operations over bytes.
//
// THE SEAM THIS EXISTS FOR: everything above it is a decision about a guest's bytes and can be
// tested on a development machine; everything below it is IPC to a running service and cannot.
// Before it existed the whole dispatch lived in `component_host`, a `*-unknown-none` binary, and
// the only way to reach any of it was a kernel boot running the one real component - which by
// construction takes the happy path. A write that is refused, a write that fails, a service that
// never answers and a log grant that does not respond were four paths with no test between them.
pub trait WorldServices {
	// Read the one granted input file into `dst`, answering how many bytes were copied - or which
	// kind of failure stopped it, which the guest gets as `STATUS_DENIED` or `STATUS_IO`.
	fn read(&mut self, dst: &mut [u8]) -> ReadOutcome;
	// Write `bytes` to the one granted output file.
	fn write(&mut self, bytes: &[u8]) -> WriteOutcome;
	// Emit `text` as one entry through the granted log.
	fn log(&mut self, text: &str) -> LogOutcome;
}

// The host side of the world: the resolved import table, the services it dispatches to, and the two
// facts the caller reports afterwards.
//
// A FAILED CALL IS A STATUS, NOT A TRAP, throughout. The component asked a legitimate question and
// the answer is that it could not be served; killing the instance tells it nothing and gives it no
// chance to say so to its caller. The one exception is an import index outside the resolved table,
// which is the interpreter contradicting itself rather than anything the guest did.
pub struct WorldHost<S: WorldServices> {
	services: S,
	imports: alloc::vec::Vec<WorldFn>,
	// whether the log grant was reached and an entry accepted.
	logged: bool,
	// the bytes the component handed to its `write` import - its output, seen through the granted
	// write path, rather than a guess at where they sit in linear memory.
	output: alloc::vec::Vec<u8>,
}

impl<S: WorldServices> WorldHost<S> {
	pub fn new(services: S, imports: alloc::vec::Vec<WorldFn>) -> Self {
		WorldHost { services, imports, logged: false, output: alloc::vec::Vec::new() }
	}

	pub fn logged(&self) -> bool {
		self.logged
	}

	pub fn output(&self) -> &[u8] {
		&self.output
	}

	pub fn services_mut(&mut self) -> &mut S {
		&mut self.services
	}
}

impl<S: WorldServices> crate::interp::Host for WorldHost<S> {
	fn call_import(&mut self, import: u32, args: &[Value], memory: &mut [u8]) -> Result<alloc::vec::Vec<Value>, crate::interp::Trap> {
		// The dispatch table was built at instantiation from imports this world offers, so an index
		// outside it is the interpreter contradicting itself rather than a component asking for
		// something it was not granted.
		let Some(op) = self.imports.get(import as usize).copied() else {
			return Err(crate::interp::Trap("import index outside the resolved world"));
		};
		let Some((ptr, end)) = window(args, memory.len()) else {
			return Ok(alloc::vec![Value::I32(STATUS_FAULT)]);
		};
		let status: i32 = match op {
			// read(ptr, max) -> n: the one granted input file into the component's memory.
			WorldFn::Read => match self.services.read(&mut memory[ptr..end]) {
				ReadOutcome::Read(n) => n as i32,
				ReadOutcome::Refused => STATUS_DENIED,
				ReadOutcome::Failed => STATUS_IO,
				ReadOutcome::Unsupported => STATUS_UNSUPPORTED,
			},
			// write(ptr, len) -> n: the component's bytes to the one granted output file.
			WorldFn::Write => {
				self.output = memory[ptr..end].to_vec();
				match self.services.write(&memory[ptr..end]) {
					WriteOutcome::Wrote(n) => n as i32,
					WriteOutcome::Refused => STATUS_DENIED,
					WriteOutcome::Failed => STATUS_IO,
					WriteOutcome::Unsupported => STATUS_UNSUPPORTED,
				}
			}
			// log(ptr, len) -> 0: the component's bytes as one structured entry.
			WorldFn::Log => {
				// UTF-8 is checked HERE rather than in the service, because the two failures are
				// not the same failure. The world's `log` takes TEXT; bytes that are not text are a
				// guest that broke its side of the contract, and reporting that as `STATUS_IO`
				// blames the service for the component's mistake.
				match core::str::from_utf8(&memory[ptr..end]) {
					Err(_) => STATUS_UNSUPPORTED,
					Ok(text) => match self.services.log(text) {
						LogOutcome::Logged => {
							self.logged = true;
							0
						}
						LogOutcome::Refused => STATUS_DENIED,
						// The log used to return NOTHING, so a component could not tell whether its
						// one diagnostic channel had worked.
						LogOutcome::Failed => STATUS_IO,
						LogOutcome::Unsupported => STATUS_UNSUPPORTED,
					},
				}
			}
		};
		Ok(alloc::vec![Value::I32(status)])
	}
}

#[cfg(test)]
mod tests;
