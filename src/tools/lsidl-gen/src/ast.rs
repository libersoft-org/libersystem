//! The LSIDL abstract syntax tree.

use crate::token::Span;

// A whole parsed `.lsidl` file: one package, its imports, and its declarations.
#[derive(Clone, Debug)]
pub struct File {
	pub package: Package,
	pub package_doc: Vec<Doc>,
	pub uses: Vec<Use>,
	pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub struct Doc {
	pub text: String,
	pub span: Span,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Evolution {
	pub since: Option<u32>,
	pub deprecated: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Package {
	pub path: Vec<String>,
	pub version: u32,
	// Kept for diagnostics once the generator reports package-level issues.
	#[allow(dead_code)]
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Use {
	pub path: Vec<String>,
	pub version: u32,
	pub names: Vec<ImportName>,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ImportName {
	pub name: String,
	pub alias: Option<String>,
	pub span: Span,
	pub alias_span: Option<Span>,
}

impl ImportName {
	pub fn local_name(&self) -> &str {
		self.alias.as_deref().unwrap_or(&self.name)
	}
}

// A top-level declaration.
#[derive(Clone, Debug)]
pub enum Item {
	Alias(Alias),
	Record(Record),
	Enum(Enum),
	Variant(Variant),
	Flags(Flags),
	Resource(Resource),
	Interface(Interface),
}

#[derive(Clone, Debug)]
pub struct Alias {
	pub name: String,
	pub ty: Type,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Record {
	pub name: String,
	pub fields: Vec<Field>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Field {
	pub name: String,
	pub ty: Type,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Enum {
	pub name: String,
	pub cases: Vec<EnumCase>,
	pub reserved: Vec<u32>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumCase {
	pub name: String,
	pub ordinal: Option<u32>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Variant {
	pub name: String,
	pub cases: Vec<VarCase>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct VarCase {
	pub name: String,
	pub payload: Option<Type>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Flags {
	pub name: String,
	pub flags: Vec<FlagCase>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FlagCase {
	pub name: String,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Resource {
	pub name: String,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Interface {
	pub name: String,
	pub methods: Vec<Method>,
	pub reserved: Vec<u32>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Method {
	pub name: String,
	pub op: u32,
	pub params: Vec<Param>,
	pub ret: Type,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
	pub name: String,
	pub ty: Type,
	pub rights: Vec<String>,
	pub doc: Vec<Doc>,
	pub evolution: Evolution,
	pub span: Span,
}

// A type reference. Generic arguments are boxed so the enum stays sized.
#[derive(Clone, Debug)]
pub enum Type {
	Bool,
	U8,
	U16,
	U32,
	U64,
	I8,
	I16,
	I32,
	I64,
	F32,
	F64,
	String,
	Unit,
	Buffer,
	Option(Box<Type>),
	List(Box<Type>),
	Stream(Box<Type>),
	Tuple(Vec<Type>),
	Result(Box<Type>, Box<Type>),
	Handle(String),
	Named(String),
}
