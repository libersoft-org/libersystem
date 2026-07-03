// Host-side tests for the WebAssembly runtime: they hand-encode small modules with
// a builder, parse them, and run them through the interpreter - including the
// import path, which is how a component reaches the host.

use crate::*;
use alloc::vec::Vec;

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
const F32: u8 = 0x7d;
const F64: u8 = 0x7c;

// Unsigned LEB128 of `v`.
fn leb(mut v: u32) -> Vec<u8> {
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
fn section(id: u8, content: &[u8]) -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![id];
	out.extend_from_slice(&leb(content.len() as u32));
	out.extend_from_slice(content);
	out
}

// A length-prefixed name.
fn name(s: &str) -> Vec<u8> {
	let mut out: Vec<u8> = leb(s.len() as u32);
	out.extend_from_slice(s.as_bytes());
	out
}

// Signed LEB128 of `v`.
fn sleb(mut v: i64) -> Vec<u8> {
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
struct Spec<'a> {
	types: &'a [(&'a [u8], &'a [u8])],        // (param val-types, result val-types)
	imports: &'a [(&'a str, &'a str, u32)],   // (module, field, type index)
	funcs: &'a [u32],                         // type index per defined function
	mem_pages: u32,                           // 0 = declare no memory
	globals: &'a [(u8, bool, i64)],           // (value-type byte, mutable, constant init)
	data: &'a [(u32, &'a [u8])],              // (memory offset, bytes)
	exports: &'a [(&'a str, u8, u32)],        // (name, kind byte, index)
	codes: &'a [(&'a [(u32, u8)], &'a [u8])], // (local groups, body bytes)
}

// Encode a [`Spec`] as a wasm module binary.
fn build(spec: &Spec) -> Vec<u8> {
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
fn runs_a_constant() {
	// (func (export "run") (result i32) i32.const 42)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 42, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

#[test]
fn runs_arithmetic() {
	// (func (export "run") (result i32) i32.const 40  i32.const 2  i32.add)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 40, 0x41, 2, 0x6a, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

#[test]
fn passes_arguments_through_locals() {
	// (func (export "run") (param i32) (result i32) local.get 0  i32.const 1  i32.add)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x20, 0, 0x41, 1, 0x6a, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	assert_eq!(m.imports.len(), 1);
	assert_eq!(m.export_func("run"), Some(1));
	let mut inst: Instance = Instance::new(&m);
	let result: Vec<Value> = inst.invoke("run", &[], &mut ReadHost).unwrap();
	assert_eq!(result, alloc::vec![Value::I32(5)], "run returns the byte count from the import");
	assert_eq!(&inst.memory()[0..5], b"hello", "the import wrote into linear memory");
}

#[test]
fn reads_back_memory_the_import_wrote() {
	// run: read 5 bytes at 0, drop the count, then load8_u memory[0] and return it.
	// i32.const 0  i32.const 5  call 0  drop  i32.const 0  i32.load8_u 0 0
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x1a, 0x41, 0, 0x2d, 0, 0, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	// 'h' is 104; the component read its granted bytes and returned the first one.
	assert_eq!(inst.invoke("run", &[], &mut ReadHost).unwrap(), alloc::vec![Value::I32(104)]);
}

#[test]
fn an_unwired_import_traps() {
	// The same module, but the host refuses the import: the component gets nothing.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(256)]);
}

#[test]
fn rejects_an_unsupported_opcode() {
	// body: a SIMD (v128) prefix opcode - vector instructions are out of scope, so the
	// decoder rejects it at instantiation and the trap surfaces on invoke.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0xfd, 0x00, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("unsupported opcode")));
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("invalid conversion to integer")));
}

#[test]
fn loads_and_stores_a_float() {
	// (func (param f32) (result f32) i32.const 0  local.get 0  f32.store  i32.const 0  f32.load)
	let body: &[u8] = &[0x41, 0x00, 0x20, 0x00, 0x38, 0x02, 0x00, 0x41, 0x00, 0x2a, 0x02, 0x00, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[F32], &[F32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[Value::F32(3.25)], &mut NoHost).unwrap(), alloc::vec![Value::F32(3.25)]);
}

#[test]
fn saturates_when_truncating_a_float_to_int() {
	// (func (param f64) (result i32) local.get 0  i32.trunc_sat_f64_s) - the
	// non-trapping cast Rust's `as` emits: NaN -> 0, out-of-range saturates to
	// i32::MIN/MAX, otherwise truncates toward zero.
	let body: &[u8] = &[0x20, 0x00, 0xfc, 0x02, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[F64], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[Value::I32(5)], &mut NoHost).unwrap(), alloc::vec![Value::I32(15)]);
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(0)]);
}

#[test]
fn takes_each_if_branch() {
	// (func (param i32) (result i32) (if (result i32) (then 10) (else 20)))
	let body: &[u8] = &[0x20, 0x00, 0x04, 0x7f, 0x41, 0x0a, 0x05, 0x41, 0x14, 0x0b, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
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
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(10)]);
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut NoHost).unwrap(), alloc::vec![Value::I32(20)]);
	assert_eq!(inst.invoke("run", &[Value::I32(7)], &mut NoHost).unwrap(), alloc::vec![Value::I32(30)]);
}

#[test]
fn reads_and_writes_a_global() {
	// One mutable i32 global initialized to 7; run bumps it by one and returns it.
	let body: &[u8] = &[0x23, 0x00, 0x41, 0x01, 0x6a, 0x24, 0x00, 0x23, 0x00, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[(I32, true, 7)], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	// the global persists across calls on the same instance.
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(8)]);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(9)]);
}

#[test]
fn initializes_memory_from_a_data_segment() {
	// A data segment writes "Hi" at offset 0; run reads back the first byte.
	let body: &[u8] = &[0x41, 0x00, 0x2d, 0x00, 0x00, 0x0b]; // i32.const 0; i32.load8_u
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 1, globals: &[], data: &[(0, b"Hi")], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(b'H' as i32)]);
	assert_eq!(&inst.memory()[0..2], b"Hi", "the data segment initialized linear memory");
}

#[test]
fn does_i64_arithmetic() {
	// (func (result i64) i64.const 1  i64.const 32  i64.shl) -> 1 << 32, an i64 value.
	let body: &[u8] = &[0x42, 0x01, 0x42, 0x20, 0x86, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I64])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I64(4294967296)]);
}

#[test]
fn traps_on_divide_by_zero() {
	// (func (result i32) i32.const 10  i32.const 0  i32.div_s) traps at runtime.
	let body: &[u8] = &[0x41, 0x0a, 0x41, 0x00, 0x6d, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("integer divide by zero")));
}

#[test]
fn selects_between_two_values() {
	// (func (param i32) (result i32) i32.const 11  i32.const 22  local.get 0  select)
	let body: &[u8] = &[0x41, 0x0b, 0x41, 0x16, 0x20, 0x00, 0x1b, 0x0b];
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], body)] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[Value::I32(1)], &mut NoHost).unwrap(), alloc::vec![Value::I32(11)]);
	assert_eq!(inst.invoke("run", &[Value::I32(0)], &mut NoHost).unwrap(), alloc::vec![Value::I32(22)]);
}

#[test]
fn rejects_an_out_of_range_branch() {
	// (func (result i32) br 1) - only the function-level label is in scope, so a
	// branch to depth 1 is structurally invalid and rejected at instantiation.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, globals: &[], data: &[], exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x0c, 0x01, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("branch label out of range")));
}
