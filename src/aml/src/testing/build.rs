//! AN AML ENCODER FOR THE TESTS: each helper answers the bytes ASL's construct compiles to, so a test writes the
//! firmware it runs the way the specification's examples read.

use alloc::vec;
use alloc::vec::Vec;

pub fn pkg(body: &[u8]) -> Vec<u8> {
	// The length counts its own bytes: one byte up to 63 in all, else two, three or four.
	for extra in 0..4usize {
		let total = body.len() + 1 + extra;
		let fits = match extra {
			0 => total < 0x40,
			1 => total < 0x1000,
			2 => total < 0x10_0000,
			_ => total < 0x1000_0000,
		};
		if fits {
			let mut out = Vec::new();
			if extra == 0 {
				out.push(total as u8);
			} else {
				out.push(((extra as u8) << 6) | (total & 0x0F) as u8);
				for i in 0..extra {
					out.push((total >> (4 + 8 * i)) as u8);
				}
			}
			out.extend_from_slice(body);
			return out;
		}
	}
	panic!("a package too large for a test");
}

/// A field length or reserved length: the package-length encoding of a plain number.
pub fn field_length(bits: usize) -> Vec<u8> {
	if bits < 0x40 {
		return vec![bits as u8];
	}
	for extra in 1..4usize {
		if bits < 1 << (4 + 8 * extra) {
			let mut out = vec![((extra as u8) << 6) | (bits & 0x0F) as u8];
			for i in 0..extra {
				out.push((bits >> (4 + 8 * i)) as u8);
			}
			return out;
		}
	}
	panic!("a field too long for a test");
}

fn seg(text: &str) -> [u8; 4] {
	let mut out = [b'_'; 4];
	out[..text.len()].copy_from_slice(text.as_bytes());
	out
}

/// A name string: `\A.B`, `^X`, `FOO`.
pub fn name(text: &str) -> Vec<u8> {
	let mut out = Vec::new();
	let mut rest = text;
	if let Some(after) = rest.strip_prefix('\\') {
		out.push(b'\\');
		rest = after;
	}
	while let Some(after) = rest.strip_prefix('^') {
		out.push(b'^');
		rest = after;
	}
	let segs: Vec<&str> = if rest.is_empty() { Vec::new() } else { rest.split('.').collect() };
	match segs.len() {
		0 => out.push(0),
		1 => out.extend_from_slice(&seg(segs[0])),
		2 => {
			out.push(0x2E);
			out.extend_from_slice(&seg(segs[0]));
			out.extend_from_slice(&seg(segs[1]));
		}
		count => {
			out.push(0x2F);
			out.push(count as u8);
			for s in segs {
				out.extend_from_slice(&seg(s));
			}
		}
	}
	out
}

pub fn int(value: u64) -> Vec<u8> {
	match value {
		0 => vec![0x00],
		1 => vec![0x01],
		u64::MAX => vec![0xFF],
		v if v <= 0xFF => vec![0x0A, v as u8],
		v if v <= 0xFFFF => {
			let mut out = vec![0x0B];
			out.extend_from_slice(&(v as u16).to_le_bytes());
			out
		}
		v if v <= 0xFFFF_FFFF => {
			let mut out = vec![0x0C];
			out.extend_from_slice(&(v as u32).to_le_bytes());
			out
		}
		v => {
			let mut out = vec![0x0E];
			out.extend_from_slice(&v.to_le_bytes());
			out
		}
	}
}

pub fn string(text: &str) -> Vec<u8> {
	let mut out = vec![0x0D];
	out.extend_from_slice(text.as_bytes());
	out.push(0);
	out
}

pub fn buffer(bytes: &[u8]) -> Vec<u8> {
	let mut body = int(bytes.len() as u64);
	body.extend_from_slice(bytes);
	let mut out = vec![0x11];
	out.extend(pkg(&body));
	out
}

/// `Buffer(size) {}`, all zero.
pub fn buffer_of(size: u64) -> Vec<u8> {
	let mut out = vec![0x11];
	out.extend(pkg(&int(size)));
	out
}

pub fn package(elements: &[Vec<u8>]) -> Vec<u8> {
	let mut body = vec![elements.len() as u8];
	for element in elements {
		body.extend_from_slice(element);
	}
	let mut out = vec![0x12];
	out.extend(pkg(&body));
	out
}

pub fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
	parts.concat()
}

pub fn name_obj(n: &str, value: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x08], name(n), value])
}

pub fn method(n: &str, args: u8, body: Vec<u8>) -> Vec<u8> {
	method_flags(n, args & 0x07, body)
}

pub fn method_flags(n: &str, flags: u8, body: Vec<u8>) -> Vec<u8> {
	let mut inner = name(n);
	inner.push(flags);
	inner.extend(body);
	let mut out = vec![0x14];
	out.extend(pkg(&inner));
	out
}

fn ext_pkg(op: u8, n: &str, head: &[u8], body: Vec<u8>) -> Vec<u8> {
	let mut inner = name(n);
	inner.extend_from_slice(head);
	inner.extend(body);
	let mut out = vec![0x5B, op];
	out.extend(pkg(&inner));
	out
}

pub fn device(n: &str, body: Vec<u8>) -> Vec<u8> {
	ext_pkg(0x82, n, &[], body)
}

pub fn thermal_zone(n: &str, body: Vec<u8>) -> Vec<u8> {
	ext_pkg(0x85, n, &[], body)
}

pub fn power_resource(n: &str, level: u8, order: u16, body: Vec<u8>) -> Vec<u8> {
	let mut head = vec![level];
	head.extend_from_slice(&order.to_le_bytes());
	ext_pkg(0x84, n, &head, body)
}

pub fn processor(n: &str, id: u8, block: u32, length: u8, body: Vec<u8>) -> Vec<u8> {
	let mut head = vec![id];
	head.extend_from_slice(&block.to_le_bytes());
	head.push(length);
	ext_pkg(0x83, n, &head, body)
}

pub fn scope(n: &str, body: Vec<u8>) -> Vec<u8> {
	let mut inner = name(n);
	inner.extend(body);
	let mut out = vec![0x10];
	out.extend(pkg(&inner));
	out
}

pub fn local(n: u8) -> Vec<u8> {
	vec![0x60 + n]
}

pub fn arg(n: u8) -> Vec<u8> {
	vec![0x68 + n]
}

pub fn debug() -> Vec<u8> {
	vec![0x5B, 0x31]
}

pub fn store(value: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x70], value, target])
}

pub fn copy_object(value: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x9D], value, target])
}

fn binary(op: u8, a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![op], a, b, target])
}

pub fn add(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x72, a, b, target)
}

pub fn subtract(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x74, a, b, target)
}

pub fn multiply(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x77, a, b, target)
}

pub fn shift_left(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x79, a, b, target)
}

pub fn and(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x7B, a, b, target)
}

pub fn or(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x7D, a, b, target)
}

pub fn modulo(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x85, a, b, target)
}

pub fn concat(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x73, a, b, target)
}

pub fn concat_res(a: Vec<u8>, b: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	binary(0x84, a, b, target)
}

pub fn divide(a: Vec<u8>, b: Vec<u8>, remainder: Vec<u8>, quotient: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x78], a, b, remainder, quotient])
}

pub fn not(a: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x80], a, target])
}

pub fn find_set_left_bit(a: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x81], a, target])
}

pub fn increment(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x75], target])
}

pub fn decrement(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x76], target])
}

pub fn lequal(a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x93], a, b])
}

pub fn lless(a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x95], a, b])
}

pub fn lgreater(a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x94], a, b])
}

pub fn lnot(a: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x92], a])
}

pub fn land(a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x90], a, b])
}

pub fn if_(predicate: Vec<u8>, body: Vec<u8>) -> Vec<u8> {
	let mut out = vec![0xA0];
	out.extend(pkg(&cat(&[predicate, body])));
	out
}

pub fn else_(body: Vec<u8>) -> Vec<u8> {
	let mut out = vec![0xA1];
	out.extend(pkg(&body));
	out
}

pub fn while_(predicate: Vec<u8>, body: Vec<u8>) -> Vec<u8> {
	let mut out = vec![0xA2];
	out.extend(pkg(&cat(&[predicate, body])));
	out
}

pub fn ret(value: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0xA4], value])
}

pub fn brk() -> Vec<u8> {
	vec![0xA5]
}

pub fn index(source: Vec<u8>, at: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x88], source, at, target])
}

pub fn deref(reference: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x83], reference])
}

pub fn refof(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x71], target])
}

pub fn cond_refof(target: Vec<u8>, result: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x12], target, result])
}

pub fn sizeof(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x87], target])
}

pub fn object_type(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x8E], target])
}

pub fn to_hex_string(a: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x98], a, target])
}

pub fn to_decimal_string(a: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x97], a, target])
}

pub fn to_integer(a: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x99], a, target])
}

pub fn to_buffer(a: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x96], a, target])
}

pub fn to_string(a: Vec<u8>, length: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x9C], a, length, target])
}

pub fn mid(a: Vec<u8>, at: Vec<u8>, length: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x9E], a, at, length, target])
}

pub fn match_(package: Vec<u8>, op1: u8, a: Vec<u8>, op2: u8, b: Vec<u8>, start: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x89], package, vec![op1], a, vec![op2], b, start])
}

pub fn notify(target: Vec<u8>, value: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x86], target, value])
}

pub fn mutex(n: &str, sync: u8) -> Vec<u8> {
	cat(&[vec![0x5B, 0x01], name(n), vec![sync]])
}

pub fn event(n: &str) -> Vec<u8> {
	cat(&[vec![0x5B, 0x02], name(n)])
}

pub fn acquire(target: Vec<u8>, timeout: u16) -> Vec<u8> {
	cat(&[vec![0x5B, 0x23], target, timeout.to_le_bytes().to_vec()])
}

pub fn release(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x27], target])
}

pub fn signal(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x24], target])
}

pub fn wait(target: Vec<u8>, timeout: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x25], target, timeout])
}

pub fn sleep(ms: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x22], ms])
}

pub fn stall(us: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x21], us])
}

pub fn fatal(kind: u8, code: u32, argument: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x32, kind], code.to_le_bytes().to_vec(), argument])
}

pub fn alias(source: &str, target: &str) -> Vec<u8> {
	cat(&[vec![0x06], name(source), name(target)])
}

pub fn external(n: &str, kind: u8, args: u8) -> Vec<u8> {
	cat(&[vec![0x15], name(n), vec![kind, args]])
}

pub fn op_region(n: &str, space: u8, offset: Vec<u8>, length: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x80], name(n), vec![space], offset, length])
}

pub fn named_field(n: &str, bits: usize) -> Vec<u8> {
	let mut out = seg(n).to_vec();
	out.extend(field_length(bits));
	out
}

pub fn reserved(bits: usize) -> Vec<u8> {
	let mut out = vec![0x00];
	out.extend(field_length(bits));
	out
}

pub fn access_as(kind: u8, attrib: u8) -> Vec<u8> {
	vec![0x01, kind, attrib]
}

pub fn access_as_bytes(kind: u8, attrib: u8, length: u8) -> Vec<u8> {
	vec![0x03, kind, attrib, length]
}

pub fn connection(resource: &[u8]) -> Vec<u8> {
	cat(&[vec![0x02], buffer(resource)])
}

pub fn field(region: &str, flags: u8, elements: &[Vec<u8>]) -> Vec<u8> {
	let mut inner = name(region);
	inner.push(flags);
	for element in elements {
		inner.extend_from_slice(element);
	}
	let mut out = vec![0x5B, 0x81];
	out.extend(pkg(&inner));
	out
}

pub fn index_field(index: &str, data: &str, flags: u8, elements: &[Vec<u8>]) -> Vec<u8> {
	let mut inner = name(index);
	inner.extend(name(data));
	inner.push(flags);
	for element in elements {
		inner.extend_from_slice(element);
	}
	let mut out = vec![0x5B, 0x86];
	out.extend(pkg(&inner));
	out
}

pub fn bank_field(region: &str, bank: &str, value: Vec<u8>, flags: u8, elements: &[Vec<u8>]) -> Vec<u8> {
	let mut inner = name(region);
	inner.extend(name(bank));
	inner.extend(value);
	inner.push(flags);
	for element in elements {
		inner.extend_from_slice(element);
	}
	let mut out = vec![0x5B, 0x87];
	out.extend(pkg(&inner));
	out
}

pub fn create_dword_field(source: Vec<u8>, at: Vec<u8>, n: &str) -> Vec<u8> {
	cat(&[vec![0x8A], source, at, name(n)])
}

pub fn create_bit_field(source: Vec<u8>, at: Vec<u8>, n: &str) -> Vec<u8> {
	cat(&[vec![0x8D], source, at, name(n)])
}

pub fn create_field(source: Vec<u8>, bit: Vec<u8>, bits: Vec<u8>, n: &str) -> Vec<u8> {
	cat(&[vec![0x5B, 0x13], source, bit, bits, name(n)])
}

pub fn load_table(signature: &str, oem: &str, table: &str) -> Vec<u8> {
	cat(&[vec![0x5B, 0x1F], string(signature), string(oem), string(table), string(""), string(""), int(0)])
}

pub fn load_op(source: Vec<u8>, target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x20], source, target])
}

pub fn unload(target: Vec<u8>) -> Vec<u8> {
	cat(&[vec![0x5B, 0x2A], target])
}

/// `ToUUID("...")` - the buffer.
pub fn uuid(text: &str) -> Vec<u8> {
	buffer(&crate::dsd::uuid(text).expect("a UUID"))
}

/// `EISAID("PNP0A08")`.
pub fn eisaid(text: &str) -> Vec<u8> {
	int(crate::resource::eisa_value(text).expect("an EISA id") as u64)
}

/// A table: signature, revision, the AML, with a valid checksum.
pub fn table(signature: &str, revision: u8, body: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(signature.as_bytes());
	out.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
	out.push(revision);
	out.push(0);
	out.extend_from_slice(b"LIBER ");
	out.extend_from_slice(b"TESTTABL");
	out.extend_from_slice(&1u32.to_le_bytes());
	out.extend_from_slice(b"LIBR");
	out.extend_from_slice(&1u32.to_le_bytes());
	out.extend_from_slice(body);
	let sum = out.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	out[9] = 0u8.wrapping_sub(sum);
	out
}

// ------------------------------------------------------------------ resource descriptors

pub fn end_tag() -> Vec<u8> {
	vec![0x79, 0x00]
}

pub fn template(descriptors: &[Vec<u8>]) -> Vec<u8> {
	let mut out = descriptors.concat();
	out.extend(end_tag());
	out
}

pub fn memory32_fixed(writable: bool, base: u32, length: u32) -> Vec<u8> {
	let mut out = vec![0x86, 9, 0, writable as u8];
	out.extend_from_slice(&base.to_le_bytes());
	out.extend_from_slice(&length.to_le_bytes());
	out
}

pub fn io(base: u16, length: u8) -> Vec<u8> {
	let mut out = vec![0x47, 0x01];
	out.extend_from_slice(&base.to_le_bytes());
	out.extend_from_slice(&base.to_le_bytes());
	out.push(1);
	out.push(length);
	out
}

pub fn irq_no_flags(lines: &[u8]) -> Vec<u8> {
	let mask: u16 = lines.iter().fold(0, |mask, line| mask | 1 << line);
	let mut out = vec![0x22];
	out.extend_from_slice(&mask.to_le_bytes());
	out
}

pub fn interrupt(consumer: bool, level: bool, active_low: bool, lines: &[u32]) -> Vec<u8> {
	let flags = consumer as u8 | (!level as u8) << 1 | (active_low as u8) << 2;
	let length = 2 + 4 * lines.len();
	let mut out = vec![0x89];
	out.extend_from_slice(&(length as u16).to_le_bytes());
	out.push(flags);
	out.push(lines.len() as u8);
	for line in lines {
		out.extend_from_slice(&line.to_le_bytes());
	}
	out
}

/// `QWordMemory` with the consumer/producer bit.
pub fn qword_memory(producer: bool, base: u64, length: u64) -> Vec<u8> {
	let mut body = vec![0u8, (!producer) as u8, 0];
	body.extend_from_slice(&0u64.to_le_bytes());
	body.extend_from_slice(&base.to_le_bytes());
	body.extend_from_slice(&(base + length - 1).to_le_bytes());
	body.extend_from_slice(&0u64.to_le_bytes());
	body.extend_from_slice(&length.to_le_bytes());
	let mut out = vec![0x8A];
	out.extend_from_slice(&(body.len() as u16).to_le_bytes());
	out.extend(body);
	out
}

/// `GpioInt` (`interrupt`) or `GpioIo` with `pins` on `controller`; `flags` the interrupt-or-I/O flags word.
pub fn gpio(interrupt: bool, flags: u16, pins: &[u16], controller: &str) -> Vec<u8> {
	let pin_offset: u16 = 23;
	let source_offset = pin_offset + 2 * pins.len() as u16;
	let end = source_offset + controller.len() as u16 + 1;
	let mut out = vec![0x8C, 0, 0, 1, if interrupt { 0 } else { 1 }, 0, 0];
	out.extend_from_slice(&flags.to_le_bytes());
	out.push(0);
	out.extend_from_slice(&0u16.to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	out.extend_from_slice(&pin_offset.to_le_bytes());
	out.push(0);
	out.extend_from_slice(&source_offset.to_le_bytes());
	out.extend_from_slice(&end.to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	for pin in pins {
		out.extend_from_slice(&pin.to_le_bytes());
	}
	out.extend_from_slice(controller.as_bytes());
	out.push(0);
	let length = (out.len() - 3) as u16;
	out[1..3].copy_from_slice(&length.to_le_bytes());
	out
}

/// `I2cSerialBusV2(address, ControllerInitiated, speed, AddressingMode7Bit, controller)`.
pub fn i2c(address: u16, speed: u32, controller: &str) -> Vec<u8> {
	let mut out = vec![0x8E, 0, 0, 2, 0, 1, 0x02, 0, 0, 1];
	out.extend_from_slice(&6u16.to_le_bytes());
	out.extend_from_slice(&speed.to_le_bytes());
	out.extend_from_slice(&address.to_le_bytes());
	out.extend_from_slice(controller.as_bytes());
	out.push(0);
	let length = (out.len() - 3) as u16;
	out[1..3].copy_from_slice(&length.to_le_bytes());
	out
}
