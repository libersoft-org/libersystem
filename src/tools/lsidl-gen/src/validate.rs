//! Semantic validation of a parsed LSIDL file.
//!
//! The parser guarantees the file is syntactically well formed; this pass checks
//! the meaning: names are unique and resolvable, opcodes and ordinals are unique
//! and in range, handles refer to resources, and `@rights` name real rights. All
//! problems are collected so a single run reports every error.

use crate::ast::*;
use crate::token::{Error, Span};
use std::collections::HashMap;
use std::collections::HashSet;

// The kind of a top-level name, used to resolve type references.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	// A record / enum / variant / flags, or an interface name.
	Type,
	Resource,
	// Imported via `use`; its concrete kind lives in another file, so it is
	// accepted wherever a type or a resource is expected.
	External,
}

// The capability rights `@rights(...)` may name (mirrors abi RIGHT_* bits).
const RIGHTS: &[&str] = &["read", "write", "execute", "map", "send", "receive", "duplicate", "transfer", "revoke", "get-info", "manage", "wait"];

// Resources the kernel provides without a declaration.
const BUILTIN_RESOURCES: &[&str] = &["channel"];

// Validate a file, returning every error found (empty = valid).
pub fn validate(file: &File) -> Vec<Error> {
	let mut errs = Vec::new();
	let mut names: HashMap<String, Kind> = HashMap::new();

	for r in BUILTIN_RESOURCES {
		names.insert((*r).to_string(), Kind::Resource);
	}
	for u in &file.uses {
		for n in &u.names {
			define(&mut names, n, Kind::External, u.span, &mut errs);
		}
	}
	for item in &file.items {
		let (name, span, kind) = match item {
			Item::Record(r) => (&r.name, r.span, Kind::Type),
			Item::Enum(e) => (&e.name, e.span, Kind::Type),
			Item::Variant(v) => (&v.name, v.span, Kind::Type),
			Item::Flags(f) => (&f.name, f.span, Kind::Type),
			Item::Resource(r) => (&r.name, r.span, Kind::Resource),
			Item::Interface(i) => (&i.name, i.span, Kind::Type),
		};
		define(&mut names, name, kind, span, &mut errs);
	}

	for item in &file.items {
		match item {
			Item::Record(r) => check_record(r, &names, &mut errs),
			Item::Enum(e) => check_enum(e, &mut errs),
			Item::Variant(v) => check_variant(v, &names, &mut errs),
			Item::Flags(f) => check_flags(f, &mut errs),
			Item::Resource(_) => {}
			Item::Interface(i) => check_interface(i, &names, &mut errs),
		}
	}

	errs
}

fn define(names: &mut HashMap<String, Kind>, name: &str, kind: Kind, span: Span, errs: &mut Vec<Error>) {
	if names.contains_key(name) {
		errs.push(Error::new(span, format!("`{name}` is already defined")));
	} else {
		names.insert(name.to_string(), kind);
	}
}

fn check_record(r: &Record, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	let mut seen = HashSet::new();
	for f in &r.fields {
		if !seen.insert(f.name.as_str()) {
			errs.push(Error::new(f.span, format!("duplicate field `{}` in record `{}`", f.name, r.name)));
		}
		check_type(&f.ty, f.span, names, errs);
	}
}

fn check_enum(e: &Enum, errs: &mut Vec<Error>) {
	let mut seen_names = HashSet::new();
	let mut used: HashSet<u32> = HashSet::new();
	let mut next = 0u32;
	for c in &e.cases {
		if !seen_names.insert(c.name.as_str()) {
			errs.push(Error::new(c.span, format!("duplicate case `{}` in enum `{}`", c.name, e.name)));
		}
		let ord = c.ordinal.unwrap_or(next);
		if ord > 255 {
			errs.push(Error::new(c.span, format!("enum `{}` ordinal {ord} does not fit in u8", e.name)));
		}
		if !used.insert(ord) {
			errs.push(Error::new(c.span, format!("enum `{}` reuses ordinal {ord}", e.name)));
		}
		next = ord.wrapping_add(1);
	}
	for r in &e.reserved {
		if used.contains(r) {
			errs.push(Error::new(e.span, format!("enum `{}` reserves ordinal {r} that is also in use", e.name)));
		}
	}
}

fn check_variant(v: &Variant, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	let mut seen = HashSet::new();
	for c in &v.cases {
		if !seen.insert(c.name.as_str()) {
			errs.push(Error::new(c.span, format!("duplicate case `{}` in variant `{}`", c.name, v.name)));
		}
		if let Some(t) = &c.payload {
			check_type(t, c.span, names, errs);
		}
	}
	if v.cases.len() > 256 {
		errs.push(Error::new(v.span, format!("variant `{}` has more than 256 cases (u8 tag)", v.name)));
	}
}

fn check_flags(f: &Flags, errs: &mut Vec<Error>) {
	let mut seen = HashSet::new();
	for name in &f.flags {
		if !seen.insert(name.as_str()) {
			errs.push(Error::new(f.span, format!("duplicate flag `{name}` in flags `{}`", f.name)));
		}
	}
	if f.flags.len() > 64 {
		errs.push(Error::new(f.span, format!("flags `{}` has more than 64 members (max u64 bitset)", f.name)));
	}
}

fn check_interface(i: &Interface, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	let mut seen_names = HashSet::new();
	let mut used_ops: HashSet<u32> = HashSet::new();
	for m in &i.methods {
		if !seen_names.insert(m.name.as_str()) {
			errs.push(Error::new(m.span, format!("duplicate method `{}` in interface `{}`", m.name, i.name)));
		}
		if m.op == 0 || m.op > 65535 {
			errs.push(Error::new(m.span, format!("opcode {} for `{}` must be in 1..=65535", m.op, m.name)));
		}
		if !used_ops.insert(m.op) {
			errs.push(Error::new(m.span, format!("interface `{}` reuses opcode {}", i.name, m.op)));
		}
		let mut seen_params = HashSet::new();
		for p in &m.params {
			if !seen_params.insert(p.name.as_str()) {
				errs.push(Error::new(p.span, format!("duplicate parameter `{}` in `{}`", p.name, m.name)));
			}
			for r in &p.rights {
				if !RIGHTS.contains(&r.as_str()) {
					errs.push(Error::new(p.span, format!("unknown right `{r}` in `@rights`")));
				}
			}
			check_type(&p.ty, p.span, names, errs);
		}
		check_type(&m.ret, m.span, names, errs);
	}
	for r in &i.reserved {
		if used_ops.contains(r) {
			errs.push(Error::new(i.span, format!("interface `{}` reserves opcode {r} that is also in use", i.name)));
		}
	}
}

fn check_type(ty: &Type, span: Span, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	match ty {
		Type::Option(t) | Type::List(t) | Type::Stream(t) => check_type(t, span, names, errs),
		Type::Tuple(ts) => {
			for t in ts {
				check_type(t, span, names, errs);
			}
		}
		Type::Result(a, b) => {
			check_type(a, span, names, errs);
			check_type(b, span, names, errs);
		}
		Type::Handle(res) => match names.get(res) {
			Some(Kind::Resource) | Some(Kind::External) => {}
			Some(_) => errs.push(Error::new(span, format!("`handle<{res}>` requires `{res}` to be a resource"))),
			None => errs.push(Error::new(span, format!("unknown resource `{res}` in `handle<{res}>`"))),
		},
		Type::Named(name) => match names.get(name) {
			Some(Kind::Type) | Some(Kind::External) => {}
			Some(Kind::Resource) => errs.push(Error::new(span, format!("`{name}` is a resource; pass it as `handle<{name}>`"))),
			None => errs.push(Error::new(span, format!("unknown type `{name}`"))),
		},
		_ => {}
	}
}
