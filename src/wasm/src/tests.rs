// Host-side tests for the WebAssembly runtime: they hand-encode small modules with
// a builder, parse them, and run them through the interpreter - including the
// import path, which is how a component reaches the host.

use crate::*;
use alloc::vec::Vec;

pub(crate) const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
const F32: u8 = 0x7d;
const F64: u8 = 0x7c;

// Unsigned LEB128 of `v`.
pub(crate) fn leb(mut v: u32) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	loop {
		let mut b: u8 = (v & 0x7f) as u8;
		v >>= 7;
		if v != 0 {
			b |= 0x80;
		}
		out.push(b);
		if v == 0 {
			break;
		}
	}
	out
}

// A section: id byte, then the LEB128-prefixed content.
pub(crate) fn section(id: u8, content: &[u8]) -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![id];
	out.extend_from_slice(&leb(content.len() as u32));
	out.extend_from_slice(content);
	out
}

// A length-prefixed name.
pub(crate) fn name(s: &str) -> Vec<u8> {
	let mut out: Vec<u8> = leb(s.len() as u32);
	out.extend_from_slice(s.as_bytes());
	out
}

// Signed LEB128 of `v`.
pub(crate) fn sleb(mut v: i64) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	loop {
		let b: u8 = (v & 0x7f) as u8;
		v >>= 7;
		let done: bool = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
		out.push(if done { b } else { b | 0x80 });
		if done {
			break;
		}
	}
	out
}

// A module specification the test builder turns into a wasm binary.
pub(crate) struct Spec<'a> {
	pub(crate) types: &'a [(&'a [u8], &'a [u8])],        // (param val-types, result val-types)
	pub(crate) imports: &'a [(&'a str, &'a str, u32)],   // (module, field, type index)
	pub(crate) funcs: &'a [u32],                         // type index per defined function
	pub(crate) mem_pages: u32,                           // 0 = declare no memory
	pub(crate) globals: &'a [(u8, bool, i64)],           // (value-type byte, mutable, constant init)
	pub(crate) data: &'a [(u32, &'a [u8])],              // (memory offset, bytes)
	pub(crate) exports: &'a [(&'a str, u8, u32)],        // (name, kind byte, index)
	pub(crate) codes: &'a [(&'a [(u32, u8)], &'a [u8])], // (local groups, body bytes)
}

// Encode a [`Spec`] as a wasm module binary.
pub(crate) fn build(spec: &Spec) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(b"\0asm");
	out.extend_from_slice(&[1, 0, 0, 0]);
	if !spec.types.is_empty() {
		let mut c: Vec<u8> = leb(spec.types.len() as u32);
		for &(p, r) in spec.types {
			c.push(0x60);
			c.extend_from_slice(&leb(p.len() as u32));
			c.extend_from_slice(p);
			c.extend_from_slice(&leb(r.len() as u32));
			c.extend_from_slice(r);
		}
		out.extend_from_slice(&section(1, &c));
	}
	if !spec.imports.is_empty() {
		let mut c: Vec<u8> = leb(spec.imports.len() as u32);
		for &(m, f, ti) in spec.imports {
			c.extend_from_slice(&name(m));
			c.extend_from_slice(&name(f));
			c.push(0x00);
			c.extend_from_slice(&leb(ti));
		}
		out.extend_from_slice(&section(2, &c));
	}
	if !spec.funcs.is_empty() {
		let mut c: Vec<u8> = leb(spec.funcs.len() as u32);
		for &ti in spec.funcs {
			c.extend_from_slice(&leb(ti));
		}
		out.extend_from_slice(&section(3, &c));
	}
	if spec.mem_pages > 0 {
		let mut c: Vec<u8> = leb(1);
		c.push(0x00);
		c.extend_from_slice(&leb(spec.mem_pages));
		out.extend_from_slice(&section(5, &c));
	}
	if !spec.globals.is_empty() {
		let mut c: Vec<u8> = leb(spec.globals.len() as u32);
		for &(vt, mutable, init) in spec.globals {
			c.push(vt);
			c.push(if mutable { 0x01 } else { 0x00 });
			c.push(if vt == 0x7e { 0x42 } else { 0x41 }); // i64.const / i32.const
			c.extend_from_slice(&sleb(init));
			c.push(0x0b); // end of the init expression
		}
		out.extend_from_slice(&section(6, &c));
	}
	if !spec.exports.is_empty() {
		let mut c: Vec<u8> = leb(spec.exports.len() as u32);
		for &(n, k, idx) in spec.exports {
			c.extend_from_slice(&name(n));
			c.push(k);
			c.extend_from_slice(&leb(idx));
		}
		out.extend_from_slice(&section(7, &c));
	}
	if !spec.codes.is_empty() {
		let mut c: Vec<u8> = leb(spec.codes.len() as u32);
		for &(groups, body) in spec.codes {
			let mut entry: Vec<u8> = leb(groups.len() as u32);
			for &(count, vt) in groups {
				entry.extend_from_slice(&leb(count));
				entry.push(vt);
			}
			entry.extend_from_slice(body);
			c.extend_from_slice(&leb(entry.len() as u32));
			c.extend_from_slice(&entry);
		}
		out.extend_from_slice(&section(10, &c));
	}
	if !spec.data.is_empty() {
		let mut c: Vec<u8> = leb(spec.data.len() as u32);
		for &(offset, bytes) in spec.data {
			c.push(0x00); // active segment, memory 0
			c.push(0x41); // i32.const offset
			c.extend_from_slice(&sleb(offset as i64));
			c.push(0x0b); // end of the offset expression
			c.extend_from_slice(&leb(bytes.len() as u32));
			c.extend_from_slice(bytes);
		}
		out.extend_from_slice(&section(11, &c));
	}
	out
}

// A host that refuses every import (for modules that should not call out).
struct NoHost;

impl Host for NoHost {
	fn call_import(&mut self, _import: u32, _args: &[Value], _memory: &mut [u8]) -> Result<Vec<Value>, Trap> {
		Err(Trap("no imports available"))
	}
}

#[test]
fn memory_grow_is_bounded_and_answers_minus_one() {
	// `memory.grow` was `memory.resize(memory.len() + delta * PAGE, 0)` with `delta` off the guest's
	// stack: no declared maximum consulted, no host ceiling, unchecked arithmetic, and an infallible
	// resize. A module asking for a large grow did not get the failure code the specification
	// defines - it took the host process down, from inside a service that runs other people's code.
	//
	// (memory 1) (func (export "run") (result i32) i32.const N memory.grow)
	let grow = |pages: i64| -> Vec<u8> {
		let mut body: Vec<u8> = alloc::vec![0x41];
		body.extend_from_slice(&sleb(pages));
		body.extend_from_slice(&[0x40, 0x00, 0x0b]); // memory.grow (memory 0), end
		body
	};
	let run = |pages: i64| -> Value {
		let body = grow(pages);
		let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] });
		let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
		let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
		inst.invoke("run", &[], &mut NoHost).unwrap()[0]
	};

	// An ordinary grow still answers the OLD page count, which is what a guest allocator reads.
	assert_eq!(run(1), Value::I32(1), "growing one page from one reports the page count before it");

	// The host's ceiling is 1024 pages, so this is refused - and refused with -1 and an unchanged
	// memory rather than by dying, which is the whole point.
	assert_eq!(run(4096), Value::I32(-1), "a grow past the host ceiling is refused, not attempted");

	// 0xffff_ffff pages is what an unchecked `delta * PAGE` used to turn into an enormous resize.
	assert_eq!(run(-1), Value::I32(-1), "the largest u32 delta is a refusal, not an allocation");

	// And the module's OWN maximum is honoured, below the host's. (memory 1 2) grown by two.
	let body = grow(2);
	let mut wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] });
	// Rewrite the memory section as `limits = { flags: 1, min: 1, max: 2 }`.
	let plain = section(5, &[1, 0x00, 1]);
	let capped = section(5, &[1, 0x01, 1, 2]);
	let at = wasm.windows(plain.len()).position(|w| w == plain.as_slice()).expect("the memory section this test just built");
	wasm.splice(at..at + plain.len(), capped);
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	assert_eq!(m.module().memory.and_then(|mem| mem.max_pages), Some(2), "the declared maximum is kept rather than skipped");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap()[0], Value::I32(-1), "one page plus two is past the module's own maximum of two");
}

#[test]
fn runs_a_constant() {
	// (func (export "run") (result i32) i32.const 42)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 42, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

#[test]
fn runs_arithmetic() {
	// (func (export "run") (result i32) i32.const 40  i32.const 2  i32.add)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 40, 0x41, 2, 0x6a, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

#[test]
fn passes_arguments_through_locals() {
	// (func (export "run") (param i32) (result i32) local.get 0  i32.const 1  i32.add)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x20, 0, 0x41, 1, 0x6a, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(41)], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

// A host that services import 0 as a "read into memory": it writes "hello" at the
// requested pointer (clamped to the requested max) and returns the byte count.
struct ReadHost;

impl Host for ReadHost {
	fn call_import(&mut self, import: u32, args: &[Value], memory: &mut [u8]) -> Result<Vec<Value>, Trap> {
		if import != 0 {
			return Err(Trap("unknown import"));
		}
		let ptr: usize = args[0].as_i32() as usize;
		let max: usize = args[1].as_i32() as usize;
		let data: &[u8] = b"hello";
		let n: usize = data.len().min(max);
		memory[ptr..ptr + n].copy_from_slice(&data[..n]);
		Ok(alloc::vec![Value::I32(n as i32)])
	}
}

#[test]
fn calls_an_import_that_writes_memory() {
	// type 0: (i32, i32) -> i32 (the import); type 1: () -> i32 (run).
	// (func (export "run") (result i32) i32.const 0  i32.const 5  call $read)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	assert_eq!(m.module().imports.len(), 1);
	assert_eq!(m.module().export_func("run"), Some(1));
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	let result: Vec<Value> = inst.invoke("run", &[], &mut ReadHost).unwrap();
	assert_eq!(result, alloc::vec![Value::I32(5)], "run returns the byte count from the import");
	assert_eq!(&inst.memory()[0..5], b"hello", "the import wrote into linear memory");
}

#[test]
fn reads_back_memory_the_import_wrote() {
	// run: read 5 bytes at 0, drop the count, then load8_u memory[0] and return it.
	// i32.const 0  i32.const 5  call 0  drop  i32.const 0  i32.load8_u 0 0
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x1a, 0x41, 0, 0x2d, 0, 0, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	// 'h' is 104; the component read its granted bytes and returned the first one.
	assert_eq!(inst.invoke("run", &[], &mut ReadHost).unwrap(), alloc::vec![Value::I32(104)]);
}

#[test]
fn an_unwired_import_traps() {
	// The same module, but the host refuses the import: the component gets nothing.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("no imports available")));
}

#[test]
fn rejects_a_non_module() {
	assert_eq!(parse(&[0, 1, 2, 3, 4, 5, 6, 7]), Err(ParseError("bad magic")));
}

#[test]
fn decodes_a_multibyte_constant() {
	// i32.const 256 is the two-byte LEB128 0x80 0x02 - exercise multi-byte decoding
	// (the WASI host's component passes a buffer size this way).
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 0x80, 0x02, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(256)]);
}

#[test]
fn rejects_an_unsupported_opcode() {
	// body: a SIMD (v128) prefix opcode - vector instructions are out of scope.
	//
	// REFUSED BY VALIDATION, not surfaced as a trap on the first invoke. That is the whole shape
	// change: the module never becomes an `Instance`, so nothing of it - its data segments, its
	// globals - is installed before it is known to be malformed.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0xfd, 0x00, 0x0b])] });
	let error: ValidationError = validate(parse(&wasm).unwrap()).expect_err("a vector opcode is refused");
	assert_eq!(error.func, Some(0), "and the refusal says which function it was in");
	assert_eq!(error.reason, "unsupported opcode");
}

#[test]
fn does_f64_arithmetic() {
	// (func (result f64) f64.const 2.0  f64.const 3.0  f64.mul  f64.sqrt) -> sqrt(6).
	let mut body: Vec<u8> = alloc::vec![0x44];
	body.extend_from_slice(&2.0f64.to_le_bytes());
	body.push(0x44);
	body.extend_from_slice(&3.0f64.to_le_bytes());
	body.push(0xa2); // f64.mul
	body.push(0x9f); // f64.sqrt
	body.push(0x0b);
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[F64])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	let r: Vec<Value> = inst.invoke("run", &[], &mut NoHost).unwrap();
	match r[0] {
		Value::F64(v) => assert!((v - 6.0f64.sqrt()).abs() < 1e-12, "got {v}"),
		other => panic!("expected f64, got {other:?}"),
	}
}

#[test]
fn compares_f32_and_converts_to_int() {
	// (func (param f32 f32) (result i32) local.get 0  local.get 1  f32.lt) - a < b ? 1 : 0.
	let body: &[u8] = &[0x20, 0x00, 0x20, 0x01, 0x5d, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[F32, F32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::F32(1.5), Value::F32(2.5)], &mut NoHost).unwrap(), alloc::vec![Value::I32(1)]);
	assert_eq!(inst.invoke("run", &[Value::F32(2.5), Value::F32(1.5)], &mut NoHost).unwrap(), alloc::vec![Value::I32(0)]);
}

#[test]
fn converts_int_to_float_and_floors() {
	// (func (param i32) (result f64) local.get 0  f64.convert_i32_s  f64.const 0.5  f64.add  f64.floor)
	let mut body: Vec<u8> = alloc::vec![0x20, 0x00, 0xb7]; // local.get 0; f64.convert_i32_s
	body.push(0x44);
	body.extend_from_slice(&0.5f64.to_le_bytes());
	body.push(0xa0); // f64.add
	body.push(0x9c); // f64.floor
	body.push(0x0b);
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[F64])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(7)], &mut NoHost).unwrap(), alloc::vec![Value::F64(7.0)]);
}

#[test]
fn traps_on_truncating_nan_to_int() {
	// (func (result i32) f64.const NaN  i32.trunc_f64_s) - an undefined conversion traps.
	let mut body: Vec<u8> = alloc::vec![0x44];
	body.extend_from_slice(&f64::NAN.to_le_bytes());
	body.push(0xaa); // i32.trunc_f64_s
	body.push(0x0b);
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("invalid conversion to integer")));
}

#[test]
fn loads_and_stores_a_float() {
	// (func (param f32) (result f32) i32.const 0  local.get 0  f32.store  i32.const 0  f32.load)
	let body: &[u8] = &[0x41, 0x00, 0x20, 0x00, 0x38, 0x02, 0x00, 0x41, 0x00, 0x2a, 0x02, 0x00, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[F32], &[F32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::F32(3.25)], &mut NoHost).unwrap(), alloc::vec![Value::F32(3.25)]);
}

#[test]
fn saturates_when_truncating_a_float_to_int() {
	// (func (param f64) (result i32) local.get 0  i32.trunc_sat_f64_s) - the
	// non-trapping cast Rust's `as` emits: NaN -> 0, out-of-range saturates to
	// i32::MIN/MAX, otherwise truncates toward zero.
	let body: &[u8] = &[0x20, 0x00, 0xfc, 0x02, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[F64], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::F64(3.9)], &mut NoHost).unwrap(), alloc::vec![Value::I32(3)]);
	assert_eq!(inst.invoke("run", &[Value::F64(1e18)], &mut NoHost).unwrap(), alloc::vec![Value::I32(i32::MAX)]);
	assert_eq!(inst.invoke("run", &[Value::F64(f64::NAN)], &mut NoHost).unwrap(), alloc::vec![Value::I32(0)]);
}

#[test]
fn loops_and_branches_to_sum() {
	// (func (param n i32) (result i32) (local sum i32) (local i i32)
	//   sum = 0; i = n; block { loop { if i == 0 break; sum += i; i -= 1; continue } }
	//   sum) - exercises block / loop / br / br_if and the integer ALU.
	let body: &[u8] = &[
		0x41,
		0x00,
		0x21,
		0x01, // i32.const 0; local.set 1 (sum = 0)
		0x20,
		0x00,
		0x21,
		0x02, // local.get 0; local.set 2 (i = n)
		0x02,
		0x40, // block
		0x03,
		0x40, // loop
		0x20,
		0x02,
		0x45,
		0x0d,
		0x01, // local.get 2; i32.eqz; br_if 1 (break)
		0x20,
		0x01,
		0x20,
		0x02,
		0x6a,
		0x21,
		0x01, // sum += i
		0x20,
		0x02,
		0x41,
		0x01,
		0x6b,
		0x21,
		0x02, // i -= 1
		0x0c,
		0x00, // br 0 (continue)
		0x0b, // end loop
		0x0b, // end block
		0x20,
		0x01, // local.get 1 (sum)
		0x0b, // end
	];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[(2, I32)], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(5)], &mut NoHost).unwrap(), alloc::vec![Value::I32(15)]);
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(0)]);
}

#[test]
fn takes_each_if_branch() {
	// (func (param i32) (result i32) (if (result i32) (then 10) (else 20)))
	let body: &[u8] = &[0x20, 0x00, 0x04, 0x7f, 0x41, 0x0a, 0x05, 0x41, 0x14, 0x0b, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut NoHost).unwrap(), alloc::vec![Value::I32(10)]);
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(20)]);
}

#[test]
fn dispatches_with_br_table() {
	// (func (param i32) (result i32)) switch: 0 -> 10, 1 -> 20, default -> 30.
	let body: &[u8] = &[
		0x02,
		0x40, // block $default
		0x02,
		0x40, // block $case1
		0x02,
		0x40, // block $case0
		0x20,
		0x00, // local.get 0
		0x0e,
		0x02,
		0x00,
		0x01,
		0x02, // br_table [0, 1] default 2
		0x0b, // end $case0
		0x41,
		0x0a,
		0x0f, // i32.const 10; return
		0x0b, // end $case1
		0x41,
		0x14,
		0x0f, // i32.const 20; return
		0x0b, // end $default
		0x41,
		0x1e, // i32.const 30
		0x0b, // end
	];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(10)]);
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut NoHost).unwrap(), alloc::vec![Value::I32(20)]);
	assert_eq!(inst.invoke("run", &[Value::I32(7)], &mut NoHost).unwrap(), alloc::vec![Value::I32(30)]);
}

#[test]
fn reads_and_writes_a_global() {
	// One mutable i32 global initialized to 7; run bumps it by one and returns it.
	let body: &[u8] = &[0x23, 0x00, 0x41, 0x01, 0x6a, 0x24, 0x00, 0x23, 0x00, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[(I32, true, 7)], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	// the global persists across calls on the same instance.
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(8)]);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(9)]);
}

#[test]
fn initializes_memory_from_a_data_segment() {
	// A data segment writes "Hi" at offset 0; run reads back the first byte.
	let body: &[u8] = &[0x41, 0x00, 0x2d, 0x00, 0x00, 0x0b]; // i32.const 0; i32.load8_u
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[(0, b"Hi")], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(b'H' as i32)]);
	assert_eq!(&inst.memory()[0..2], b"Hi", "the data segment initialized linear memory");
}

#[test]
fn does_i64_arithmetic() {
	// (func (result i64) i64.const 1  i64.const 32  i64.shl) -> 1 << 32, an i64 value.
	let body: &[u8] = &[0x42, 0x01, 0x42, 0x20, 0x86, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I64])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I64(4294967296)]);
}

#[test]
fn traps_on_divide_by_zero() {
	// (func (result i32) i32.const 10  i32.const 0  i32.div_s) traps at runtime.
	let body: &[u8] = &[0x41, 0x0a, 0x41, 0x00, 0x6d, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("integer divide by zero")));
}

#[test]
fn selects_between_two_values() {
	// (func (param i32) (result i32) i32.const 11  i32.const 22  local.get 0  select)
	let body: &[u8] = &[0x41, 0x0b, 0x41, 0x16, 0x20, 0x00, 0x1b, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut NoHost).unwrap(), alloc::vec![Value::I32(11)]);
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(22)]);
}

#[test]
fn rejects_an_out_of_range_branch() {
	// (func (result i32) br 1) - only the function-level label is in scope, so a branch to depth 1
	// is structurally invalid and is refused by validation rather than trapped on at invoke.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x0c, 0x01, 0x0b])] });
	let error: ValidationError = validate(parse(&wasm).unwrap()).expect_err("an out-of-range branch is refused");
	assert_eq!(error.reason, "branch label out of range");
}

// Build a module with a funcref table for the call_indirect tests. Two (i32)->i32
// callees (add-one at slot 0, double at slot 1), an (i64)->i64 callee at slot 2 (whose
// signature does not match the call site), and slot 3 left null (the table's declared
// minimum is 4, but the element segment fills only slots 0..2). The exported `run(sel)`
// calls `table[sel]` with the argument 10 through type 0 ((i32)->i32), so it round-trips
// for a matching callee and traps for a mismatch, a null entry, or an out-of-range index.
fn indirect_module() -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(b"\0asm");
	out.extend_from_slice(&[1, 0, 0, 0]);
	// types: 0 = (i32)->i32, 1 = (i64)->i64.
	let mut types: Vec<u8> = leb(2);
	types.extend_from_slice(&[0x60, 0x01, I32, 0x01, I32]);
	types.extend_from_slice(&[0x60, 0x01, I64, 0x01, I64]);
	out.extend_from_slice(&section(1, &types));
	// funcs: add_one(type 0), double(type 0), wrong(type 1), run(type 0).
	let mut funcs: Vec<u8> = leb(4);
	funcs.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]);
	out.extend_from_slice(&section(3, &funcs));
	// table: one funcref table, minimum 4 (no maximum).
	let mut table: Vec<u8> = leb(1);
	table.extend_from_slice(&[0x70, 0x00]);
	table.extend_from_slice(&leb(4));
	out.extend_from_slice(&section(4, &table));
	// export "run" = function 3.
	let mut exports: Vec<u8> = leb(1);
	exports.extend_from_slice(&name("run"));
	exports.push(0x00);
	exports.extend_from_slice(&leb(3));
	out.extend_from_slice(&section(7, &exports));
	// element: active, table 0, offset 0, functions [0, 1, 2] (slot 3 stays null).
	let mut elem: Vec<u8> = leb(1);
	elem.push(0x00); // flags: active, table 0, a vector of function indices
	elem.push(0x41); // i32.const offset
	elem.extend_from_slice(&sleb(0));
	elem.push(0x0b); // end of the offset expression
	elem.extend_from_slice(&leb(3));
	elem.extend_from_slice(&leb(0));
	elem.extend_from_slice(&leb(1));
	elem.extend_from_slice(&leb(2));
	out.extend_from_slice(&section(9, &elem));
	// code: the four bodies (no locals beyond the parameters).
	let bodies: [&[u8]; 4] = [
		&[0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],             // add_one: local.get 0; i32.const 1; i32.add
		&[0x20, 0x00, 0x20, 0x00, 0x6a, 0x0b],             // double: local.get 0; local.get 0; i32.add
		&[0x20, 0x00, 0x0b],                               // wrong: local.get 0 (returns its i64 argument)
		&[0x41, 0x0a, 0x20, 0x00, 0x11, 0x00, 0x00, 0x0b], // run: i32.const 10; local.get 0; call_indirect type 0 table 0
	];
	let mut code: Vec<u8> = leb(4);
	for b in bodies {
		let mut entry: Vec<u8> = leb(0);
		entry.extend_from_slice(b);
		code.extend_from_slice(&leb(entry.len() as u32));
		code.extend_from_slice(&entry);
	}
	out.extend_from_slice(&section(10, &code));
	out
}

#[test]
fn call_indirect_dispatches_through_the_table() {
	// The function-pointer / trait-object round trip: `run(sel)` invokes table[sel](10).
	// Slot 0 is add-one, slot 1 is double - what a Rust component's indirect call compiles
	// to and the one gap a real toolchain-built component hits first.
	let wasm: Vec<u8> = indirect_module();
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(11)]);
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut NoHost).unwrap(), alloc::vec![Value::I32(20)]);
}

#[test]
fn call_indirect_traps_on_a_signature_mismatch() {
	// Slot 2 holds an (i64)->i64 callee, but the call site expects (i32)->i32: the runtime
	// type-checks the table entry against the expected signature at call time and traps.
	let wasm: Vec<u8> = indirect_module();
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(2)], &mut NoHost), Err(Trap("call_indirect: indirect call type mismatch")));
}

#[test]
fn call_indirect_traps_on_a_null_or_out_of_range_entry() {
	// Slot 3 was never filled (a null entry), and slot 4 is past the table's end - both trap.
	let wasm: Vec<u8> = indirect_module();
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(3)], &mut NoHost), Err(Trap("call_indirect: uninitialized element")));
	assert_eq!(inst.invoke("run", &[Value::I32(4)], &mut NoHost), Err(Trap("call_indirect: undefined element")));
}

#[test]
fn sqrt_is_correctly_rounded_against_the_host() {
	// The engine's `sqrt` was Newton-Raphson to "within ~1 ULP, which the runtime accepts in
	// exchange for being self-contained" - and a component that computes a different last bit here
	// than everywhere else is a portability defect in an ABI meant to be a contract. Being
	// self-contained turned out not to require being approximate.
	//
	// Checked against the PLATFORM'S OWN `f64::sqrt`, which is the hardware instruction and is
	// correctly rounded by IEEE-754. That comparison is only possible here, because the crate pulls
	// in `std` under `cargo test`; the engine itself has neither libm nor intrinsics.
	let mut cases: Vec<f64> = alloc::vec![
		1.0,
		2.0,
		3.0,
		4.0,
		0.5,
		0.25,
		1e-300,
		1e300,
		f64::MIN_POSITIVE,
		// Subnormals, which the seed's exponent trick cannot read at all.
		f64::from_bits(1),
		f64::from_bits(0x000f_ffff_ffff_ffff),
		f64::from_bits(1 << 51),
		// Values whose roots are exactly representable, so the answer must be exact rather than
		// nearly so.
		9.0,
		16.0,
		1024.0,
		1.0 / 4096.0,
		// And the ones a Newton iteration is worst at: just above and below a power of two.
		f64::from_bits(1.0f64.to_bits() + 1),
		f64::from_bits(2.0f64.to_bits() - 1),
	];
	// Plus a deterministic sweep across the whole exponent range, three mantissas per binade.
	for exponent in -320i32..=308 {
		for mantissa in [1.0f64, 1.3, 1.9999999999999998] {
			let value = mantissa * libm_pow10(exponent);
			if value.is_finite() && value > 0.0 {
				cases.push(value);
			}
		}
	}
	let mut checked = 0usize;
	for &x in &cases {
		let ours = super::interp::sqrt_f64_for_test(x);
		let theirs = x.sqrt();
		assert_eq!(ours.to_bits(), theirs.to_bits(), "sqrt({x:e}): this engine says {ours:e} ({:#x}), the hardware says {theirs:e} ({:#x})", ours.to_bits(), theirs.to_bits());
		checked += 1;
	}
	assert!(checked > 900, "the sweep covered {checked} values, which is not a sweep");

	// And the signed zeros, which are the other half of the specification's float rules.
	assert_eq!(super::interp::sqrt_f64_for_test(-0.0).to_bits(), (-0.0f64).to_bits(), "sqrt(-0) is -0");
	assert_eq!(super::interp::sqrt_f64_for_test(0.0).to_bits(), 0.0f64.to_bits(), "sqrt(+0) is +0");
}

// 10^n without libm: repeated multiplication, which is exact enough for a test corpus.
fn libm_pow10(n: i32) -> f64 {
	let mut out = 1.0f64;
	if n >= 0 {
		for _ in 0..n {
			out *= 10.0;
		}
	} else {
		for _ in 0..-n {
			out /= 10.0;
		}
	}
	out
}

#[test]
fn rounding_keeps_the_sign_of_a_zero_result() {
	// `trunc(-0.5)` is `-0.0`, `ceil(-0.5)` is `-0.0` and `nearest(-0.3)` is `-0.0`. Every one of
	// them used to answer `+0.0`, because the arithmetic went through `as i64` and came back
	// unsigned - a one-bit difference that `copysign`, `1.0/x` and any component comparing bit
	// patterns can see.
	for &x in &[-0.5f64, -0.3, -0.0, -f64::MIN_POSITIVE] {
		assert!(super::interp::trunc_f64_for_test(x).is_sign_negative(), "trunc({x:e}) keeps its sign");
		assert!(super::interp::ceil_f64_for_test(x).is_sign_negative(), "ceil({x:e}) keeps its sign");
	}
	assert!(super::interp::nearest_f64_for_test(-0.3).is_sign_negative(), "nearest(-0.3) is -0.0");
	assert!(super::interp::nearest_f64_for_test(-0.5).is_sign_negative(), "nearest(-0.5) ties to even, which is -0.0");
	// And a positive input still gives a positive zero.
	assert!(super::interp::trunc_f64_for_test(0.5).is_sign_positive(), "trunc(0.5) is +0.0");
}

// ---------------------------------------------------------------------------------------------
// The refusal corpus: one module per rule the validator and the parser now enforce.
//
// EVERY ONE OF THESE WAS ONCE ACCEPTED BY THE TREE, and most of them ran. That ordering
// is the point of a corpus rather than a test list: a refusal test written after the refusal exists
// passes for reasons nobody checked. Each case below names what the old engine did with it.
// ---------------------------------------------------------------------------------------------

// Validate a hand-built module and return the refusal, or panic saying it was accepted.
fn refuse(wasm: &[u8], what: &str) -> String {
	match parse(wasm) {
		Err(ParseError(reason)) => alloc::string::String::from(reason),
		Ok(module) => match validate(module) {
			Err(error) => alloc::format!("{error}"),
			Ok(_) => panic!("{what}: accepted, and it must not be"),
		},
	}
}

#[test]
fn a_body_whose_operand_types_do_not_match_is_refused() {
	// (func (result i32) i64.const 1) - the body leaves an i64 where the signature promises an i32.
	// RAN BEFORE: nothing type-checked a body, so the i64 came back through `invoke` as the
	// function's "i32" result, and the world boundary above converted it to a pointer.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x42, 0x01, 0x0b])] });
	let reason = refuse(&wasm, "a body returning the wrong type");
	assert!(reason.contains("I32") && reason.contains("I64"), "the refusal names both types, got: {reason}");
}

#[test]
fn a_body_that_leaves_nothing_where_a_result_is_declared_is_refused() {
	// (func (result i32) nop) - RAN BEFORE, and `invoke` returned whatever the stack happened to
	// hold, which for an empty stack was an empty result vector the host then indexed.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x01, 0x0b])] });
	let reason = refuse(&wasm, "a body that produces no result");
	assert!(reason.contains("leaves"), "the refusal says what the block leaves, got: {reason}");
}

#[test]
fn an_instruction_popping_an_empty_stack_is_refused() {
	// (func i32.add) with nothing pushed. RAN BEFORE: `pop` trapped at run time, so a module could
	// be instantiated and partially execute before this was known.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x6a, 0x0b])] });
	let reason = refuse(&wasm, "an underflowing body");
	assert!(reason.contains("empty operand stack"), "got: {reason}");
}

#[test]
fn a_call_to_a_function_that_does_not_exist_is_refused() {
	// (func call 7) in a module with one function. RAN BEFORE: `call` looked the index up at run
	// time and trapped, so the module instantiated.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x10, 0x07, 0x0b])] });
	let reason = refuse(&wasm, "a call to a missing function");
	assert!(reason.contains("call to function 7"), "got: {reason}");
}

#[test]
fn a_local_index_past_the_frame_is_refused() {
	// (func local.get 3) with no parameters and no locals. RAN BEFORE: an out-of-range local was a
	// run-time trap.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x20, 0x03, 0x1a, 0x0b])] });
	let reason = refuse(&wasm, "an out-of-range local");
	assert!(reason.contains("local.get 3"), "got: {reason}");
}

#[test]
fn a_write_to_an_immutable_global_is_refused() {
	// (global i32 (i32.const 1)) - immutable - and (func i32.const 2 global.set 0).
	// RAN BEFORE: `global.set` wrote without consulting `Global.mutable`, which the parser recorded
	// and nothing read. Any module could change any global.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[(I32, false, 1)], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 0x02, 0x24, 0x00, 0x0b])] });
	let reason = refuse(&wasm, "a write to an immutable global");
	assert!(reason.contains("immutable global 0"), "got: {reason}");
}

#[test]
fn a_memory_instruction_without_a_memory_is_refused() {
	// (func i32.const 0 i32.load) in a module declaring no memory. RAN BEFORE: the load read a
	// zero-length memory and trapped on the bounds check - a run-time answer to a static question.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 0x00, 0x28, 0x02, 0x00, 0x0b])] });
	let reason = refuse(&wasm, "a load with no memory");
	assert!(reason.contains("no memory"), "got: {reason}");
}

#[test]
fn a_memarg_whose_alignment_exceeds_the_access_is_refused() {
	// (func i32.const 0 i32.load8_u align=2) - a one-byte access claiming four-byte alignment.
	// RAN BEFORE: the alignment immediate was read and dropped, so any value was accepted.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 0x00, 0x2d, 0x02, 0x00, 0x0b])] });
	let reason = refuse(&wasm, "an over-aligned memarg");
	assert!(reason.contains("alignment"), "got: {reason}");
}

#[test]
fn a_second_else_for_one_if_is_refused() {
	// (func (if (then nop) (else nop) (else nop))) - RAN BEFORE: the second `else` overwrote the
	// first's recorded jump, so the then-branch fell into the second arm and the first became
	// unreachable code nothing had said was unreachable.
	let body: &[u8] = &[0x41, 0x01, 0x04, 0x40, 0x01, 0x05, 0x01, 0x05, 0x01, 0x0b, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let reason = refuse(&wasm, "a second else");
	assert!(reason.contains("second else"), "got: {reason}");
}

#[test]
fn bytes_after_the_final_end_are_refused() {
	// A body whose terminating `end` is followed by more instructions. RAN BEFORE: the decoder
	// broke out of its loop at the final `end` and ignored whatever followed, so the module's
	// author and its reader disagreed about what the function contained.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x01, 0x0b, 0x00, 0x00])] });
	let reason = refuse(&wasm, "trailing bytes in a body");
	assert!(reason.contains("after the function body's final end"), "got: {reason}");
}

#[test]
fn a_leb128_with_a_trailing_zero_is_well_formed_and_an_over_wide_one_is_not() {
	// INVERTED, because the rule it pinned is not the specification's.
	//
	// It asserted that a type-section count of 1 written as `0x81 0x00` is refused, under a comment
	// saying "the specification requires the shortest encoding". It does not: the binary format's
	// integer grammar restricts the encoding's LENGTH, and its own note gives this exact example -
	// `0x03` and `0x83 0x00` are both well-formed encodings of 3. So the parser was refusing
	// conforming modules and a test was holding it there.
	//
	// The motivation - two byte-level spellings of one module - is a real concern for anything that
	// hashes or signs, and it belongs to a packaging policy over an image this system will ship,
	// not to the core parser's idea of well-formed.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	// section 1, content = [count = 1 as `0x81 0x00`, 0x60, 0 params, 0 results]
	let content: Vec<u8> = alloc::vec![0x81, 0x00, 0x60, 0x00, 0x00];
	wasm.extend_from_slice(&section(1, &content));
	let module = parse(&wasm).expect("a trailing zero is within the width bound, so the module is well-formed");
	assert_eq!(module.types.len(), 1, "and it decodes to the value it spells");

	// The bound that IS the specification's: a `u32` past five bytes, or a fifth byte carrying bits
	// above the 32nd.
	for (label, count) in [("six bytes", alloc::vec![0x80u8, 0x80, 0x80, 0x80, 0x80, 0x00]), ("bits above 32", alloc::vec![0x80u8, 0x80, 0x80, 0x80, 0x70])] {
		let mut wasm: Vec<u8> = alloc::vec![];
		wasm.extend_from_slice(b"\0asm");
		wasm.extend_from_slice(&[1, 0, 0, 0]);
		let mut content: Vec<u8> = count;
		content.extend_from_slice(&[0x60, 0x00, 0x00]);
		wasm.extend_from_slice(&section(1, &content));
		assert!(parse(&wasm).is_err(), "{label}: past the width the specification allows");
	}
}

#[test]
fn a_repeated_or_out_of_order_section_is_refused() {
	// Two type sections. RAN BEFORE: the second was parsed and its types APPENDED to the first's,
	// so the indices a module used depended on a rule the format does not have.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	let content: Vec<u8> = alloc::vec![0x01, 0x60, 0x00, 0x00];
	wasm.extend_from_slice(&section(1, &content));
	wasm.extend_from_slice(&section(1, &content));
	let reason = refuse(&wasm, "a repeated section");
	assert!(reason.contains("out of order or repeated"), "got: {reason}");

	// And out of order: the export section before the function section.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	let mut exports: Vec<u8> = leb(1);
	exports.extend_from_slice(&name("run"));
	exports.push(0x00);
	exports.extend_from_slice(&leb(0));
	wasm.extend_from_slice(&section(7, &exports));
	wasm.extend_from_slice(&section(3, &[0x01, 0x00]));
	let reason = refuse(&wasm, "sections out of order");
	assert!(reason.contains("out of order or repeated"), "got: {reason}");
}

#[test]
fn a_start_section_is_refused_rather_than_ignored() {
	// RAN BEFORE, silently: the start section was skipped by its declared size and `Instance::new`
	// never ran one, so a module whose initialisation never happened ran in a state its author
	// never wrote.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[0x01, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[0x01, 0x00]));
	wasm.extend_from_slice(&section(8, &[0x00]));
	wasm.extend_from_slice(&section(10, &[0x01, 0x02, 0x00, 0x0b]));
	let reason = refuse(&wasm, "a start section");
	assert!(reason.contains("start section"), "got: {reason}");
}

#[test]
fn a_code_section_that_does_not_match_the_function_section_is_refused() {
	// One declared function, two code entries. RAN BEFORE: the extra entry hit
	// "code entry without a function" only if it was reached, and the reverse case - a function
	// with no code - produced a function whose body was empty and whose calls fell off the end.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[0x01, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[0x01, 0x00]));
	wasm.extend_from_slice(&section(10, &[0x02, 0x02, 0x00, 0x0b, 0x02, 0x00, 0x0b]));
	let reason = refuse(&wasm, "a code section with too many entries");
	assert!(reason.contains("different counts"), "got: {reason}");

	// And a declared function with no code at all.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[0x01, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[0x01, 0x00]));
	let reason = refuse(&wasm, "a function with no code");
	assert!(reason.contains("no code entry"), "got: {reason}");
}

#[test]
fn a_global_initialised_by_the_wrong_type_is_refused() {
	// (global i32 (i64.const 1)) - RAN BEFORE: `const_expr` returned a value and no type, so the
	// bytes were simply evaluated and the i64 became the i32 global's initial value.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	let mut globals: Vec<u8> = leb(1);
	globals.push(I32);
	globals.push(0x00);
	globals.push(0x42); // i64.const
	globals.extend_from_slice(&sleb(1));
	globals.push(0x0b);
	wasm.extend_from_slice(&section(6, &globals));
	let reason = refuse(&wasm, "a global of the wrong initialiser type");
	assert!(reason.contains("declared type"), "got: {reason}");
}

#[test]
fn a_data_segment_past_the_declared_memory_is_refused() {
	// One page declared, a segment written at 100000. RAN BEFORE: `Instance::new` noticed and held
	// a `Trap` in a field, surfacing it on the first `invoke` - so the instance existed, with its
	// globals installed, carrying an error nobody had asked for yet.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[(100_000, b"x")], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x0b])] });
	let reason = refuse(&wasm, "a data segment past the memory");
	assert!(reason.contains("data segment 0"), "got: {reason}");
}

#[test]
fn an_export_naming_a_function_that_does_not_exist_is_refused() {
	// RAN BEFORE: `export_func` returned the index and `call` trapped on it at invoke time.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 9)], codes: &[(&[], &[0x0b])] });
	let reason = refuse(&wasm, "an export naming a missing function");
	assert!(reason.contains("names function 9"), "got: {reason}");
}

#[test]
fn a_guest_that_never_returns_runs_out_of_fuel() {
	// (func (loop br 0 end)) - an unbounded loop inside the interpreter with nothing that ended it.
	// RAN BEFORE, forever: a component host in a capability system may not have "the guest can
	// decide never to return" among its outcomes.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("an infinite loop is well typed - it is a run-time bound, not a static one");
	let mut inst: Instance = Instance::new(&m).expect("and it instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("out of fuel: the guest ran for longer than the host allows")));
}

#[test]
fn unbounded_guest_recursion_traps_rather_than_overflowing_the_host() {
	// (func $f (call $f)) - RAN BEFORE as Rust recursion, so a deeply recursive module overflowed
	// the HOST's stack. A guest may not decide to crash its host.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x10, 0x00, 0x0b])] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("infinite recursion is well typed");
	let mut inst: Instance = Instance::new(&m).expect("and it instantiates");
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("call depth limit reached")));
}

#[test]
fn the_narrow_i64_stores_take_an_i64() {
	// The decoder folds all seven integer stores into one `Store`, and the validator typed the value
	// by the STORAGE WIDTH: `if width == 8 { I64 } else { I32 }`. `i64.store8`, `i64.store16` and
	// `i64.store32` all take an `i64` and narrow it on the way to memory, so all three were refused
	// when written correctly and accepted when given an `i32` - the validator wrong in both
	// directions at once, which is worse than not checking.
	//
	// Six cases: each narrow `i64` store correct and incorrect. Three of them failed before.
	for (label, opcode, correct_is_i64) in [
		("i64.store8", 0x3cu8, true),
		("i64.store16", 0x3d, true),
		("i64.store32", 0x3e, true),
		("i32.store8", 0x3a, false),
		("i32.store16", 0x3b, false),
		("i32.store", 0x36, false),
	] {
		for value_is_i64 in [false, true] {
			let mut body: Vec<u8> = Vec::new();
			body.push(0x41); // i32.const 0 - the address
			body.extend_from_slice(&sleb(0));
			if value_is_i64 {
				body.push(0x42); // i64.const 1
				body.extend_from_slice(&sleb(1));
			} else {
				body.push(0x41); // i32.const 1
				body.extend_from_slice(&sleb(1));
			}
			body.push(opcode);
			body.extend_from_slice(&leb(0)); // align
			body.extend_from_slice(&leb(0)); // offset
			body.push(0x0b);
			let spec = Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] };
			let module = parse(&build(&spec)).expect("parses");
			let validated = validate(module);
			assert_eq!(validated.is_ok(), value_is_i64 == correct_is_i64, "{label} with an {} value: validation said {:?}", if value_is_i64 { "i64" } else { "i32" }, validated.err());
		}
	}
}

#[test]
fn a_memory_of_zero_pages_is_a_memory() {
	// `memory_min_pages` was documented as "0 if module declares none" and existence was derived
	// from `min > 0 || max.is_some()` - so `(memory 0)` with a `memory.size` was told it had no
	// memory, while `(memory 0 10)` worked by accident because the maximum made the disjunction
	// true. A sentinel cannot express a legal value that equals it.
	//
	// Built by hand, because `Spec`'s `mem_pages: 0` means "emit no memory section" - the same
	// sentinel one layer up, in the fixture.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x01, 0x7f]));
	wasm.extend_from_slice(&section(3, &[1, 0]));
	wasm.extend_from_slice(&section(5, &[1, 0x00, 0x00]));
	let mut exports: Vec<u8> = alloc::vec![1, 3];
	exports.extend_from_slice(b"run");
	exports.extend_from_slice(&[0x00, 0]);
	wasm.extend_from_slice(&section(7, &exports));
	wasm.extend_from_slice(&section(10, &[1, 4, 0x00, 0x3f, 0x00, 0x0b]));

	let module = parse(&wasm).expect("parses");
	assert_eq!(module.memory, Some(crate::module::MemoryType { min_pages: 0, max_pages: None }), "a declared memory of zero pages is a declared memory");
	let validated = validate(module).expect("a memory instruction in a module that declares a memory");
	let mut instance = Instance::new(&validated).expect("instantiates");
	let mut host = NoHost;
	assert_eq!(instance.invoke("run", &[], &mut host).expect("runs"), alloc::vec![Value::I32(0)], "and it has zero pages");
}

#[test]
fn a_module_declaring_more_memory_than_the_host_allows_is_refused() {
	// `MAX_MEMORY_PAGES` appeared at its definition and in `memory.grow` and nowhere else, so a
	// six-byte declaration - `(memory 100000)` - asked the host for six gigabytes before an
	// instruction ran. The milestone's own policy is that a limit the module declares is not a
	// limit the host imposed; here the module's declaration WAS the limit.
	let over = (crate::interp::MAX_MEMORY_PAGES + 1) as u32;
	let mut memory: Vec<u8> = alloc::vec![1, 0x00];
	memory.extend_from_slice(&leb(over));
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[1, 0]));
	wasm.extend_from_slice(&section(5, &memory));
	let mut exports: Vec<u8> = alloc::vec![1, 3];
	exports.extend_from_slice(b"run");
	exports.extend_from_slice(&[0x00, 0]);
	wasm.extend_from_slice(&section(7, &exports));
	wasm.extend_from_slice(&section(10, &[1, 2, 0x00, 0x0b]));

	let module = parse(&wasm).expect("a declaration is not a parse error");
	let error = validate(module).expect_err("but it is a validation error");
	assert!(format!("{error:?}").contains("host limit"), "named for the rule: {error:?}");
}

#[test]
fn a_typed_select_is_checked_against_the_type_it_declares() {
	// The annotation was read and thrown away, so the validator could only require the two operands
	// to agree with each other - never with the type the instruction states. An annotation asking
	// for an `i64` over two `i32`s was accepted, and the declaration checked against nothing.
	for (label, annotation, operands_i64, ok) in [("i32 over i32s", 0x7fu8, false, true), ("i64 over i64s", 0x7e, true, true), ("i64 over i32s", 0x7e, false, false), ("i32 over i64s", 0x7f, true, false)] {
		let mut body: Vec<u8> = alloc::vec![0x00]; // no locals
		for _ in 0..2 {
			if operands_i64 {
				body.push(0x42);
				body.extend_from_slice(&sleb(1));
			} else {
				body.push(0x41);
				body.extend_from_slice(&sleb(1));
			}
		}
		body.push(0x41); // the condition
		body.extend_from_slice(&sleb(0));
		body.extend_from_slice(&[0x1c, 0x01, annotation]); // select with one declared type
		body.push(0x1a); // drop it - the function returns nothing
		body.push(0x0b);
		let mut wasm: Vec<u8> = alloc::vec![];
		wasm.extend_from_slice(b"\0asm");
		wasm.extend_from_slice(&[1, 0, 0, 0]);
		wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
		wasm.extend_from_slice(&section(3, &[1, 0]));
		let mut exports: Vec<u8> = alloc::vec![1, 3];
		exports.extend_from_slice(b"run");
		exports.extend_from_slice(&[0x00, 0]);
		wasm.extend_from_slice(&section(7, &exports));
		let mut code: Vec<u8> = alloc::vec![1];
		code.extend_from_slice(&leb(body.len() as u32));
		code.extend_from_slice(&body);
		wasm.extend_from_slice(&section(10, &code));
		let parsed = parse(&wasm).expect("parses");
		assert_eq!(validate(parsed).is_ok(), ok, "{label}");
	}
}

#[test]
fn a_declared_count_is_bounded_by_the_input_before_it_is_allocated() {
	// A few bytes of body declaring `0xffffffff` `br_table` labels asked for sixteen gigabytes, in
	// an INFALLIBLE `Vec::with_capacity` - so the failure was an abort in the host process rather
	// than a refusal. And it happened during decode, which is before fuel, before the host limits
	// and before everything else the engine bounds.
	//
	// Two changes, and this test proves the second: `try_reserve` makes the failure a REFUSAL rather
	// than an abort, and the input bound - a label is at least one byte, so `n` of them cannot be
	// present in fewer than `n` bytes of what is left - makes the refusal arrive without asking the
	// allocator for sixteen gigabytes first.
	//
	// Said plainly because the test cannot tell them apart: with the bound removed the `try_reserve`
	// still refuses and this still passes. What it pins is that a hostile count is answered rather
	// than aborted on; the bound above it is the cheaper path to the same answer.
	let mut body: Vec<u8> = alloc::vec![0x00, 0x02, 0x40, 0x41, 0x00, 0x0e]; // block; i32.const 0; br_table
	body.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f]); // count = 0xffffffff
	body.extend_from_slice(&[0x00, 0x0b, 0x0b]);
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[1, 0]));
	let mut exports: Vec<u8> = alloc::vec![1, 3];
	exports.extend_from_slice(b"run");
	exports.extend_from_slice(&[0x00, 0]);
	wasm.extend_from_slice(&section(7, &exports));
	let mut code: Vec<u8> = alloc::vec![1];
	code.extend_from_slice(&leb(body.len() as u32));
	code.extend_from_slice(&body);
	wasm.extend_from_slice(&section(10, &code));
	let parsed = parse(&wasm).expect("the count is a body matter, not a section one");
	assert!(validate(parsed).is_err(), "a count the body cannot hold is refused rather than allocated");
}

#[test]
fn call_indirect_naming_another_table_is_refused() {
	// The table index was read and DISCARDED, and the validator checked only that some table exists
	// - so a module could name table 1 and have the call executed against table 0. This engine
	// supports one table, so the rule is that the index is zero.
	for (label, table, ok) in [("table 0", 0u8, true), ("table 1", 1, false)] {
		let body: Vec<u8> = alloc::vec![0x00, 0x41, 0x00, 0x11, 0x00, table, 0x0b];
		let mut wasm: Vec<u8> = alloc::vec![];
		wasm.extend_from_slice(b"\0asm");
		wasm.extend_from_slice(&[1, 0, 0, 0]);
		wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
		wasm.extend_from_slice(&section(3, &[1, 0]));
		wasm.extend_from_slice(&section(4, &[1, 0x70, 0x00, 0x01])); // one funcref table, min 1
		let mut exports: Vec<u8> = alloc::vec![1, 3];
		exports.extend_from_slice(b"run");
		exports.extend_from_slice(&[0x00, 0]);
		wasm.extend_from_slice(&section(7, &exports));
		let mut code: Vec<u8> = alloc::vec![1];
		code.extend_from_slice(&leb(body.len() as u32));
		code.extend_from_slice(&body);
		wasm.extend_from_slice(&section(10, &code));
		let parsed = parse(&wasm).expect("parses");
		assert_eq!(validate(parsed).is_ok(), ok, "{label}");
	}
}

#[test]
fn the_body_decoder_bounds_a_u32_to_thirty_two_bits() {
	// The parser's reader refused a fifth byte carrying bits above the 32nd and the BODY decoder had
	// no width check at all - it read the fifth byte at shift 28 and silently dropped the rest. So
	// the two readers in this crate disagreed about what a `u32` is, and the permissive one is the
	// one used for instruction immediates.
	let mut body: Vec<u8> = alloc::vec![0x00, 0x20]; // no locals; local.get
	body.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x70]); // an index with bits above 32
	body.push(0x0b);
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[1, 0]));
	let mut exports: Vec<u8> = alloc::vec![1, 3];
	exports.extend_from_slice(b"run");
	exports.extend_from_slice(&[0x00, 0]);
	wasm.extend_from_slice(&section(7, &exports));
	let mut code: Vec<u8> = alloc::vec![1];
	code.extend_from_slice(&leb(body.len() as u32));
	code.extend_from_slice(&body);
	wasm.extend_from_slice(&section(10, &code));
	let parsed = parse(&wasm).expect("the section parses; the body is the decoder's business");
	let error = validate(parsed).expect_err("an over-wide immediate is refused");
	assert!(format!("{error:?}").contains("32 bits"), "named for the rule: {error:?}");
}

#[test]
fn a_body_that_pushes_without_popping_is_bounded() {
	// The per-instance limits item named a "maximum operand stack depth" and the DONE note listed
	// four constants, none of which is one. Call depth was bounded, which covers recursion; a single
	// body pushing without popping was not, and it grows both the validator's `Vec<Type>` and the
	// interpreter's `Vec<Value>` with it.
	let mut body: Vec<u8> = alloc::vec![0x00]; // no locals
	for _ in 0..(crate::validate::MAX_STACK_DEPTH + 8) {
		body.push(0x41);
		body.extend_from_slice(&sleb(1));
	}
	body.push(0x0b);
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[1, 0]));
	let mut exports: Vec<u8> = alloc::vec![1, 3];
	exports.extend_from_slice(b"run");
	exports.extend_from_slice(&[0x00, 0]);
	wasm.extend_from_slice(&section(7, &exports));
	let mut code: Vec<u8> = alloc::vec![1];
	code.extend_from_slice(&leb(body.len() as u32));
	code.extend_from_slice(&body);
	wasm.extend_from_slice(&section(10, &code));
	let parsed = parse(&wasm).expect("parses");
	let error = validate(parsed).expect_err("a body past the depth ceiling is refused");
	assert!(format!("{error:?}").contains("operand stack"), "named for the rule: {error:?}");
}

#[test]
fn the_embedder_boundary_checks_types_and_not_only_counts() {
	// Validation proved the BODY consistent with its declared signature. A host calling with the
	// wrong types is the caller breaking that signature from outside, which validation cannot see -
	// and only the COUNT was checked, so an `F64` sat in a local the body reads as an `i32` and
	// `Value::as_i32` converted it rather than reporting anything.
	let body: Vec<u8> = alloc::vec![0x00, 0x20, 0x00, 0x0b]; // local.get 0; end
	let spec = Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body[1..])] };
	let module = parse(&build(&spec)).expect("parses");
	let validated = validate(module).expect("validates");
	let mut instance = Instance::new(&validated).expect("instantiates");
	let mut host = NoHost;
	assert_eq!(instance.invoke("run", &[Value::I32(7)], &mut host).expect("the declared type runs"), alloc::vec![Value::I32(7)]);
	assert!(instance.invoke("run", &[Value::F64(1.0)], &mut host).is_err(), "an argument of the wrong type is refused rather than converted");
	assert!(instance.invoke("run", &[Value::I64(7)], &mut host).is_err(), "and so is one of a different integer width");
}

#[test]
fn a_block_is_checked_against_the_type_it_declares() {
	// The decoder recorded ARITIES, so everything else about a block type was gone by the time
	// validation ran. `block_signature` then reconstructed one: `i32` for a single result, and for
	// anything larger the FIRST declared type with matching counts. Both directions wrong at once -
	// `(block (result i64) i64.const 7)` refused, `(block (result i64) i32.const 7)` accepted - and a
	// block naming type 3 got type 0 if type 0 had the same shape.
	//
	// Three shapes, and the third is the one no arity-based check can pass.
	// The FUNCTION's type is always the last one, so the earlier ones exist only to be named by a
	// block - which is what makes "the first with matching counts" a wrong answer rather than an
	// accidentally right one.
	fn module_with(body: Vec<u8>, types: &[(&[u8], &[u8])]) -> Vec<u8> {
		let last = (types.len() - 1) as u32;
		let spec = Spec { types, imports: &[], funcs: &[last], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &body)] };
		build(&spec)
	}

	// (block (result i64) i64.const 7 end) drop - correct, and it must pass.
	let ok: Vec<u8> = alloc::vec![0x02, 0x7e, 0x42, 0x07, 0x0b, 0x1a, 0x0b];
	assert!(validate(parse(&module_with(ok, &[(&[], &[])])).expect("parses")).is_ok(), "a declared i64 result filled with an i64");

	// The same block filled with an i32 - and it must NOT.
	let bad: Vec<u8> = alloc::vec![0x02, 0x7e, 0x41, 0x07, 0x0b, 0x1a, 0x0b];
	assert!(validate(parse(&module_with(bad, &[(&[], &[])])).expect("parses")).is_err(), "a declared i64 result filled with an i32");

	// A block naming TYPE 1 while type 0 has the same arity: the named one is what counts. Type 2 is
	// the function's own, so both (0,1) types are there only to be chosen between.
	let named: Vec<u8> = alloc::vec![0x02, 0x01, 0x42, 0x07, 0x0b, 0x1a, 0x0b];
	let types: &[(&[u8], &[u8])] = &[(&[], &[I32]), (&[], &[I64]), (&[], &[])];
	assert!(validate(parse(&module_with(named, types)).expect("parses")).is_ok(), "the block's own type index is used, not the first with matching counts");
	// And the same block pushing an i32, which type 0 would have accepted.
	let named_bad: Vec<u8> = alloc::vec![0x02, 0x01, 0x41, 0x07, 0x0b, 0x1a, 0x0b];
	assert!(validate(parse(&module_with(named_bad, types)).expect("parses")).is_err(), "type 0's shape does not stand in for type 1's");
}

// The WebAssembly specification's own binary-form cases, as an OUTSIDE authority.
//
// Everything else in this file is hand-written, and a hand-written corpus can prove a rule is
// ENFORCED and cannot prove the rule is RIGHT - the same reading of the specification writes both
// the rule and the test. That is not hypothetical here: `a_non_canonical_leb128_is_refused` was
// well-built, watched failing, and pinning behaviour the specification contradicts. It took an
// outside reading to notice, which is exactly the cost this test exists to remove.
//
// `src/wasm/tests/spec-binary-cases.tsv` is extracted by `src/tools/extract-wasm-spec-cases.py` from
// the specification's `test/core` suite: every case written as `(module binary "...")` - raw bytes
// with a stated outcome - which is most of what the binary-format and validation suites are made of.
// The `.wast` text-format modules are not covered; parsing that language is a project of its own.
mod spec {
	use super::*;

	struct Case<'a> {
		kind: &'a str,
		file: &'a str,
		bytes: Vec<u8>,
		reason: &'a str,
	}

	fn hex(b: &[u8]) -> alloc::string::String {
		b.iter().map(|x| alloc::format!("{x:02x}")).collect()
	}

	fn cases() -> Vec<Case<'static>> {
		const TSV: &str = include_str!("../tests/spec-binary-cases.tsv");
		let mut out: Vec<Case<'static>> = Vec::new();
		for line in TSV.lines() {
			let mut parts = line.splitn(4, '\t');
			let (Some(kind), Some(file), Some(hex)) = (parts.next(), parts.next(), parts.next()) else { continue };
			let reason = parts.next().unwrap_or("");
			let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex")).collect();
			out.push(Case { kind, file, bytes, reason });
		}
		out
	}

	#[test]
	fn the_specifications_malformed_modules_are_refused_and_the_gap_is_measured() {
		// A module the specification calls MALFORMED must not parse. This engine implements a
		// subset, so refusing MORE than the specification requires is allowed - which makes this the
		// direction that can be held in full, and the number below is what "in full" costs today.
		//
		// MEASURED, 2026-08-12: 205 of 707 accepted when this suite was first pointed at the engine,
		// and the breakdown named the fixes. Two of them closed 180 cases between them:
		//
		//   176  malformed UTF-8         - custom-section NAMES were never read. The section was
		//                                  skipped by its declared size, so the name a module must
		//                                  encode as UTF-8 was never looked at.
		//     8  integer too large       - the signed reader bounded its WIDTH and not its final
		//                                  byte's unused bits, so an over-wide `s33`/`s64` decoded.
		//                                  Writing that check found its own defect immediately: read
		//                                  from the running VALUE it refused `i64.const -1` written
		//                                  in full, which the suite calls well-formed.
		//     1  malformed limits flags  - a flags byte above 1 is a feature this engine does not
		//                                  have, read as though bit 0 were the only one that counts.
		//
		// RE-MEASURED, 2026-08-14: thirteen. Seven more closed when the signed LEB readers were split
		// by width - `s32` for `i32.const`, `s33` for a block type, `s64` for `i64.const`, each
		// bounded by its own length and its own last-byte rule - and one when the table parser was
		// given the flags rule the memory parser already stated.
		//
		// Thirteen remain, and they are named rather than hidden: two memop flags, an END-opcode
		// case, an illegal-opcode one, and integer-width forms this parser reaches by other paths.
		// Each is a real conformance gap and none of them is a way to run something dangerous - see
		// the assertion below, which is the one that says so rather than assuming it.
		//
		// The ceiling is a RATCHET: it may go down and must not go up, so a change that starts
		// accepting a module the specification calls malformed has to move this number and say why.
		const ACCEPTED_CEILING: usize = 13;
		let all = cases();
		let malformed: Vec<&Case<'_>> = all.iter().filter(|c| c.kind == "malformed").collect();
		assert!(malformed.len() > 600, "only {} malformed cases were extracted - the fixture is not what it was", malformed.len());
		let accepted: Vec<&&Case<'_>> = malformed.iter().filter(|c| parse(&c.bytes).is_ok()).collect();
		assert!(accepted.len() <= ACCEPTED_CEILING, "{} of {} specification-malformed modules parse, past the recorded {ACCEPTED_CEILING}: {:?}", accepted.len(), malformed.len(), accepted.iter().map(|c| (c.file, c.reason)).take(8).collect::<Vec<_>>());

		// AND NONE OF THEM REACHES AN INSTANCE, which is the claim the ceiling above only gestures
		// at. `parse` does not decode function bodies - it stores them - so a malformed body is
		// invisible to that count and the thirteen above are a statement about section structure
		// alone. The layer that decides behaviour is `parse -> validate -> Instance::new`, and this
		// is an ASSERTION rather than a ratchet: zero, and it may not become anything else.
		let reached: Vec<&&Case<'_>> = malformed.iter().filter(|c| instantiates(&c.bytes)).collect();
		assert!(reached.is_empty(), "{} specification-malformed modules reach a running instance: {:?}", reached.len(), reached.iter().map(|c| (c.file, c.reason)).take(8).collect::<Vec<_>>());
	}

	// Does this module get all the way to something that can be executed?
	//
	// The corpus assertions were written over `parse` alone, which is the first rung and was the
	// only one: a module that parses, validates, instantiates and then computes the wrong answer
	// passed every one of them. This engine's own worst defect to date was exactly such a module.
	fn instantiates(bytes: &[u8]) -> bool {
		parse(bytes).ok().and_then(|m| validate(m).ok()).is_some_and(|v| crate::interp::Instance::new(&v).is_ok())
	}

	#[test]
	fn every_invalid_module_the_specification_names_fails_validation() {
		// A module the specification calls INVALID is well-formed and must not validate. Parsing it
		// is allowed to fail too - this engine's parser refuses some shapes the specification defers
		// to validation - so what is asserted is that it never reaches a `ValidatedModule`.
		let all = cases();
		let mut checked = 0u32;
		for case in all.iter().filter(|c| c.kind == "invalid") {
			let reached = parse(&case.bytes).map(validate).is_ok_and(|v| v.is_ok());
			assert!(!reached, "{}: the specification calls this invalid ({}), and it validated", case.file, case.reason);
			checked += 1;
		}
		assert!(checked >= 11, "only {checked} invalid cases were extracted - the fixture is not what it was");
	}

	#[test]
	fn the_specifications_valid_modules_are_accepted_or_refused_for_a_stated_reason() {
		// The other direction, as a RATCHET rather than an assertion.
		//
		// A module the specification calls valid may still be refused here, because this engine is a
		// subset - multi-memory, reference types, SIMD and the rest are all legitimately absent. So
		// "every valid module parses" is not true and should not be asserted.
		//
		// What can be held is the COUNT: the number this engine accepts today, which may go up and
		// must not go down. A change that starts refusing a module the specification calls valid has
		// to move this number and say why.
		//
		// RE-MEASURED, 2026-08-14: 55 of 76, raised from a floor of 40 that no longer described the
		// engine. A ratchet left below what is actually true stops being a ratchet - it approves
		// fifteen regressions before it notices one.
		let all = cases();
		let valid: Vec<&Case<'_>> = all.iter().filter(|c| c.kind == "valid").collect();
		let accepted = valid.iter().filter(|c| parse(&c.bytes).is_ok()).count();
		assert!(valid.len() >= 70, "only {} valid cases were extracted - the fixture is not what it was", valid.len());
		assert!(accepted >= 55, "this engine accepts {accepted} of {} specification-valid modules, down from 55 - a subset got smaller and the reason belongs here", valid.len());

		// AND EVERY ONE OF THEM GETS TO AN INSTANCE. The count above is about the parser; a module
		// the specification calls valid that parses and is then refused by this engine's validator
		// or instantiation is a subset boundary somewhere nobody wrote down. Measured 2026-08-14: 55
		// of 55, so the two numbers are the same number and this asserts they stay that way.
		let instantiated = valid.iter().filter(|c| instantiates(&c.bytes)).count();
		assert_eq!(instantiated, accepted, "{accepted} specification-valid modules parse and {instantiated} reach an instance - the gap is a refusal past the parser that nothing names");
	}

	#[test]
	fn the_specification_agrees_that_a_trailing_zero_leb128_is_well_formed() {
		// THE CASE THAT STARTED THIS. `binary-leb128.wast` opens with "Unsigned LEB128 can have
		// non-minimal length" and a bare module whose memory minimum is written `\82\00` - two bytes
		// for a value that fits in one. This parser refused exactly that, and a test held it there.
		//
		// Named separately from the sweep above because it is the specific claim, and a count that
		// happens to pass is not the same as the claim being checked.
		let all = cases();
		let from_leb = all.iter().filter(|c| c.file == "binary-leb128.wast" && c.kind == "valid").count();
		assert!(from_leb >= 10, "the LEB128 file contributed {from_leb} valid modules");
		let refused: Vec<&str> = all.iter().filter(|c| c.file == "binary-leb128.wast" && c.kind == "valid" && parse(&c.bytes).is_err()).map(|c| c.reason).collect();
		let detail: Vec<String> = all.iter().filter(|c| c.file == "binary-leb128.wast" && c.kind == "valid").filter_map(|c| parse(&c.bytes).err().map(|e| alloc::format!("{:?} over {}", e, hex(&c.bytes)))).collect();
		assert!(refused.is_empty(), "{} of the LEB128 file's valid modules are refused: {detail:?}", refused.len());
	}
}

// The specification's own EXECUTABLE assertions: what a module returns, not whether it is accepted.
//
// `spec` above answers "does this engine agree about which modules are well-formed", which is the
// first rung and was for a long time the only one - and a module that parses, validates,
// instantiates and then computes the WRONG ANSWER passes every assertion in it. That is not a
// hypothetical: this engine recorded a parameterised block's label below its parameters in the
// validator and above them in the interpreter, so `(block (param i32) (result i32) ... br 0)`
// returned 50 where the specification says 40, with the whole binary corpus green.
//
// `src/wasm/tests/spec-run-cases.tsv` is produced by `src/tools/extract-wasm-spec-runs.py` from
// `spec/test/core`, via `wasm-tools json-from-wast` - the text format is a project to parse and this
// does not parse it. Integer assertions only; the float cases carry comparison rules
// (`nan:canonical`, `nan:arithmetic`) that a flattened bit pattern would misstate. Regenerated
// 2026-08-14: 150 modules, 2692 assertions.
#[cfg(test)]
mod spec_run {
	use super::*;
	use alloc::collections::BTreeMap;

	struct Run<'a> {
		module: usize,
		file: &'a str,
		export: &'a str,
		args: &'a str,
		expected: &'a str,
	}

	const TSV: &str = include_str!("../tests/spec-run-cases.tsv");

	fn fixture() -> (BTreeMap<usize, Vec<u8>>, Vec<Run<'static>>) {
		let mut modules: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
		let mut runs: Vec<Run<'static>> = Vec::new();
		for line in TSV.lines() {
			let f: Vec<&str> = line.split('\t').collect();
			match f.first() {
				Some(&"M") if f.len() == 3 => {
					let index: usize = f[1].parse().expect("module index");
					let hex = f[2];
					modules.insert(index, (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex")).collect());
				}
				Some(&"R") if f.len() == 6 => {
					runs.push(Run { module: f[1].parse().expect("module index"), file: f[2], export: f[3], args: f[4], expected: f[5] });
				}
				_ => {}
			}
		}
		(modules, runs)
	}

	// `i32:4294967295` - the type and the UNSIGNED bit pattern, which is how the specification's own
	// JSON writes it. Reinterpreting as signed is this reader's job precisely so the fixture does not
	// have to decide.
	fn parse_values(list: &str) -> Option<Vec<Value>> {
		if list.is_empty() {
			return Some(Vec::new());
		}
		list.split(',')
			.map(|item| {
				let (ty, raw) = item.split_once(':')?;
				match ty {
					"i32" => Some(Value::I32(raw.parse::<u32>().ok()? as i32)),
					"i64" => Some(Value::I64(raw.parse::<u64>().ok()? as i64)),
					_ => None,
				}
			})
			.collect()
	}

	// Bit-pattern equality, and the TYPE as well as the bits: `Value::as_i32` converts every numeric
	// type, so comparing through it would call an `i64` result equal to the `i32` the specification
	// asked for.
	fn same(got: &[Value], want: &[Value]) -> bool {
		got.len() == want.len()
			&& got.iter().zip(want).all(|(a, b)| match (a, b) {
				(Value::I32(x), Value::I32(y)) => x == y,
				(Value::I64(x), Value::I64(y)) => x == y,
				_ => false,
			})
	}

	// A trap this ENGINE imposes rather than one WebAssembly defines. The specification models no
	// fuel and no call-depth ceiling, so a case that hits either has not disagreed with it - and
	// every other trap message in the interpreter is a rule the specification does state.
	fn host_limit(trap: &Trap) -> bool {
		trap.0.contains("out of fuel") || trap.0.contains("call depth")
	}

	// Does this engine's trap say what the specification's expected message says?
	//
	// The specification's texts are short phrases ("integer divide by zero", "out of bounds memory
	// access") and this engine writes its own sentences, so an exact comparison would fail on every
	// case. Matching on the phrase being PRESENT is the honest middle: it catches a module that traps
	// for a completely different reason, which is what the audit found, without demanding that two
	// independently-written wordings agree character for character.
	fn trap_says(trap: &Trap, want: &str) -> bool {
		trap.0.contains(want)
	}
	#[test]
	fn the_specifications_own_answers_are_this_engines_answers() {
		let (modules, runs) = fixture();
		assert!(modules.len() >= 140, "only {} modules in the run fixture - it is not what it was", modules.len());
		assert!(runs.len() >= 2700, "only {} assertions in the run fixture - it is not what it was", runs.len());

		// A module this engine refuses is a SUBSET boundary and not a failure: reference types, SIMD,
		// multi-memory and the rest are legitimately absent, and their assertions cannot be run. What
		// may not happen is a module that runs and disagrees.
		let mut ran = 0usize;
		let mut skipped = 0usize;
		let mut wrong: Vec<alloc::string::String> = Vec::new();
		// GROUPED BY MODULE, because an `Instance` borrows the `ValidatedModule` it came from - and
		// because instantiating once per assertion would re-run every data segment 2692 times.
		let mut by_module: BTreeMap<usize, Vec<&Run<'_>>> = BTreeMap::new();
		for run in &runs {
			by_module.entry(run.module).or_default().push(run);
		}
		for (index, group) in &by_module {
			let bytes = modules.get(index).expect("every run names a module in the fixture");
			let Some(validated) = parse(bytes).ok().and_then(|m| validate(m).ok()) else {
				skipped += group.len();
				continue;
			};
			// A MODULE THIS HARNESS CANNOT SERVE, decided about the MODULE.
			//
			// The suite's modules import `print_i32` and friends from its `spectest` host, and this
			// runner supplies `NoHost` - which traps every import with "no imports available". That
			// is a property of the fixture, knowable before a single assertion runs, and leaving it
			// to be discovered per result is what let a genuine trap hide among skips. Whether a
			// module can be run is a module question; whether its answer is right is not.
			if !validated.module().imports.is_empty() {
				skipped += group.len();
				continue;
			}
			let Ok(mut instance) = crate::interp::Instance::new(&validated) else {
				skipped += group.len();
				continue;
			};
			for run in group {
				let Some(args) = parse_values(run.args) else {
					skipped += 1;
					continue;
				};
				// A FRESH START PER ASSERTION would need re-instantiation, and the suite's
				// assertions are written to be independent of each other on one instance - which is
				// what the reference interpreter does too.
				let outcome = instance.invoke(run.export, &args, &mut NoHost);
				// A bare `(invoke ...)` from the suite: no assertion, replayed for its EFFECT. The
				// assertions after it are written against the state it leaves - `float_memory.wast`
				// resets its memory before every check, and dropping those made thirteen of its
				// loads read a memory the specification had cleared.
				if run.expected == "effect" {
					// AND A TRAP IN ONE IS A FAILURE OF THE CASE, not something to discard.
					//
					// The result was thrown away and so was the trap, so a module that trapped
					// part-way through was replayed as though its state change had SUCCEEDED - and
					// the assertions after it are written against the state it was supposed to
					// leave. That is the same class as the `assert_return` false green this corpus
					// was built to catch, in the other direction.
					//
					// A host limit is still not a disagreement with the specification, which is the
					// one exception the value path already makes for the same reason.
					match outcome {
						Ok(_) => ran += 1,
						Err(trap) if host_limit(&trap) => skipped += 1,
						Err(trap) => {
							ran += 1;
							wrong.push(alloc::format!("{}: {}({}) trapped with {trap:?} while being replayed for its effect, so every assertion after it is written against a state that was never reached", run.file, run.export, run.args));
						}
					}
					continue;
				}
				// A TRAP CASE, with or without the message the specification names. `trap` is the
				// legacy shape - the fixture on disk was extracted before the message was captured -
				// and `trap:<text>` is what the extractor writes now. When the text is there it is
				// CHECKED, because otherwise every trap satisfies every trap assertion and a module
				// trapping with "out of bounds memory access" passes a case asserting "integer
				// divide by zero".
				if run.expected == "trap" || run.expected.starts_with("trap:") {
					ran += 1;
					match outcome {
						Ok(value) => wrong.push(alloc::format!("{}: {}({}) returned {value:?} where the specification says it traps", run.file, run.export, run.args)),
						// A HOST-LIMIT TRAP IS NOT THE TRAP THE CASE NAMES.
						//
						// Any `Err` satisfied this, so a module that hit `MAX_STACK_DEPTH`, ran out
						// of fuel or was refused by a host limit passed a case about integer
						// division by zero - the assertion held for a reason that has nothing to do
						// with what it is testing. Fuel and call depth are this engine's policy and
						// the specification does not model them, so such a case has neither agreed
						// nor disagreed: it is counted apart, exactly as the value path counts it.
						Err(trap) if host_limit(&trap) => {
							ran -= 1;
							skipped += 1;
						}
						Err(trap) => {
							// The message, WHEN THE FIXTURE CARRIES ONE. `trap:<text>` cases compare
							// it; bare `trap` cases accept any trap, which is what the fixture on
							// disk still is until it is regenerated with a specification checkout.
							if let Some(want) = run.expected.strip_prefix("trap:")
								&& !want.is_empty() && !trap_says(&trap, want)
							{
								wrong.push(alloc::format!("{}: {}({}) trapped with {trap:?} where the specification says {want:?}", run.file, run.export, run.args));
							}
						}
					}
					continue;
				}
				let Some(want) = parse_values(run.expected) else {
					skipped += 1;
					continue;
				};
				match outcome {
					// A TRAP WHERE THE SPECIFICATION SAYS A VALUE IS A WRONG ANSWER, not a skip.
					//
					// This counted every `Err` as outside the subset, on the reasoning that an
					// unsupported instruction in the body cannot be run. That reasoning describes a
					// module, and the module is past it: `validate` decodes every body and refuses
					// `unsupported opcode`, so a module that instantiated has no unreachable
					// instruction left in it. What arrived here instead was the interpreter
					// returning a trap where the specification says it returns a number - which is
					// the exact class of defect the execution corpus was built to catch, filed as
					// "skipped" and leaving the suite green.
					//
					// The two host limits are the real exception and are counted apart rather than
					// folded in: fuel and call depth are this engine's policy, they are not modelled
					// by the specification at all, and a case that hits one has not disagreed with
					// anything.
					Err(trap) if host_limit(&trap) => skipped += 1,
					Err(trap) => {
						ran += 1;
						wrong.push(alloc::format!("{}: {}({}) trapped with {trap:?}, and the specification says {want:?}", run.file, run.export, run.args));
					}
					Ok(got) => {
						ran += 1;
						if !same(&got, &want) {
							wrong.push(alloc::format!("{}: {}({}) = {got:?}, and the specification says {want:?}", run.file, run.export, run.args));
						}
					}
				}
			}
		}

		assert!(wrong.is_empty(), "{} of {ran} executed specification assertions disagree with this engine: {:?}", wrong.len(), wrong.iter().take(12).collect::<Vec<_>>());
		// THE FLOOR, so "everything was skipped" cannot pass as "nothing disagreed". A ratchet: it
		// may go up and must not go down. Measured 2026-08-14: 2137 executed, 562 skipped as outside this
		// engine's subset.
		//
		// 2026-08-15: 2133 executed, 567 skipped. The four that moved belong to modules importing the
		// suite's `spectest` host, which this runner does not provide - and two of the four were
		// TRAPPING, counted as "skipped" by the arm that read every `Err` as an unsupported
		// instruction. They are skipped for a reason stated about the module now rather than
		// discovered per result, which is the whole point of the change; the count went down by four
		// because four of them were never evidence about this engine.
		//
		// Providing a `spectest` host would win them back and little else: those modules also import
		// globals, tables and memories, which this engine does not support at all, so they are
		// refused before instantiation whatever host is passed.
		//
		// 2026-08-19: 2160 executed, 567 skipped, out of 2727 in the fixture. The floor had been
		// sitting at 2133 since the note above while the fixture had grown; nothing executed fewer
		// assertions, the ratchet had simply not been raised when the number moved. It is the
		// measured number again now, which is the only setting at which it guards anything.
		assert!(ran >= 2160, "only {ran} of {} specification assertions were executed ({skipped} skipped) - a subset got smaller and the reason belongs here", runs.len());
	}
}

#[test]
fn a_parameterised_block_leaves_its_label_below_its_parameters() {
	// THE DEFECT THAT NEEDED THIS WHOLE ROUND, stated as the one module that distinguishes the two
	// answers. Everything the corpus tests had asked until now was "is this module accepted"; this
	// one is accepted either way and the two engines return different numbers.
	//
	//   (type $t (func (param i32) (result i32)))
	//   (func (result i32)
	//     i32.const 10
	//     i32.const 20
	//     block (type $t)     ;; consumes 20; its label sits at height 1
	//       i32.const 30
	//       br 0              ;; carries 30 out, truncating to the label's height
	//     end
	//     i32.add)            ;; 10 + 30
	//
	// Correct: `[10, 30]`, and the function answers 40. With the label recorded ABOVE the parameters
	// - `base: stack.len()` before the block's own operands are accounted for - the truncation leaves
	// the 20 behind, the stack is `[10, 20, 30]`, and `i32.add` answers 50.
	//
	// `Loop` had the arithmetic right and `Block` and `If` did not, which is the shape of the whole
	// finding: the validator was taught what block parameters mean and the interpreter was not.
	let body: &[u8] = &[
		0x41,
		10, // i32.const 10
		0x41,
		20, // i32.const 20
		0x02,
		0x00, // block (type 0)
		0x41,
		30, // i32.const 30
		0x0c,
		0x00, // br 0
		0x0b, // end
		0x6a, // i32.add
		0x0b, // end
	];
	let spec = Spec { types: &[(&[I32], &[I32]), (&[], &[I32])], imports: &[], funcs: &[1], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] };
	let module = parse(&build(&spec)).expect("a parameterised block parses");
	let validated = validate(module).expect("and validates - the validator has always handled this");
	let mut instance = Instance::new(&validated).expect("and instantiates");
	let out = instance.invoke("run", &[], &mut NoHost).expect("and runs");
	assert_eq!(out, alloc::vec![Value::I32(40)], "a branch out of a parameterised block carries the block's results and leaves nothing of its parameters behind");
}

#[test]
fn an_if_with_parameters_gives_them_to_both_arms() {
	// The `else` half of the same rule, and the one the VALIDATOR had wrong: it truncated to the
	// frame's height at `else` and stopped, so the then arm saw the block's parameters and the else
	// arm did not - a valid module refused for an underflow it does not have.
	//
	//   (type $t (func (param i32 i32) (result i32)))
	//   (func (param i32) (result i32)
	//     i32.const 7  i32.const 3
	//     local.get 0
	//     if (type $t)  i32.sub  else  i32.add  end)
	//
	// `f(1)` takes the then arm: 7 - 3 = 4. `f(0)` takes the else arm: 7 + 3 = 10.
	let body: &[u8] = &[
		0x41,
		7, // i32.const 7
		0x41,
		3, // i32.const 3
		0x20,
		0x00, // local.get 0
		0x04,
		0x00, // if (type 0)
		0x6b, // i32.sub
		0x05, // else
		0x6a, // i32.add
		0x0b, // end
		0x0b, // end
	];
	let spec = Spec { types: &[(&[I32, I32], &[I32]), (&[I32], &[I32])], imports: &[], funcs: &[1], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] };
	let module = parse(&build(&spec)).expect("a parameterised if parses");
	let validated = validate(module).expect("and validates - both arms are entered with the parameters");
	let mut instance = Instance::new(&validated).expect("and instantiates");
	assert_eq!(instance.invoke("run", &[Value::I32(1)], &mut NoHost).expect("then"), alloc::vec![Value::I32(4)], "the then arm gets the parameters");
	assert_eq!(instance.invoke("run", &[Value::I32(0)], &mut NoHost).expect("else"), alloc::vec![Value::I32(10)], "and so does the else arm");
}

#[test]
fn an_if_with_no_else_is_legal_when_its_empty_branch_typechecks() {
	// `[i32] -> [i32]`: the missing arm produces what it was entered with, so there is nothing for an
	// `else` to say. The rule was `!end_types.is_empty()`, which is the same rule only for blocks
	// with no parameters, and it refused this module.
	//
	//   (func (param i32) (result i32)
	//     i32.const 5
	//     local.get 0
	//     if (type $t) drop i32.const 9 end)   ;; $t = [i32] -> [i32]
	let body: &[u8] = &[
		0x41,
		5, // i32.const 5
		0x20,
		0x00, // local.get 0
		0x04,
		0x00, // if (type 0): [i32] -> [i32]
		0x1a, // drop
		0x41,
		9,    // i32.const 9
		0x0b, // end
		0x0b, // end
	];
	let spec = Spec { types: &[(&[I32], &[I32]), (&[I32], &[I32])], imports: &[], funcs: &[1], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] };
	let module = parse(&build(&spec)).expect("parses");
	let validated = validate(module).expect("an if whose missing else arm typechecks is legal without one");
	let mut instance = Instance::new(&validated).expect("and instantiates");
	assert_eq!(instance.invoke("run", &[Value::I32(1)], &mut NoHost).expect("then"), alloc::vec![Value::I32(9)], "the then arm ran");
	assert_eq!(instance.invoke("run", &[Value::I32(0)], &mut NoHost).expect("no else"), alloc::vec![Value::I32(5)], "and the absent arm left the parameter, which is the result");

	// And the case the old rule was written for is still refused: `[] -> [i32]` has no legal empty
	// branch, because the missing arm would produce nothing where the block promises an `i32`.
	let body: &[u8] = &[
		0x20,
		0x00, // local.get 0
		0x04,
		0x7f, // if (result i32)
		0x41,
		9,    // i32.const 9
		0x0b, // end
		0x0b, // end
	];
	let spec = Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] };
	let module = parse(&build(&spec)).expect("parses");
	assert!(validate(module).is_err(), "an if that promises a result its missing else arm cannot produce is still refused");
}

#[test]
fn an_over_wide_signed_leb_is_refused_at_the_width_it_belongs_to() {
	// `i32.const` is `s32`, `i64.const` is `s64` and a block type is `s33`, and one `s64` reader
	// served all three - so `i32.const 0` written in six bytes decoded and was truncated by `as i32`,
	// which is a different, well-formed module than the one on disk.
	//
	// The specification's own suite named these: seven of its malformed cases stopped parsing when
	// the readers were split, and none of the 707 reaches a running instance any more.
	let over_wide: &[u8] = &[
		0x41,
		0x80,
		0x80,
		0x80,
		0x80,
		0x80,
		0x00, // i32.const 0 in six bytes, one past what `s32` holds
		0x0b,
	];
	let spec = Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], over_wide)] };
	// Refused at VALIDATION rather than at parse: `parse` stores a function body and does not read
	// it, and the validator is the first pass that decodes the instruction stream. Which layer says
	// no is not the point; that one of them does, before anything runs, is.
	let module = parse(&build(&spec)).expect("the section structure is well-formed; the body is not");
	assert!(validate(module).is_err(), "an `i32.const` written wider than `s32` allows is malformed, not a truncated value");

	// The width bound is not a length bound alone: five bytes are legal for `s32`, and the fifth
	// byte's unused payload bits have to repeat the SIGN. `... 0x7f` is `-1` written in full and
	// well-formed; `... 0x0f` is the same five bytes with those bits zeroed, which the specification
	// calls "integer too large" - and its own suite is what said so, four of its malformed cases
	// being exactly that.
	let legal: &[u8] = &[
		0x41,
		0xff,
		0xff,
		0xff,
		0xff,
		0x7f, // i32.const -1, fully written
		0x0b,
	];
	let spec = Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], legal)] };
	let validated = validate(parse(&build(&spec)).expect("parses")).expect("validates");
	let mut instance = Instance::new(&validated).expect("and a five-byte `i32.const -1` is well-formed");
	assert_eq!(instance.invoke("run", &[], &mut NoHost).expect("runs"), alloc::vec![Value::I32(-1)]);
}

#[test]
fn an_export_naming_a_memory_that_is_not_there_does_not_validate() {
	// EVERY OTHER KIND OF EXPORT HAD ITS INDEX CHECKED. This one asked only whether the module
	// declares A memory, so `(memory 1) (export "mem" (memory 1))` validated - and one memory is
	// memory 0, because this engine supports exactly one. A dangling export is a malformed module
	// whichever item it names, and the promise this validator makes is that every static index is
	// checked before an instance exists.
	let good = Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("mem", 0x02, 0)], codes: &[(&[], &[0x0b])] };
	assert!(validate(parse(&build(&good)).expect("parses")).is_ok(), "memory 0 is the one the module declares");

	let dangling = Spec { exports: &[("mem", 0x02, 1)], ..good };
	let error = validate(parse(&build(&dangling)).expect("parses")).expect_err("memory 1 is not a memory this module has");
	assert!(error.reason.contains("names memory 1"), "{error:?}");

	// And the case that already worked, so the rule is not narrowed to the new half.
	let none = Spec { mem_pages: 0, exports: &[("mem", 0x02, 0)], ..good };
	assert!(validate(parse(&build(&none)).expect("parses")).is_err(), "a module with no memory may not export one");
}

#[test]
fn an_exported_import_checks_its_arguments_before_the_host_sees_them() {
	// THE BOUNDARY IS WHERE THE VALUES ENTER. The argument count and type check sat in the DEFINED
	// function branch, past the return that dispatches an import - so a module that imports a
	// function and re-exports it handed the embedder's arguments straight to `Host::call_import`
	// with neither checked. The embedder guarantee was true of every call shape but that one.
	//
	// The host here records what it was handed, so the test can say the values never reached it
	// rather than only that the call failed.
	struct Recording {
		seen: Vec<Vec<Value>>,
	}
	impl Host for Recording {
		fn call_import(&mut self, _import: u32, args: &[Value], _memory: &mut [u8]) -> Result<Vec<Value>, Trap> {
			self.seen.push(args.to_vec());
			Ok(alloc::vec![Value::I32(0)])
		}
	}

	// (import "liber" "read" (func (param i32 i32) (result i32))) re-exported as "run".
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32])], imports: &[("liber", "read", 0)], funcs: &[], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[] });
	let m: ValidatedModule = validate(parse(&wasm).expect("parses")).expect("an imported function may be re-exported");
	let mut inst: Instance = Instance::new(&m).expect("instantiates");

	let mut host = Recording { seen: Vec::new() };
	assert_eq!(inst.invoke("run", &[Value::F64(123.0), Value::I32(1)], &mut host), Err(Trap("an argument's type does not match the function's signature")));
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut host), Err(Trap("wrong argument count")));
	assert!(host.seen.is_empty(), "the host was handed arguments that do not match what the import declares: {:?}", host.seen);

	// And the call that DOES match still reaches it, so the check is a check and not a wall.
	assert_eq!(inst.invoke("run", &[Value::I32(1), Value::I32(2)], &mut host), Ok(alloc::vec![Value::I32(0)]));
	assert_eq!(host.seen, alloc::vec![alloc::vec![Value::I32(1), Value::I32(2)]]);
}

#[test]
fn blocks_may_not_nest_past_the_hosts_control_depth() {
	// ALLOCATION AMPLIFICATION, in the structure that walks the body. `MAX_STACK_DEPTH` bounds the
	// operand stack and does not bound this: entering a block pops its parameters and pushes them
	// straight back, so a body nesting blocks of a type with many parameters holds the stack at a
	// constant height forever while every control frame clones three type vectors. Two bytes of
	// module per frame against kilobytes of validator allocation is the same shape as the `br_table`
	// amplification, which is why the answer is the same: a bound.
	let deep = |depth: usize| {
		let mut body: Vec<u8> = Vec::new();
		for _ in 0..depth {
			body.extend_from_slice(&[0x02, 0x40]); // block (empty type)
		}
		for _ in 0..depth {
			body.push(0x0b); // end
		}
		body.push(0x0b); // end of the function
		body
	};
	let run = |depth: usize| {
		let body = deep(depth);
		let spec = Spec { types: &[(&[], &[])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[], codes: &[(&[], &body)] };
		validate(parse(&build(&spec)).expect("parses"))
	};
	assert!(run(crate::validate::MAX_CONTROL_DEPTH).is_ok(), "the limit itself is reachable, not one short of it");
	let error = run(crate::validate::MAX_CONTROL_DEPTH + 1).expect_err("one deeper is refused");
	assert!(error.reason.contains("nest"), "{error:?}");
}

#[test]
fn memory_grow_is_charged_for_the_pages_it_zeroes() {
	// `memory.copy` and `memory.fill` are charged per kilobyte because their cost is bounded by an
	// operand. `memory.grow` has the same property - `resize` zeroes every byte it adds - and cost
	// one unit, so the budget priced the same work two ways depending on which instruction asked
	// for it.
	//
	// (func (export "run") (result i32) i32.const 16  memory.grow)  ;; sixteen pages = 1 MiB
	let body: &[u8] = &[0x41, 16, 0x40, 0x00, 0x0b];
	let spec = Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] };
	let m: ValidatedModule = validate(parse(&build(&spec)).expect("parses")).expect("validates");

	// Sixteen pages is 1 MiB, which is 1024 units at the bulk rate - so a budget of a few hundred
	// cannot pay for it while it comfortably pays for the two instructions themselves.
	let mut inst = Instance::new(&m).expect("instantiates");
	assert_eq!(inst.invoke_with_fuel("run", &[], &mut NoHost, 256), Err(Trap("out of fuel: the guest ran for longer than the host allows")));
	assert_eq!(inst.memory().len(), 65536, "and a grow the budget refused did not happen");

	// And with fuel to pay for it, the grow succeeds and answers the old size in pages.
	assert_eq!(inst.invoke_with_fuel("run", &[], &mut NoHost, 100_000), Ok(alloc::vec![Value::I32(1)]));
	assert_eq!(inst.memory().len(), 17 * 65536);
}

// The sixth audit round.

// A SECTION MAY NOT READ ITS NEIGHBOUR'S BYTES. A count that runs past the section's own declared
// size used to be served from whatever followed - the next section's content - and only the
// `r.pos != end` check afterwards noticed, once the vector had already been allocated and filled
// from bytes that were never part of this section. With each sub-parser bounded to its own span the
// read fails at the boundary instead, so the failure is "this section is short", which it is.
// A HOST IMPORT IS CHARGED FOR WHAT IT MOVES. Fuel bounded interpretation only: a host call cost
// nothing, so a guest that did nothing but call out could move sixty-four megabytes per unit and the
// budget never noticed. The rate is the one `memory.copy` pays, so a byte costs the same whichever
// side of the boundary moves it.
#[test]
fn a_host_import_is_charged_for_the_work_it_reports() {
	struct BulkHost {
		bytes: usize,
		calls: u32,
	}
	impl Host for BulkHost {
		fn call_import(&mut self, _import: u32, _args: &[Value], _memory: &mut [u8]) -> Result<Vec<Value>, Trap> {
			self.calls += 1;
			Ok(Vec::new())
		}
		fn work_bytes(&self) -> usize {
			self.bytes
		}
	}

	// A module importing `env.work` (type 0) and calling it in a loop until the fuel runs out.
	// body: loop { call 0; br 0 } end
	let body: Vec<u8> = alloc::vec![0x03, 0x40, 0x10, 0x00, 0x0c, 0x00, 0x0b, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[])], imports: &[("env", "work", 0)], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 1)], codes: &[(&[], &body)] });
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");

	// A host that reports moving a megabyte per call: the budget must stop it far sooner than a host
	// that reports nothing.
	let mut heavy = BulkHost { bytes: 1024 * 1024, calls: 0 };
	let mut inst = Instance::new(&m).unwrap();
	let _ = inst.invoke_with_fuel("run", &[], &mut heavy, 100_000);

	let mut light = BulkHost { bytes: 0, calls: 0 };
	let mut inst = Instance::new(&m).unwrap();
	let _ = inst.invoke_with_fuel("run", &[], &mut light, 100_000);

	assert!(heavy.calls > 0 && light.calls > 0, "both hosts ran");
	assert!(light.calls > heavy.calls * 10, "the bulk host must exhaust the budget far sooner: heavy={} light={}", heavy.calls, light.calls);
}

#[test]
fn a_section_cannot_read_into_the_one_that_follows_it() {
	// A type section declaring TWO types but carrying only one, followed by a second section whose
	// bytes happen to spell a valid second type. Unbounded, the type parser walks straight into it.
	let one_type: Vec<u8> = alloc::vec![0x60, 0x00, 0x00];
	let mut types: Vec<u8> = alloc::vec![0x02];
	types.extend_from_slice(&one_type);
	let mut m: Vec<u8> = alloc::vec![0x00, b'a', b's', b'm', 0x01, 0x00, 0x00, 0x00];
	m.extend_from_slice(&section(1, &types));
	// The function section that follows starts with bytes readable as `0x60 () -> ()`.
	m.extend_from_slice(&[0x03, 0x04, 0x60, 0x00, 0x00, 0x00]);
	let err = crate::parser::parse(&m).unwrap_err();
	assert!(err.0.contains("end") || err.0.contains("short") || err.0.contains("section"), "the type section must fail at its own boundary, not borrow the next one: {}", err.0);
}

#[test]
fn a_declared_count_larger_than_its_own_section_is_refused_before_it_is_filled() {
	// Every section parser read a count and then filled the corresponding vector, so a six-byte
	// declaration of four billion entries had the host allocating before anything asked whether the
	// count was plausible. The module is refused in the end and the host paid for it first, which is
	// this engine's resource policy beginning after the expensive part.
	//
	// The bound needs no knowledge of the entries: a count times the SMALLEST an entry can be is a
	// floor on the bytes the section must contain, and a count past the bytes remaining cannot be
	// honest whatever else is true.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	// A type section declaring 0x0fff_ffff types in five bytes of content.
	let content: Vec<u8> = alloc::vec![0xff, 0xff, 0xff, 0x7f];
	wasm.extend_from_slice(&section(1, &content));
	assert!(crate::parse(&wasm).is_err(), "a count larger than its section could encode is refused at the boundary");

	// And an honest one still parses, so this is a bound and not a refusal of the ordinary case.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	let content: Vec<u8> = alloc::vec![0x01, 0x60, 0x00, 0x00];
	wasm.extend_from_slice(&section(1, &content));
	assert_eq!(crate::parse(&wasm).expect("one type in four bytes is honest").types.len(), 1);
}

#[test]
fn the_published_stack_ceiling_is_the_effective_one() {
	// The precheck reserved `MAX_TYPE_RESULTS` on top of the limit before EVERY instruction,
	// independently of what that instruction does. Past `MAX_STACK_DEPTH - MAX_TYPE_RESULTS` the
	// next opcode was refused even when it was a `drop` - room reserved for the widest call any
	// module could contain, in front of an instruction that cannot make one. So the documented
	// 8,192 was not the effective ceiling, and acceptance could turn on where a body's high-water
	// mark happened to fall.
	//
	// This body sits in the band the old check refused and the published one allows: it pushes
	// past the old effective ceiling, then drops back down. Every instruction in it is legal and
	// the stack never reaches `MAX_STACK_DEPTH`.
	let depth = crate::validate::MAX_STACK_DEPTH - 16;
	let mut body: Vec<u8> = alloc::vec![0x00]; // no locals
	for _ in 0..depth {
		body.push(0x41);
		body.extend_from_slice(&sleb(1));
	}
	for _ in 0..depth {
		body.push(0x1a); // drop
	}
	body.push(0x0b);

	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	wasm.extend_from_slice(&section(1, &[1, 0x60, 0x00, 0x00]));
	wasm.extend_from_slice(&section(3, &[1, 0]));
	let mut exports: Vec<u8> = alloc::vec![1, 3];
	exports.extend_from_slice(b"run");
	exports.extend_from_slice(&[0x00, 0]);
	wasm.extend_from_slice(&section(7, &exports));
	let mut code: Vec<u8> = alloc::vec![1];
	code.extend_from_slice(&leb(body.len() as u32));
	code.extend_from_slice(&body);
	wasm.extend_from_slice(&section(10, &code));

	let parsed = parse(&wasm).expect("parses");
	assert!(validate(parsed).is_ok(), "a body that stays under the published ceiling is accepted: depth {depth} of {}", crate::validate::MAX_STACK_DEPTH);
}
