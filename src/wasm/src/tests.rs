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
	assert_eq!(m.module().memory_max_pages, Some(2), "the declared maximum is kept rather than skipped");
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
	assert_eq!(inst.invoke("run", &[Value::I32(2)], &mut NoHost), Err(Trap("call_indirect: signature mismatch")));
}

#[test]
fn call_indirect_traps_on_a_null_or_out_of_range_entry() {
	// Slot 3 was never filled (a null entry), and slot 4 is past the table's end - both trap.
	let wasm: Vec<u8> = indirect_module();
	let m: ValidatedModule = validate(parse(&wasm).unwrap()).expect("the fixture module validates");
	let mut inst: Instance = Instance::new(&m).expect("the validated module instantiates");
	assert_eq!(inst.invoke("run", &[Value::I32(3)], &mut NoHost), Err(Trap("call_indirect: null table entry")));
	assert_eq!(inst.invoke("run", &[Value::I32(4)], &mut NoHost), Err(Trap("call_indirect: table index out of bounds")));
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
// EVERY ONE OF THESE WAS ACCEPTED BY THE TREE BEFORE P02M0134, and most of them ran. That ordering
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
fn a_non_canonical_leb128_is_refused() {
	// A type-section count of 1 encoded as `0x81 0x00` - two spellings of one module, which matters
	// the moment anything hashes or signs one. RAN BEFORE: widths were not checked.
	let mut wasm: Vec<u8> = alloc::vec![];
	wasm.extend_from_slice(b"\0asm");
	wasm.extend_from_slice(&[1, 0, 0, 0]);
	// section 1, content = [count=0x81 0x00, 0x60, 0 params, 0 results]
	let content: Vec<u8> = alloc::vec![0x81, 0x00, 0x60, 0x00, 0x00];
	wasm.extend_from_slice(&section(1, &content));
	let reason = refuse(&wasm, "a redundant LEB128 byte");
	assert!(reason.contains("canonical") || reason.contains("LEB128"), "got: {reason}");
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
