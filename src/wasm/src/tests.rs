// Host-side tests for the WebAssembly runtime: they hand-encode small modules with
// a builder, parse them, and run them through the interpreter - including the
// import path, which is how a component reaches the host.

use crate::*;
use alloc::vec::Vec;

const I32: u8 = 0x7f;

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

// A module specification the test builder turns into a wasm binary.
struct Spec<'a> {
	types: &'a [(&'a [u8], &'a [u8])],        // (param val-types, result val-types)
	imports: &'a [(&'a str, &'a str, u32)],   // (module, field, type index)
	funcs: &'a [u32],                         // type index per defined function
	mem_pages: u32,                           // 0 = declare no memory
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
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 42, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

#[test]
fn runs_arithmetic() {
	// (func (export "run") (result i32) i32.const 40  i32.const 2  i32.add)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 40, 0x41, 2, 0x6a, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(42)]);
}

#[test]
fn passes_arguments_through_locals() {
	// (func (export "run") (param i32) (result i32) local.get 0  i32.const 1  i32.add)
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x20, 0, 0x41, 1, 0x6a, 0x0b])] });
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
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x0b])] });
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
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x1a, 0x41, 0, 0x2d, 0, 0, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	// 'h' is 104; the component read its granted bytes and returned the first one.
	assert_eq!(inst.invoke("run", &[], &mut ReadHost).unwrap(), alloc::vec![Value::I32(104)]);
}

#[test]
fn an_unwired_import_traps() {
	// The same module, but the host refuses the import: the component gets nothing.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber", "read", 0)], funcs: &[1], mem_pages: 1, exports: &[("run", 0x00, 1)], codes: &[(&[], &[0x41, 0, 0x41, 5, 0x10, 0, 0x0b])] });
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
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 0x80, 0x02, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost).unwrap(), alloc::vec![Value::I32(256)]);
}

#[test]
fn rejects_an_unsupported_opcode() {
	// body: i32.const 0  block(0x02) ... - block is unsupported control flow.
	let wasm: Vec<u8> = build(&Spec { types: &[(&[], &[I32])], imports: &[], funcs: &[0], mem_pages: 0, exports: &[("run", 0x00, 0)], codes: &[(&[], &[0x41, 0, 0x02, 0x40, 0x0b, 0x0b])] });
	let m: Module = parse(&wasm).unwrap();
	let mut inst: Instance = Instance::new(&m);
	assert_eq!(inst.invoke("run", &[], &mut NoHost), Err(Trap("unsupported opcode")));
}
