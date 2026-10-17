use super::*;
use std::vec::Vec;

fn namespace(state: u16, capabilities: Option<u32>) -> Namespace {
	Namespace { index: 2, base: 0x1_8000_0000, len: 16 << 20, attributes: 0x8008, state, modules: 1, flush_hints: 0, capabilities }
}

#[test]
// THE PLATFORM'S WORD DECIDES THE ORDERING, AND A MODULE THAT WILL NOT SAVE DECIDES THE SERVICE.
fn a_namespace_is_writable_with_the_durability_its_platform_names_or_read_only_for_a_reason() {
	assert_eq!(plan(&namespace(0, Some(nfit::CAPABILITY_CPU_CACHE_FLUSH | nfit::CAPABILITY_MEMORY_CONTROLLER_FLUSH))), Ok(Durability::Visible), "caches inside the domain: a fence is enough");
	assert_eq!(plan(&namespace(0, Some(nfit::CAPABILITY_MEMORY_CONTROLLER_FLUSH))), Ok(Durability::Flushed), "the memory controller alone: the lines are written back");
	assert_eq!(plan(&namespace(0, None)), Ok(Durability::Flushed), "a table that says nothing is a platform older than the structure, and the lines are written back");
	assert_eq!(plan(&namespace(0, Some(0))), Err(ReadOnly::NoDomain), "a platform that names no domain promises nothing a store reaches");
	assert_eq!(plan(&namespace(nfit::STATE_NOT_ARMED, Some(nfit::CAPABILITY_CPU_CACHE_FLUSH))), Err(ReadOnly::NotArmed), "a module that is not armed will not save the write");
	for (flag, _) in STATE_WORDS {
		assert_eq!(plan(&namespace(flag, None)), Ok(Durability::Flushed), "a failed save, restore or flush is said, and the namespace stays writable so it can be repaired");
	}
}

#[test]
fn a_request_s_bytes_lie_inside_the_namespace_or_nowhere() {
	let capacity = 16u64 << 20;
	assert_eq!(span(0, 1, capacity), Some((0, 512)));
	assert_eq!(span(2049, 8, capacity), Some((2049 * 512, 4096)));
	let last = capacity / SECTOR - 1;
	assert_eq!(span(last, 1, capacity), Some((last as usize * 512, 512)), "the last sector");
	assert_eq!(span(last, 2, capacity), None, "one past it");
	assert_eq!(span(u64::MAX / 256, 1, capacity), None, "an address that wraps");
}

// What the processor was asked to do, in order.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Step {
	// A line written back, and whether the namespace already held the new bytes there when it was.
	Flush(usize, bool),
	Fence,
}

struct Recorder {
	steps: Vec<Step>,
	pmem: *const u8,
	expect: Vec<u8>,
}

impl Persist for Recorder {
	fn flush(&mut self, address: usize) {
		let at = address - self.pmem as usize;
		let written = (at..at + LINE).filter(|&byte| byte < self.expect.len()).all(|byte| unsafe { *self.pmem.add(byte) } == self.expect[byte]);
		self.steps.push(Step::Flush(at, written));
	}

	fn fence(&mut self) {
		self.steps.push(Step::Fence);
	}
}

#[test]
// THE ANSWER IS TRUE WHEN IT IS GIVEN: the bytes are in place before their line is written back, every line the span
// touches is written back once - the partial ones at both ends among them - and the fence comes after all of it.
fn a_write_is_copied_then_every_line_it_touches_is_written_back_then_fenced() {
	let mut pmem = alloc::vec![0u8; 4096 + LINE];
	let base = pmem.as_mut_ptr();
	let aligned = (LINE - (base as usize % LINE)) % LINE;
	let src: Vec<u8> = (0..200).map(|at| at as u8 ^ 0xA5).collect();
	let offset = aligned + 40;
	let mut expect = alloc::vec![0u8; pmem.len()];
	expect[offset..offset + src.len()].copy_from_slice(&src);
	let mut recorder = Recorder { steps: Vec::new(), pmem: base, expect: expect.clone() };
	assert!(unsafe { write_durably(base, pmem.len(), offset, &src, Durability::Flushed, LINE, &mut recorder) });
	assert_eq!(pmem, expect, "the bytes landed where the request said");
	let lines: Vec<usize> = (0..4).map(|line| aligned + line * LINE).collect();
	let mut want: Vec<Step> = lines.iter().map(|&at| Step::Flush(at, true)).collect();
	want.push(Step::Fence);
	assert_eq!(recorder.steps, want, "40 bytes into a line and 200 long is four lines, each written back after its bytes, then the fence");
}

#[test]
fn where_the_caches_are_in_the_domain_the_write_is_one_fence_and_a_span_past_the_end_writes_nothing() {
	let mut pmem = alloc::vec![0u8; 1024];
	let base = pmem.as_mut_ptr();
	let mut recorder = Recorder { steps: Vec::new(), pmem: base, expect: Vec::new() };
	assert!(unsafe { write_durably(base, 1024, 512, &[7u8; 512], Durability::Visible, LINE, &mut recorder) });
	assert_eq!(recorder.steps, alloc::vec![Step::Fence]);
	assert!(pmem[512..].iter().all(|byte| *byte == 7));
	let mut refused = Recorder { steps: Vec::new(), pmem: base, expect: Vec::new() };
	assert!(!unsafe { write_durably(base, 1024, 1000, &[9u8; 512], Durability::Flushed, LINE, &mut refused) }, "a span past the namespace");
	assert!(refused.steps.is_empty() && pmem[1000..].iter().all(|byte| *byte == 7), "is not written, flushed or fenced");
	assert!(!unsafe { write_durably(base, 1024, 0, &[1u8; 16], Durability::Flushed, 48, &mut refused) }, "a line size that is not a power of two is no line");
	let mut out = [0u8; 16];
	assert!(unsafe { read(base, 1024, 1008, &mut out) } && out == [7u8; 16]);
	assert!(!unsafe { read(base, 1024, 1012, &mut out) }, "a read past the namespace");
}

#[test]
fn the_namespace_record_is_read_out_of_the_row_s_one_value_record() {
	let namespace = namespace(nfit::STATE_NOT_ARMED, Some(nfit::CAPABILITY_MEMORY_CONTROLLER_FLUSH));
	let mut block = Vec::new();
	block.push(rt::DEVICE_PROPERTY_VALUE);
	block.push(0);
	block.extend_from_slice(&4u16.to_le_bytes());
	block.extend_from_slice(&(nfit::RECORD_LEN as u32).to_le_bytes());
	block.extend_from_slice(b"NFIT");
	block.extend_from_slice(&namespace.encode());
	assert_eq!(record(&block), Some(namespace));
	let mut renamed = block.clone();
	renamed[8..12].copy_from_slice(b"WDAT");
	assert_eq!(record(&renamed), None, "another table's record");
	assert_eq!(record(&block[..block.len() - 1]), None, "a cut record");
}
