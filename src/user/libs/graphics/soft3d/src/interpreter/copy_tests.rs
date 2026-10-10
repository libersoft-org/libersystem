use super::*;
use alloc::{boxed::Box, vec};
struct Empty;
impl Resources for Empty {
	fn uniform(&self, _: u32, _: u32) -> Option<Val> {
		None
	}
	fn attribute(&self, _: u32) -> Option<Val> {
		None
	}
	fn varying(&self, _: u32) -> Option<Val> {
		None
	}
	fn built_in(&self, _: render_shader::ir::BuiltIn) -> Option<Val> {
		None
	}
	fn sample(&self, _: u32, _: u32, _: &Val) -> Option<Val> {
		None
	}
}
fn machine() -> Machine {
	Machine { words: vec![0xdead_beef; 3 * STRIDE], len: vec![0; 3], kinds: vec![Type::f32(); 3], wide: vec![None; 3], stamp: vec![0; 3], run: 9, outputs: Outputs::default() }
}
#[test]
fn writes_preserve_exact_words_type_neighbours_and_array_transitions() {
	let patterns = [0x80000000, 0x7fc12345, 0xff800000, 0xffffffff, 0x01000001, 0, 0x7f800000];
	let mut storage = machine();
	let mut run = Run { resources: &Empty, machine: &mut storage };
	for length in [33, 1, 2, 3, 4, 5, 9, 16, 17, 0, 4] {
		let words: Vec<u32> = (0..length).map(|i| patterns[i % patterns.len()]).collect();
		let kind = match length {
			1 => Type::f32(),
			2..=4 => Type::vec(length as u8),
			9 => Type::Matrix(3),
			16 => Type::Matrix(4),
			_ => Type::Array(Box::new(Type::Scalar(ScalarType::U32)), length as u32),
		};
		assert_eq!(run.put(1, kind.clone(), &words), Ok(()));
		let actual = run.read(Value(1)).unwrap();
		assert_eq!(actual.words, words.as_slice());
		assert_eq!(actual.kind, &kind);
		assert!(run.machine.words[..STRIDE].iter().all(|v| *v == 0xdead_beef));
		assert!(run.machine.words[2 * STRIDE..].iter().all(|v| *v == 0xdead_beef));
		assert_eq!(run.machine.wide[1].is_some(), length > STRIDE);
	}
	run.machine.run += 1;
	assert!(matches!(run.read(Value(1)), Err(Fault::Unassigned { value: 1 })));
}
#[test]
fn refused_slot_leaves_previous_words_and_stamp_intact() {
	let mut storage = machine();
	let mut run = Run { resources: &Empty, machine: &mut storage };
	run.put(1, Type::vec(4), &[1, 2, 3, 4]).unwrap();
	let words = run.machine.words.clone();
	let stamps = run.machine.stamp.clone();
	assert_eq!(run.put(3, Type::f32(), &[0x7fc12345]), Err(Fault::TypeMismatch));
	assert_eq!(run.machine.words, words);
	assert_eq!(run.machine.stamp, stamps);
	assert_eq!(run.read(Value(1)).unwrap().words, &[1, 2, 3, 4]);
}
#[test]
fn repeated_scalar_vector_and_matrix_register_writes_allocate_nothing() {
	let mut storage = machine();
	let mut run = Run { resources: &Empty, machine: &mut storage };
	let words = [0x7fc12345; STRIDE];
	let before = crate::counted::count();
	for _ in 0..32 {
		for length in [1, 2, 3, 4, 9, 16] {
			let kind = match length {
				1 => Type::f32(),
				2..=4 => Type::vec(length as u8),
				9 => Type::Matrix(3),
				_ => Type::Matrix(4),
			};
			run.put(1, kind, &words[..length]).unwrap();
			assert_eq!(run.read(Value(1)).unwrap().words, &words[..length]);
		}
	}
	assert_eq!(crate::counted::count(), before);
}
