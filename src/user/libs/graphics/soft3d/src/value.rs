//! THE RUNTIME VALUE OF A SHADER, AND WHY IT IS A BAG OF WORDS.
//!
//! ONE REPRESENTATION FOR EVERY IR TYPE. A `bool`, an `i32`, a `u32`, an `f32`, their vectors, a
//! matrix and an array are all a type plus a sequence of 32-bit words here. The alternative - an
//! enumeration with a variant per shape - makes every operation a nested match over two operand
//! shapes, which is where an interpreter silently does the wrong thing for one combination.
//!
//! THE WORDS ARE BIT PATTERNS AND NOT NUMBERS. An `f32` is stored as its bits, so `-0.0` stays
//! `-0.0` and a NaN keeps its payload - which the frozen encoding rules require in those words,
//! because an interpreter that normalised either would change the sign of a division and the
//! outcome of a comparison.

use alloc::vec::Vec;

use render_shader::{ScalarType, Type};

/// The largest value that fits inline: a `mat4`, which is the widest thing an IR type can be that is
/// not an array.
///
/// INLINE UP TO HERE AND ON THE HEAP BEYOND IT. A shader assigns a value per instruction and a
/// fragment stage runs once per covered pixel, so a `Vec` per value is an allocation per instruction
/// per pixel - millions per frame, and a frame whose cost depends on the allocator rather than on
/// the geometry. Scalars, vectors and matrices are every value a shader computes; an ARRAY is a
/// uniform a shader loads, which happens once rather than per pixel, so that is where the fallback
/// belongs.
///
/// AND IT IS SIXTEEN RATHER THAN FOUR, WHICH WAS MEASURED AND REJECTED. A fragment stage computes
/// scalars and vectors, so four words would hold every value it produces and would make a `Val` half
/// the size; the benchmark said that was worth about six percent. What it would also do is push
/// every `mat4` onto the heap, and a vertex stage reads two matrix uniforms per vertex - so the
/// saving is bought with an allocation per vertex per frame, against a crate whose stated property
/// is that a warmed frame asks the allocator for nothing. Six percent is not what that is worth.
pub const INLINE_WORDS: usize = 16;

/// Where a value's words live.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Words {
	Inline([u32; INLINE_WORDS], u8),
	Heap(Vec<u32>),
}

impl Words {
	fn from_slice(words: &[u32]) -> Self {
		if words.len() <= INLINE_WORDS {
			let mut inline = [0; INLINE_WORDS];
			inline[..words.len()].copy_from_slice(words);
			return Self::Inline(inline, words.len() as u8);
		}
		Self::Heap(words.to_vec())
	}

	fn as_slice(&self) -> &[u32] {
		match self {
			Self::Inline(words, len) => &words[..*len as usize],
			Self::Heap(words) => words,
		}
	}
}

/// A value the interpreter holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Val {
	pub kind: Type,
	/// Row-major for a vector, COLUMN-MAJOR for a matrix, and element after element for an array -
	/// the same storage order the rest of this stack uses, stated so a caller that builds a uniform
	/// by hand cannot get it backwards.
	words: Words,
}

impl Val {
	/// Build from words, inline when they fit.
	pub fn new(kind: Type, words: &[u32]) -> Self {
		Self { kind, words: Words::from_slice(words) }
	}

	/// The words, whichever side of the bound they are on.
	pub fn words(&self) -> &[u32] {
		self.words.as_slice()
	}
}

/// A value AS THE REGISTER FILE HOLDS IT: its declared type and its words, borrowed in place.
///
/// THE TYPE IS THE MODULE'S AND NOT A COPY OF IT, which is the whole of why this exists. Every value
/// a shader assigns has a type the module already states and which cannot change between runs, so a
/// runtime carrying one per value copies that type once per instruction per fragment - and a `Val` is
/// ninety bytes of which sixteen are exactly that. A `Reg` is two pointers and a length.
#[derive(Clone, Copy)]
pub struct Reg<'a> {
	pub kind: &'a Type,
	pub words: &'a [u32],
}

impl<'a> Reg<'a> {
	pub const fn new(kind: &'a Type, words: &'a [u32]) -> Self {
		Self { kind, words }
	}

	pub fn scalar_type(&self) -> ScalarType {
		scalar_type_of(self.kind)
	}

	pub fn len(&self) -> usize {
		self.words.len()
	}

	pub fn is_empty(&self) -> bool {
		self.words.is_empty()
	}

	pub fn f32_at(&self, index: usize) -> f32 {
		f32::from_bits(self.words.get(index).copied().unwrap_or(0))
	}

	pub fn i32_at(&self, index: usize) -> i32 {
		self.words.get(index).copied().unwrap_or(0) as i32
	}

	pub fn u32_at(&self, index: usize) -> u32 {
		self.words.get(index).copied().unwrap_or(0)
	}

	pub fn bool_at(&self, index: usize) -> bool {
		self.words.get(index).copied().unwrap_or(0) != 0
	}

	/// The first four components as `f32`, which is what the vector arithmetic reads.
	pub fn to_f32(&self) -> [f32; 4] {
		let mut out = [0.0; 4];
		for (index, slot) in out.iter_mut().enumerate() {
			*slot = self.f32_at(index);
		}
		out
	}

	/// An owned value, for the two places that genuinely need one: a stage OUTPUT, which the caller
	/// keeps after the run, and a resource read that answers one.
	pub fn to_val(&self) -> Val {
		Val::new(self.kind.clone(), self.words)
	}
}

/// The scalar a type is made of, WITHOUT BUILDING A VALUE to ask. An array answers its element's.
pub fn scalar_type_of(kind: &Type) -> ScalarType {
	match kind {
		Type::Scalar(scalar) | Type::Vector(scalar, _) => *scalar,
		Type::Matrix(_) => ScalarType::F32,
		Type::Array(element, _) => scalar_type_of(element),
	}
}

impl Val {
	pub fn scalar_f32(value: f32) -> Self {
		Self::new(Type::f32(), &[value.to_bits()])
	}

	pub fn scalar_i32(value: i32) -> Self {
		Self::new(Type::Scalar(ScalarType::I32), &[value as u32])
	}

	pub fn scalar_u32(value: u32) -> Self {
		Self::new(Type::Scalar(ScalarType::U32), &[value])
	}

	pub fn scalar_bool(value: bool) -> Self {
		Self::new(Type::Scalar(ScalarType::Bool), &[u32::from(value)])
	}

	pub fn vector_f32(components: &[f32]) -> Self {
		let mut words = [0_u32; INLINE_WORDS];
		let count = components.len().min(INLINE_WORDS);
		for (slot, value) in words[..count].iter_mut().zip(components) {
			*slot = value.to_bits();
		}
		Self { kind: Type::Vector(ScalarType::F32, components.len() as u8), words: Words::from_slice(&words[..count]) }
	}

	/// A square matrix from its COLUMNS.
	pub fn matrix(columns: &[[f32; 4]], size: u8) -> Self {
		let mut words = [0_u32; INLINE_WORDS];
		let mut count = 0;
		for column in columns.iter().take(size as usize) {
			for component in column.iter().take(size as usize) {
				if count < INLINE_WORDS {
					words[count] = component.to_bits();
					count += 1;
				}
			}
		}
		Self { kind: Type::Matrix(size), words: Words::from_slice(&words[..count]) }
	}

	/// The scalar type of every word.
	pub fn scalar_type(&self) -> ScalarType {
		match &self.kind {
			Type::Scalar(scalar) | Type::Vector(scalar, _) => *scalar,
			Type::Matrix(_) => ScalarType::F32,
			Type::Array(element, _) => Self::new((**element).clone(), &[]).scalar_type(),
		}
	}

	pub fn len(&self) -> usize {
		self.words().len()
	}

	pub fn is_empty(&self) -> bool {
		self.words().is_empty()
	}

	pub fn f32_at(&self, index: usize) -> f32 {
		f32::from_bits(self.words().get(index).copied().unwrap_or(0))
	}

	pub fn i32_at(&self, index: usize) -> i32 {
		self.words().get(index).copied().unwrap_or(0) as i32
	}

	pub fn u32_at(&self, index: usize) -> u32 {
		self.words().get(index).copied().unwrap_or(0)
	}

	pub fn bool_at(&self, index: usize) -> bool {
		self.words().get(index).copied().unwrap_or(0) != 0
	}

	/// Every component as `f32`, whatever the scalar type, WITHOUT COLLECTING - which is what an
	/// interpolation or a colour write needs, once per varying per vertex.
	pub fn f32_components(&self) -> impl Iterator<Item = f32> + '_ {
		(0..self.words().len()).map(|index| match self.scalar_type() {
			ScalarType::F32 => self.f32_at(index),
			ScalarType::I32 => self.i32_at(index) as f32,
			ScalarType::U32 => self.u32_at(index) as f32,
			ScalarType::Bool => f32::from(self.bool_at(index)),
		})
	}

	/// Every component as `f32`, whatever the scalar type - which is what an interpolation or a
	/// colour write needs.
	pub fn to_f32(&self) -> Vec<f32> {
		(0..self.words().len())
			.map(|index| match self.scalar_type() {
				ScalarType::F32 => self.f32_at(index),
				ScalarType::I32 => self.i32_at(index) as f32,
				ScalarType::U32 => self.u32_at(index) as f32,
				ScalarType::Bool => f32::from(self.bool_at(index)),
			})
			.collect()
	}

	/// How many words a type occupies. A TYPE WITH NO SIZE IS NOT ONE, so an array of length zero
	/// answers zero rather than being a special case elsewhere.
	pub fn words_in(kind: &Type) -> usize {
		match kind {
			Type::Scalar(_) => 1,
			Type::Vector(_, components) => *components as usize,
			Type::Matrix(size) => (*size as usize) * (*size as usize),
			Type::Array(element, length) => Self::words_in(element) * *length as usize,
		}
	}
}
