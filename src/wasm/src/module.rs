// The parsed shape of a WebAssembly module - the integer subset the LiberSystem
// runtime supports. Function indices follow the wasm convention: imported
// functions come first, then the module's own defined functions.

use alloc::string::String;
use alloc::vec::Vec;

// A value type. The runtime handles the 32/64-bit integers and floats; reference
// types are out of scope for the minimal host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValType {
	I32,
	I64,
	F32,
	F64,
}

// A function signature: the parameter and result value types.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FuncType {
	pub params: Vec<ValType>,
	pub results: Vec<ValType>,
}

// An imported function: the module and field it is imported under (e.g. "liber" /
// "read"), and the index of its signature in the type section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
	pub module: String,
	pub field: String,
	pub type_index: u32,
}

// A defined function: its signature (a type-section index), the value types of its
// declared locals, and the raw instruction bytes the interpreter executes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Func {
	pub type_index: u32,
	pub locals: Vec<ValType>,
	pub body: Vec<u8>,
}

// The kind of item an export names. Only functions and memory are tracked; any
// other export kind is recorded as `Other` and ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
	Func,
	Memory,
	Other,
}

// An export: a name bound to an item by index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
	pub name: String,
	pub kind: ExportKind,
	pub index: u32,
}

// A defined global: its value type, whether it is mutable, and its constant
// initial value (held as a 64-bit pattern; an i32 global uses the low 32 bits, and
// a float global stores its IEEE-754 bits). The minimal runtime supports only
// constant init expressions (`i32.const` / `i64.const` / `f32.const` / `f64.const`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
	pub val_type: ValType,
	pub mutable: bool,
	pub init: i64,
}

// An active data segment: raw bytes copied into linear memory at `offset` when the
// module is instantiated. Passive segments are out of scope for the minimal runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSegment {
	pub offset: u32,
	pub bytes: Vec<u8>,
}

// A parsed module. `memory_min_pages` is the declared minimum of the single memory
// (0 if the module declares none); one page is 64 kB.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
	pub types: Vec<FuncType>,
	pub imports: Vec<Import>,
	pub funcs: Vec<Func>,
	pub exports: Vec<Export>,
	pub memory_min_pages: u32,
	pub globals: Vec<Global>,
	pub data: Vec<DataSegment>,
}

impl Module {
	// The number of imported functions (the size of the leading import index space).
	pub fn import_count(&self) -> u32 {
		self.imports.len() as u32
	}

	// The signature of function `index` in the combined (imports then defined) space.
	pub fn func_type(&self, index: u32) -> Option<&FuncType> {
		let i = index as usize;
		let type_index = if i < self.imports.len() { self.imports[i].type_index } else { self.funcs.get(i - self.imports.len())?.type_index };
		self.types.get(type_index as usize)
	}

	// The combined index of an exported function, by name.
	pub fn export_func(&self, name: &str) -> Option<u32> {
		self.exports.iter().find(|e: &&Export| e.kind == ExportKind::Func && e.name == name).map(|e: &Export| e.index)
	}
}
