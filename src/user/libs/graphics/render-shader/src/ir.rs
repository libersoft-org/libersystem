//! THE IR ITSELF: the types, the operations, the control flow and the interface a stage has.
//!
//! EVERY ENUMERATION HERE IS CLOSED, which is what "a validated portable IR" means once it is more
//! than a description: a backend implements every variant, and an operation outside the set is not
//! "unsupported" - there is no way to write one.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// The scalar types. Four, and no more: a portable IR with an implementation-defined integer width
/// is not portable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScalarType {
	Bool,
	I32,
	U32,
	F32,
}

/// A type in the IR.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Type {
	Scalar(ScalarType),
	/// Two, three or four components of one scalar type.
	Vector(ScalarType, u8),
	/// A square matrix of `f32`: 2, 3 or 4. COLUMN-MAJOR, like everything else in this stack.
	Matrix(u8),
	/// An array with a COMPILE-TIME length. A runtime length would make the bound of a loop over it
	/// unknowable, which is the thing this IR refuses.
	Array(Box<Type>, u32),
}

impl Type {
	pub const fn f32() -> Self {
		Self::Scalar(ScalarType::F32)
	}

	pub const fn vec(components: u8) -> Self {
		Self::Vector(ScalarType::F32, components)
	}

	/// The scalar a type is made of, or `None` for an array.
	pub fn component(&self) -> Option<ScalarType> {
		match self {
			Self::Scalar(scalar) | Self::Vector(scalar, _) => Some(*scalar),
			Self::Matrix(_) => Some(ScalarType::F32),
			Self::Array(..) => None,
		}
	}

	/// How many components a value of this type has, for the operations that care.
	pub fn width(&self) -> Option<u8> {
		match self {
			Self::Scalar(_) => Some(1),
			Self::Vector(_, components) => Some(*components),
			Self::Matrix(order) => Some(*order),
			Self::Array(..) => None,
		}
	}

	pub fn is_float(&self) -> bool {
		matches!(self.component(), Some(ScalarType::F32))
	}
}

/// A value in the module: a single static assignment, referred to by index.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Value(pub u32);

/// A compile-time constant.
///
/// A FLOAT CONSTANT IS ITS EXACT BITS. `-0.0` is itself and is not normalised to `0.0`, because an
/// encoder that normalised it would change the sign of a division - which the frozen encoding rules
/// say in the same words.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Constant {
	Bool(bool),
	I32(i32),
	U32(u32),
	F32(f32),
}

impl Constant {
	pub const fn scalar_type(self) -> ScalarType {
		match self {
			Self::Bool(_) => ScalarType::Bool,
			Self::I32(_) => ScalarType::I32,
			Self::U32(_) => ScalarType::U32,
			Self::F32(_) => ScalarType::F32,
		}
	}
}

/// How a varying is interpolated between the stages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interpolation {
	Smooth,
	NoPerspective,
	Flat,
}

/// WHERE a varying is evaluated, which the profile keeps SEPARATE from how it is interpolated.
///
/// `centroid` AND `sample` ARE LOCATIONS AND NOT RULES. The frozen profile says it in those words:
/// each "chooses an evaluation LOCATION and does not change the interpolation rule". Folding them
/// into `Interpolation` would make `centroid flat` and `sample flat` expressible, which are
/// meaningless - a constant has no location dependence - and would make `centroid smooth` and
/// `centroid noperspective` two unrelated variants instead of one modifier on two rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sampling {
	/// The pixel centre. At an edge this can lie OUTSIDE the primitive, and extrapolating a texture
	/// coordinate there samples outside the surface - which is the shimmer along silhouettes that
	/// multisampling is supposed to remove.
	#[default]
	Pixel,
	/// A point inside the COVERED part of the pixel, which is what removes that shimmer.
	Centroid,
	/// Once per covered SAMPLE. This is what makes per-sample shading mean anything: a fragment
	/// stage run once per pixel cannot produce different values at two samples of it.
	Sample,
}

/// Where a loaded value comes from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Binding {
	/// A uniform, by its block and its member index.
	Uniform { block: u32, member: u32 },
	/// A vertex attribute, by the location the vertex layout feeds.
	Attribute { location: u32 },
	/// A varying from the previous stage.
	Varying { location: u32 },
	/// A built-in the stage is given.
	BuiltIn(BuiltIn),
	/// A texture and the sampler it is read through.
	Texture { texture: u32, sampler: u32 },
}

/// The built-ins, per stage. Named rather than numbered, because a numeric built-in id is a thing
/// two implementations disagree about silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuiltIn {
	VertexIndex,
	InstanceIndex,
	BaseVertex,
	BaseInstance,
	FragmentCoordinate,
	FrontFacing,
	SampleIndex,
	SampleMaskIn,
}

/// What a stage writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
	/// THE VERTEX POSITION. The one output every StrictF32 rule is about.
	Position,
	PointSize,
	/// A varying, by location.
	Varying(u32),
	/// A colour attachment, by index.
	Colour(u32),
	/// AN INTEGER ATTACHMENT, for object ids. In the profile because picking is not optional in an
	/// application, and every implementation that leaves it out grows a worse version of it.
	Integer(u32),
	Depth,
	SampleMask,
}

/// A unary operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnaryOp {
	Negate,
	Not,
	/// Bitwise complement, on the integer types.
	Complement,
	Normalize,
	Length,
	Abs,
	Floor,
	Ceil,
	Fract,
	/// A conversion to the named scalar type, component-wise.
	Convert(ScalarType),
}

/// A binary operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinaryOp {
	Add,
	Subtract,
	Multiply,
	Divide,
	Modulo,
	And,
	Or,
	Xor,
	ShiftLeft,
	ShiftRight,
	Min,
	Max,
	Dot,
	Cross,
	/// A matrix times a vector, or a matrix times a matrix. One operation rather than two, because
	/// the operand types decide which and a second name would be a second place to check them.
	MatrixProduct,
}

/// A comparison, which answers a `bool` whatever its operands are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompareKind {
	Equal,
	NotEqual,
	Less,
	LessOrEqual,
	Greater,
	GreaterOrEqual,
}

/// A transcendental. SEPARATE FROM `BinaryOp` AND `UnaryOp` because these are the operations whose
/// ACCURACY the profile bounds, and a position may depend only on the ones bounded at zero ULP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transcendental {
	Sqrt,
	InverseSqrt,
	Sin,
	Cos,
	Tan,
	Asin,
	Acos,
	Atan,
	Atan2,
	Exp,
	Exp2,
	Log,
	Log2,
	Pow,
}

impl Transcendental {
	/// The name the frozen accuracy table uses.
	pub const fn name(self) -> &'static str {
		match self {
			Self::Sqrt => "sqrt",
			Self::InverseSqrt => "inversesqrt",
			Self::Sin | Self::Cos => "sin, cos",
			Self::Tan => "tan",
			Self::Asin | Self::Acos | Self::Atan => "asin, acos, atan",
			Self::Atan2 => "atan2",
			Self::Exp | Self::Exp2 => "exp, exp2",
			Self::Log | Self::Log2 => "log, log2",
			Self::Pow => "pow",
		}
	}

	/// The ULP bound the profile freezes for this operation, READ FROM THE TABLE rather than
	/// restated - so an accuracy corrected in the profile corrects this without a second edit.
	pub fn max_ulp(self) -> Option<u32> {
		graphics_profile::shader_ir::TRANSCENDENTAL_ACCURACY.iter().find(|entry| entry.operation == self.name()).map(|entry| entry.max_ulp)
	}

	/// Whether a POSITION may depend on this operation.
	///
	/// ONLY WITH A STRICT DEFINITION, which is what a zero-ULP bound is: `sqrt` is correctly rounded
	/// and is allowed; `sin` has a four-ULP bound and is NOT deterministic across backends, so a
	/// position depending on it is refused at module load.
	pub fn strict(self) -> bool {
		self.max_ulp() == Some(0)
	}
}

/// One operation that produces a value.
#[derive(Clone, PartialEq, Debug)]
pub enum Op {
	Const(Constant),
	Load(Binding),
	/// Build a vector from components.
	Compose(Type, Vec<Value>),
	/// One component of a vector or one column of a matrix.
	Extract(Value, u8),
	Unary(UnaryOp, Value),
	Binary(BinaryOp, Value, Value),
	Compare(CompareKind, Value, Value),
	/// `condition ? on_true : on_false`, component-wise for a vector condition.
	Select {
		condition: Value,
		on_true: Value,
		on_false: Value,
	},
	Transcendental(Transcendental, Value, Option<Value>),
	Clamp {
		value: Value,
		low: Value,
		high: Value,
	},
	Mix {
		from: Value,
		to: Value,
		at: Value,
	},
	/// An array element. THE INDEX IS ITSELF A VALUE and is on whatever dependency path the result
	/// is, which is why an index on a position path is strict.
	Index {
		array: Value,
		index: Value,
	},
	/// A texture read.
	Sample {
		binding: Binding,
		coordinate: Value,
	},
}

/// A statement. STRUCTURED, with no label and no jump.
#[derive(Clone, PartialEq, Debug)]
pub enum Stmt {
	/// Assign a value. SSA: each `Value` is assigned exactly once.
	Assign(Value, Op),
	Store(Output, Value),
	If {
		condition: Value,
		then_body: Vec<Stmt>,
		else_body: Vec<Stmt>,
	},
	Switch {
		selector: Value,
		cases: Vec<(i32, Vec<Stmt>)>,
		default: Vec<Stmt>,
	},
	/// A LOOP CARRIES ITS TRIP COUNT. There is no way to write one without a bound, which is what
	/// makes a frame's worst case computable and an unbounded loop unrepresentable rather than
	/// refused.
	Loop {
		trips: u32,
		body: Vec<Stmt>,
	},
	Break,
	Continue,
	Return,
	/// Fragment only. A discarded fragment writes nothing - not depth, not stencil, not colour.
	Discard,
}

/// Which stage a body is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
	Vertex,
	Fragment,
}

/// One declared varying, which both stages have to agree about.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Varying {
	pub location: u32,
	pub kind: Type,
	pub interpolation: Interpolation,
	/// Where it is evaluated. `Pixel` unless the shader says otherwise.
	pub sampling: Sampling,
}

/// A module: one stage, its interface, and its body.
#[derive(Clone, PartialEq, Debug)]
pub struct Module {
	pub stage: Stage,
	pub name: String,
	/// The type of every value the body assigns, indexed by the value's own id.
	pub types: Vec<Type>,
	pub varyings: Vec<Varying>,
	pub body: Vec<Stmt>,
}

impl Module {
	/// How often the fragment stage must run, which is a CONSEQUENCE OF THE SHADER and not a state a
	/// caller sets: a stage with a `sample`-qualified input cannot be run once per pixel, because the
	/// input has a different value at each sample.
	pub fn per_sample_shading(&self) -> bool {
		self.varyings.iter().any(|varying| varying.sampling == Sampling::Sample)
	}
}
