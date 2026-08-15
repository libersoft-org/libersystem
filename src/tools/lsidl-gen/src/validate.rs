//! Semantic validation of a parsed LSIDL file.
//!
//! The parser guarantees the file is syntactically well formed; this pass checks
//! the meaning: names are unique and resolvable, opcodes and ordinals are unique
//! and in range, handles refer to resources, and `@rights` name real rights. All
//! problems are collected so a single run reports every error.

use crate::ast::*;
use crate::resolve::{HandleCardinality, ResolvedSymbol, SymbolKind};
use crate::token::{Error, Span};
use std::collections::HashMap;
use std::collections::HashSet;

// The kind of a top-level name, used to resolve type references.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Value,
	Resource,
	Interface,
	// Imported via `use`; its concrete kind lives in another file, so it is
	// accepted wherever a type or a resource is expected.
	External,
}

// The capability rights `@rights(...)` may name (mirrors abi RIGHT_* bits).
const RIGHTS: &[&str] = &["read", "write", "execute", "map", "send", "receive", "duplicate", "transfer", "revoke", "get-info", "manage", "wait"];

// Resources the kernel provides without a declaration.
const BUILTIN_RESOURCES: &[&str] = &["channel"];

// Validate a file, returning every error found (empty = valid).
#[cfg(test)]
pub fn validate(file: &File) -> Vec<Error> {
	validate_impl(file, None)
}

pub fn validate_resolved(file: &File, imports: &HashMap<String, ResolvedSymbol>) -> Vec<Error> {
	validate_impl(file, Some(imports))
}

fn validate_impl(file: &File, imports: Option<&HashMap<String, ResolvedSymbol>>) -> Vec<Error> {
	let mut errs = Vec::new();
	let mut names: HashMap<String, Kind> = HashMap::new();

	for r in BUILTIN_RESOURCES {
		names.insert((*r).to_string(), Kind::Resource);
	}
	for u in &file.uses {
		for n in &u.names {
			let kind = imports
				.and_then(|resolved| resolved.get(n.local_name()))
				.map(|symbol| match symbol.kind {
					SymbolKind::Value => Kind::Value,
					SymbolKind::Resource => Kind::Resource,
					SymbolKind::Interface => Kind::Interface,
				})
				.unwrap_or(Kind::External);
			define(&mut names, n.local_name(), kind, n.alias_span.unwrap_or(n.span), &mut errs);
		}
	}
	for item in &file.items {
		let (name, span, kind) = match item {
			Item::Alias(a) => (&a.name, a.span, Kind::Value),
			Item::Record(r) => (&r.name, r.span, Kind::Value),
			Item::Enum(e) => (&e.name, e.span, Kind::Value),
			Item::Variant(v) => (&v.name, v.span, Kind::Value),
			Item::Flags(f) => (&f.name, f.span, Kind::Value),
			Item::Resource(r) => (&r.name, r.span, Kind::Resource),
			Item::Interface(i) => (&i.name, i.span, Kind::Interface),
		};
		define(&mut names, name, kind, span, &mut errs);
	}

	for item in &file.items {
		match item {
			Item::Alias(a) => {
				check_evolution(a.evolution, file.package.version, a.span, &mut errs);
				check_type(&a.ty, a.span, &names, &mut errs);
			}
			Item::Record(r) => {
				check_evolution(r.evolution, file.package.version, r.span, &mut errs);
				check_record(r, file.package.version, &names, &mut errs);
			}
			Item::Enum(e) => {
				check_evolution(e.evolution, file.package.version, e.span, &mut errs);
				check_enum(e, file.package.version, &mut errs);
			}
			Item::Variant(v) => {
				check_evolution(v.evolution, file.package.version, v.span, &mut errs);
				check_variant(v, file.package.version, &names, &mut errs);
			}
			Item::Flags(f) => {
				check_evolution(f.evolution, file.package.version, f.span, &mut errs);
				check_flags(f, file.package.version, &mut errs);
			}
			Item::Resource(resource) => check_evolution(resource.evolution, file.package.version, resource.span, &mut errs),
			Item::Interface(i) => {
				check_evolution(i.evolution, file.package.version, i.span, &mut errs);
				check_interface(i, file.package.version, &names, &mut errs);
			}
		}
	}
	check_wire_shapes(file, &names, imports, &mut errs);

	errs
}

fn define(names: &mut HashMap<String, Kind>, name: &str, kind: Kind, span: Span, errs: &mut Vec<Error>) {
	if names.contains_key(name) {
		errs.push(Error::new(span, format!("`{name}` is already defined")));
	} else {
		names.insert(name.to_string(), kind);
	}
}

fn check_record(r: &Record, package_version: u32, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	let mut seen = HashSet::new();
	for f in &r.fields {
		check_evolution(f.evolution, package_version, f.span, errs);
		if !seen.insert(f.name.as_str()) {
			errs.push(Error::new(f.span, format!("duplicate field `{}` in record `{}`", f.name, r.name)));
		}
		check_type(&f.ty, f.span, names, errs);
	}
}

fn check_enum(e: &Enum, package_version: u32, errs: &mut Vec<Error>) {
	let mut seen_names = HashSet::new();
	let mut used: HashMap<u32, Span> = HashMap::new();
	let mut next = 0u32;
	for c in &e.cases {
		check_evolution(c.evolution, package_version, c.span, errs);
		if !seen_names.insert(c.name.as_str()) {
			errs.push(Error::new(c.span, format!("duplicate case `{}` in enum `{}`", c.name, e.name)));
		}
		let ord = c.ordinal.unwrap_or(next);
		if ord > 255 {
			errs.push(Error::new(c.span, format!("enum `{}` ordinal {ord} does not fit in u8", e.name)));
		}
		if let Some(first) = used.insert(ord, c.span) {
			errs.push(Error::new(c.span, format!("enum `{}` reuses ordinal {ord}; first declared at {first}", e.name)));
		}
		next = ord.wrapping_add(1);
	}
	for r in &e.reserved {
		if used.contains_key(r) {
			errs.push(Error::new(e.span, format!("enum `{}` reserves ordinal {r} that is also in use", e.name)));
		}
	}
}

fn check_variant(v: &Variant, package_version: u32, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	let mut seen = HashSet::new();
	for c in &v.cases {
		check_evolution(c.evolution, package_version, c.span, errs);
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

fn check_flags(f: &Flags, package_version: u32, errs: &mut Vec<Error>) {
	let mut seen = HashSet::new();
	for flag in &f.flags {
		check_evolution(flag.evolution, package_version, flag.span, errs);
		if !seen.insert(flag.name.as_str()) {
			errs.push(Error::new(flag.span, format!("duplicate flag `{}` in flags `{}`", flag.name, f.name)));
		}
	}
	if f.flags.len() > 64 {
		errs.push(Error::new(f.span, format!("flags `{}` has more than 64 members (max u64 bitset)", f.name)));
	}
}

fn check_interface(i: &Interface, package_version: u32, names: &HashMap<String, Kind>, errs: &mut Vec<Error>) {
	let mut seen_names = HashSet::new();
	let mut used_ops: HashMap<u32, Span> = HashMap::new();
	for m in &i.methods {
		check_evolution(m.evolution, package_version, m.span, errs);
		if !seen_names.insert(m.name.as_str()) {
			errs.push(Error::new(m.span, format!("duplicate method `{}` in interface `{}`", m.name, i.name)));
		}
		if m.op == 0 || m.op > abi::TYPED_OP_MAX as u32 {
			errs.push(Error::new(m.span, format!("opcode {} for `{}` must be in 1..={}", m.op, m.name, abi::TYPED_OP_MAX)));
		}
		if let Some(first) = used_ops.insert(m.op, m.span) {
			errs.push(Error::new(m.span, format!("interface `{}` reuses opcode {}; first declared at {first}", i.name, m.op)));
		}
		let mut seen_params = HashSet::new();
		for p in &m.params {
			check_evolution(p.evolution, package_version, p.span, errs);
			if !seen_params.insert(p.name.as_str()) {
				errs.push(Error::new(p.span, format!("duplicate parameter `{}` in `{}`", p.name, m.name)));
			}
			for r in &p.rights {
				if !RIGHTS.contains(&r.as_str()) {
					let suggestion = crate::resolve::suggest(r, RIGHTS.iter().copied());
					errs.push(Error::new(p.span, format!("unknown right `{r}` in `@rights`{}", suggestion.map(|value| format!("; did you mean `{value}`?")).unwrap_or_default())));
				}
			}
			check_type(&p.ty, p.span, names, errs);
		}
		check_type(&m.ret, m.span, names, errs);
	}
	for r in &i.reserved {
		if used_ops.contains_key(r) {
			errs.push(Error::new(i.span, format!("interface `{}` reserves opcode {r} that is also in use", i.name)));
		}
	}
}

fn check_evolution(evolution: Evolution, package_version: u32, span: Span, errs: &mut Vec<Error>) {
	for (name, version) in [("since", evolution.since), ("deprecated", evolution.deprecated)] {
		if let Some(version) = version {
			if version == 0 || version > package_version {
				errs.push(Error::new(span, format!("`@{name}({version})` must be in 1..={package_version}")));
			}
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
			None => {
				let suggestion = crate::resolve::suggest(res, names.iter().filter_map(|(name, kind)| if *kind == Kind::Resource { Some(name.as_str()) } else { None }));
				errs.push(Error::new(span, format!("unknown resource `{res}` in `handle<{res}>`{}", suggestion.map(|value| format!("; did you mean `{value}`?")).unwrap_or_default())));
			}
		},
		Type::Named(name) => match names.get(name) {
			Some(Kind::Value) | Some(Kind::External) => {}
			Some(Kind::Resource) => errs.push(Error::new(span, format!("`{name}` is a resource; pass it as `handle<{name}>`"))),
			Some(Kind::Interface) => errs.push(Error::new(span, format!("`{name}` is an interface, not a value type"))),
			None => {
				let suggestion = crate::resolve::suggest(name, names.keys().map(String::as_str));
				errs.push(Error::new(span, format!("unknown type `{name}`{}", suggestion.map(|value| format!("; did you mean `{value}`?")).unwrap_or_default())));
			}
		},
		_ => {}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cardinality {
	// A KNOWN COUNT. `Many` said "more than one" and could not tell four from five, so a five-handle
	// record validated and then lost its fifth capability at the writer, which refuses past
	// `MAX_HANDLES` - a schema expressing what the wire drops, which is the defect this whole
	// milestone is named for. A number can be compared against the limit and can be PRINTED, so the
	// diagnostic says five and says four.
	Exact(usize),
	// More than the wire carries, with the count, so the refusal can name it.
	OverLimit(usize),
	// A `list<handle<T>>`: the count is a property of the VALUE, not of the schema, so no
	// generation-time check can bound it. REFUSED, and it took a fourth audit to see why the
	// acceptance was wrong.
	//
	// The argument for accepting it was that the writer answers at run time: `set_handle` returns
	// `Option`, every generated `write` propagates it, so the whole message fails to encode rather
	// than losing its fifth capability. The message does fail. The CAPABILITY is still lost, because
	// the fifth handle is never given to the writer - `set_handle` refuses at the boundary - so
	// `writer.handles()` holds four and the generated failure cleanup, which can only hand back what
	// the writer holds, cannot reach the fifth. It sits in the service's own value, which for a
	// `list<handle<T>>` is a `Vec<u64>`: integers with no `Drop`. Nothing closes it and nothing can.
	//
	// "The whole message fails rather than losing a capability" was a statement about the BYTES,
	// made in a system where losing a capability means a live handle with no holder.
	//
	// Refusing it is the cheap half of a choice. The other half is an encoder that gives back every
	// capability the VALUE holds rather than every one the WRITER accepted, which is a change to the
	// writer trait and to every generated encoder - worth doing the day a schema needs this shape,
	// and not before, since none does.
	Unbounded,
	// An imported shape nothing has resolved. Fails closed, as it always did.
	Unknown,
}

// The wire's limit, from the wire rather than as a copy of its value - the same constant
// `SliceWriter::set_handle` refuses past and the kernel's `MAX_MESSAGE_CAPS`.
const MAX_HANDLES: usize = abi::MAX_MESSAGE_CAPS;

impl Cardinality {
	// Two shapes SIDE BY SIDE in one message - a record's fields, a tuple's items.
	fn sum(self, other: Cardinality) -> Cardinality {
		use Cardinality::*;
		match (self, other) {
			(Unknown, _) | (_, Unknown) => Unknown,
			(Unbounded, _) | (_, Unbounded) => Unbounded,
			(OverLimit(a), OverLimit(b)) => OverLimit(a + b),
			(OverLimit(a), Exact(b)) | (Exact(b), OverLimit(a)) => OverLimit(a + b),
			(Exact(a), Exact(b)) => {
				let total = a + b;
				if total > MAX_HANDLES { OverLimit(total) } else { Exact(total) }
			}
		}
	}

	// Two shapes that are ALTERNATIVES - a result's arms. The message carries whichever arrives, so
	// the worst case is what has to fit.
	fn max(self, other: Cardinality) -> Cardinality {
		use Cardinality::*;
		match (self, other) {
			(Unknown, _) | (_, Unknown) => Unknown,
			(Unbounded, _) | (_, Unbounded) => Unbounded,
			(OverLimit(a), OverLimit(b)) => OverLimit(a.max(b)),
			(OverLimit(a), Exact(_)) | (Exact(_), OverLimit(a)) => OverLimit(a),
			(Exact(a), Exact(b)) => Exact(a.max(b)),
		}
	}
}

fn check_wire_shapes(file: &File, names: &HashMap<String, Kind>, imports: Option<&HashMap<String, ResolvedSymbol>>, errs: &mut Vec<Error>) {
	let mut cards: HashMap<String, Cardinality> = HashMap::new();
	for item in &file.items {
		match item {
			Item::Alias(a) => {
				cards.insert(a.name.clone(), Cardinality::Exact(0));
			}
			Item::Record(r) => {
				cards.insert(r.name.clone(), Cardinality::Exact(0));
			}
			Item::Enum(e) => {
				cards.insert(e.name.clone(), Cardinality::Exact(0));
			}
			Item::Variant(v) => {
				cards.insert(v.name.clone(), Cardinality::Exact(0));
			}
			Item::Flags(f) => {
				cards.insert(f.name.clone(), Cardinality::Exact(0));
			}
			Item::Resource(_) | Item::Interface(_) => {}
		}
	}

	check_value_cycles(file, &cards, errs);

	// Recursive list-shaped values need a fixed point: a list contributes zero
	// until its element is known to carry a handle, then permanently becomes many.
	loop {
		let mut changed = false;
		for item in &file.items {
			let (name, next) = match item {
				Item::Alias(a) => (&a.name, type_cardinality(&a.ty, &cards, names, imports)),
				Item::Record(r) => {
					let next = r.fields.iter().fold(Cardinality::Exact(0), |acc, field| acc.sum(type_cardinality(&field.ty, &cards, names, imports)));
					(&r.name, next)
				}
				Item::Variant(v) => {
					let next = v.cases.iter().filter_map(|case| case.payload.as_ref()).fold(Cardinality::Exact(0), |acc, ty| acc.max(type_cardinality(ty, &cards, names, imports)));
					(&v.name, next)
				}
				Item::Enum(e) => (&e.name, Cardinality::Exact(0)),
				Item::Flags(f) => (&f.name, Cardinality::Exact(0)),
				Item::Resource(_) | Item::Interface(_) => continue,
			};
			if cards.get(name) != Some(&next) {
				cards.insert(name.clone(), next);
				changed = true;
			}
		}
		if !changed {
			break;
		}
	}

	for item in &file.items {
		match item {
			Item::Alias(a) => report_cardinality(cards[&a.name], a.span, &format!("alias `{}`", a.name), errs),
			Item::Record(r) => report_cardinality(cards[&r.name], r.span, &format!("record `{}`", r.name), errs),
			Item::Variant(v) => report_cardinality(cards[&v.name], v.span, &format!("variant `{}`", v.name), errs),
			Item::Interface(i) => {
				for method in &i.methods {
					let request = method.params.iter().fold(Cardinality::Exact(0), |acc, param| acc.sum(type_cardinality(&param.ty, &cards, names, imports)));
					report_cardinality(request, method.span, &format!("request for `{}.{}`", i.name, method.name), errs);
					let reply = type_cardinality(&method.ret, &cards, names, imports);
					report_cardinality(reply, method.span, &format!("reply for `{}.{}`", i.name, method.name), errs);
					check_stream_frames(&method.ret, method.span, &cards, names, imports, errs);
				}
			}
			Item::Enum(_) | Item::Flags(_) | Item::Resource(_) => {}
		}
	}
}

fn type_cardinality(ty: &Type, cards: &HashMap<String, Cardinality>, names: &HashMap<String, Kind>, imports: Option<&HashMap<String, ResolvedSymbol>>) -> Cardinality {
	match ty {
		Type::Handle(_) | Type::Buffer | Type::Stream(_) => Cardinality::Exact(1),
		Type::Option(inner) => type_cardinality(inner, cards, names, imports),
		// A LIST OF CAPABILITIES HAS NO GENERATION-TIME COUNT. `list<handle<T>>` collapsed to `Many`
		// and was accepted, so a schema could ask for as many capabilities as the value happened to
		// hold and the writer would refuse past four - at run time, on the fifth element, with the
		// first four already written and the fifth reachable by nobody. A list of things that carry
		// NONE is still zero, which is what keeps an ordinary `list<record>` out of the refusal.
		Type::List(inner) => match type_cardinality(inner, cards, names, imports) {
			Cardinality::Exact(0) => Cardinality::Exact(0),
			Cardinality::Unknown => Cardinality::Unknown,
			_ => Cardinality::Unbounded,
		},
		Type::Tuple(items) => items.iter().fold(Cardinality::Exact(0), |acc, item| acc.sum(type_cardinality(item, cards, names, imports))),
		Type::Result(ok, err) => type_cardinality(ok, cards, names, imports).max(type_cardinality(err, cards, names, imports)),
		Type::Named(name) => match names.get(name) {
			Some(Kind::External) => Cardinality::Unknown,
			Some(Kind::Value) => cards
				.get(name)
				.copied()
				.or_else(|| {
					// THE NUMBER, not a rounding of it. `HandleCardinality` was `Zero | One | Many`
					// and `Many` was read here as `Exact(MAX_HANDLES)` under a comment calling that
					// "the conservative reading" - so an imported two-capability record was counted
					// as four, and a local record combining it with another two-capability field was
					// refused at six when the truth is four. Safe, and wrong, and invisible until
					// somebody wrote that schema.
					imports.and_then(|resolved| resolved.get(name)).map(|symbol| match symbol.cardinality {
						HandleCardinality::Exact(n) => Cardinality::Exact(n),
						// The one shape no number describes, refused for the same reason a local
						// `list<handle<T>>` is.
						HandleCardinality::Unbounded => Cardinality::Unbounded,
					})
				})
				.unwrap_or(Cardinality::Unknown),
			_ => Cardinality::Unknown,
		},
		_ => Cardinality::Exact(0),
	}
}

// `Many` IS NOW ACCEPTED, and this is the last step of the 1 -> 4 handle migration rather than the
// first.
//
// `wire` said four, the kernel said four and had `SYS_CHANNEL_SEND_CAPS`/`RECV_CAPS`, and the
// generated client said four - while the typed server receive said ONE and this refusal was the
// only reason that was not already losing capabilities in production. The order was safe by
// accident: the schema could not express what the server would have dropped.
//
// It is relaxed in the SAME change as `rt::recv_caps_blocking` and the thirteen dispatch sites, so
// the two cannot be separated by a rebase. `check-single-cap-receive.sh` is what stops a new
// single-handle receive from appearing behind it.
//
// WHAT IS REFUSED IS WHAT THE WIRE CANNOT CARRY, and it is now a number rather than a word.
//
// `Many` could not tell four from five. So a five-capability record validated, and the writer -
// which refuses past `MAX_HANDLES` - dropped the fifth: a schema expressing what the wire discards,
// which is the defect this milestone is named for, at the other end of the same road. `Exact(n)`
// compares against the limit and PRINTS, so the diagnostic says five and says four.
//
// `Unbounded` is a `list<handle<T>>`, and it is REFUSED.
//
// This comment argued the opposite until 2026-08-15, and had gone on arguing it after the code
// stopped agreeing: the reasoning below it - that the wire bounds the count where the count exists -
// is true about the WRITER and false about what happens to the capabilities the writer never took.
// `SliceWriter::set_handle` refuses past `MAX_HANDLES` and the whole encode fails, which sounds
// safe; but cleanup walks `writer.handles()`, and the fifth capability was never recorded there. The
// failure that was supposed to protect it is what strands it.
//
// Left standing, a stale comment arguing for the behaviour the code no longer has is worse than no
// comment: the next reader has to work out which of the two is current, and the comment is the more
// persuasive of the pair.
//
// `Unknown` - an imported shape nothing has resolved - fails closed for the same reason it always
// did.
fn report_cardinality(card: Cardinality, span: Span, what: &str, errs: &mut Vec<Error>) {
	match card {
		Cardinality::Unknown => errs.push(Error::new(span, format!("{what} uses an imported wire shape that has not been resolved"))),
		Cardinality::OverLimit(n) => errs.push(Error::new(span, format!("{what} carries {n} capabilities and a message carries at most {MAX_HANDLES}"))),
		// A COLLECTION OF CAPABILITIES, refused for the reason written above `Cardinality::Unbounded`:
		// an encode that stops at the limit cannot give back the capabilities it never accepted, so
		// the failure that was supposed to protect the fifth one is what strands it.
		Cardinality::Unbounded => errs.push(Error::new(span, format!("{what} carries a collection of capabilities, whose count no schema can bound - and an encode that stops at {MAX_HANDLES} cannot hand back the ones it never took"))),
		Cardinality::Exact(_) => {}
	}
}

// A STREAM FRAME CARRIES WHAT A REPLY CARRIES, and it does now.
//
// This refused `Many` between 2026-08-13 and the migration on the same day, and the refusal is worth
// remembering rather than deleting: `report_cardinality` was relaxed to accept several capabilities
// for REPLIES, this call site went with it, and the generated frame transport did not - it ended with
// `*frame_handle = writer.handle()`, the first handle, and read with `Reader::with_handle`. So a
// two-capability frame would have had its second capability created, written into the writer, dropped
// by the encoder, and decoded by the client as a placeholder.
//
// The transport takes a bounded `Handles` now, so the schema and the wire agree again and the refusal
// is gone. What stays is `Unknown` - an imported shape nothing has resolved - which fails closed.
fn check_stream_frames(ty: &Type, span: Span, cards: &HashMap<String, Cardinality>, names: &HashMap<String, Kind>, imports: Option<&HashMap<String, ResolvedSymbol>>, errs: &mut Vec<Error>) {
	match ty {
		Type::Stream(item) => report_cardinality(type_cardinality(item, cards, names, imports), span, "stream frame", errs),
		Type::Option(inner) | Type::List(inner) => check_stream_frames(inner, span, cards, names, imports, errs),
		Type::Tuple(items) => {
			for item in items {
				check_stream_frames(item, span, cards, names, imports, errs);
			}
		}
		Type::Result(ok, err) => {
			check_stream_frames(ok, span, cards, names, imports, errs);
			check_stream_frames(err, span, cards, names, imports, errs);
			// AND THE ERROR ARM OF A STREAM OPEN CARRIES NO CAPABILITY.
			//
			// `result<stream<T>, E>` is the guarded stream open, and its cardinality is
			// `max(ok, err)` - so a capability-bearing `E` is one handle against the Ok arm's one
			// subchannel and the schema is legal. The generated transport cannot carry it: the
			// server's `_reply_err` takes no `reply_handles` out-parameter and refuses outright
			// when the encoder recorded one, and the client's Err arm refuses a reply that arrives
			// with any handle at all.
			//
			// So the schema permits a shape the protocol cannot express, which is the class this
			// milestone exists to close - and it is worse than a plain mismatch: the server has
			// already MINTED the capability by the time `_reply_err` refuses, and there is no
			// out-parameter to hand it back through, so it is stranded exactly as an over-long
			// capability list strands its fifth handle.
			//
			// REFUSED STATICALLY RATHER THAN SUPPORTED, deliberately. Supporting it means an
			// out-parameter on `_reply_err` and a handle-aware decode on the client's Err arm -
			// a wire and API change for a shape nothing in this tree asks for. When something does,
			// this refusal is where the decision is recorded and the place to change it.
			if matches!(ok.as_ref(), Type::Stream(_)) {
				match type_cardinality(err, cards, names, imports) {
					Cardinality::Exact(0) => {}
					Cardinality::Unknown => errs.push(Error::new(span, String::from("the error arm of a stream open uses an imported wire shape that has not been resolved"))),
					_ => errs.push(Error::new(span, String::from("the error arm of a stream open carries a capability, which the generated reply cannot transport - the server mints it and has no way to hand it back"))),
				}
			}
		}
		_ => {}
	}
}

fn check_value_cycles(file: &File, cards: &HashMap<String, Cardinality>, errs: &mut Vec<Error>) {
	let mut graph: HashMap<String, Vec<String>> = HashMap::new();
	let mut spans: HashMap<String, Span> = HashMap::new();
	for item in &file.items {
		let (name, span, tys): (&String, Span, Vec<&Type>) = match item {
			Item::Alias(a) => (&a.name, a.span, vec![&a.ty]),
			Item::Record(r) => (&r.name, r.span, r.fields.iter().map(|field| &field.ty).collect()),
			Item::Variant(v) => (&v.name, v.span, v.cases.iter().filter_map(|case| case.payload.as_ref()).collect()),
			_ => continue,
		};
		let mut refs = Vec::new();
		for ty in tys {
			collect_direct_refs(ty, cards, &mut refs);
		}
		graph.insert(name.clone(), refs);
		spans.insert(name.clone(), span);
	}
	check_alias_cycles(file, errs);

	let mut state: HashMap<String, u8> = HashMap::new();
	let mut reported = HashSet::new();
	for name in graph.keys() {
		visit_value(name, &graph, &spans, &mut state, &mut reported, errs);
	}
}

fn check_alias_cycles(file: &File, errs: &mut Vec<Error>) {
	let aliases: HashMap<&str, &Alias> = file.items.iter().filter_map(|item| if let Item::Alias(alias) = item { Some((alias.name.as_str(), alias)) } else { None }).collect();
	fn collect(ty: &Type, aliases: &HashMap<&str, &Alias>, out: &mut Vec<String>) {
		match ty {
			Type::Named(name) if aliases.contains_key(name.as_str()) => out.push(name.clone()),
			Type::Option(inner) | Type::List(inner) | Type::Stream(inner) => collect(inner, aliases, out),
			Type::Tuple(items) => items.iter().for_each(|item| collect(item, aliases, out)),
			Type::Result(ok, err) => {
				collect(ok, aliases, out);
				collect(err, aliases, out);
			}
			_ => {}
		}
	}
	let mut graph: HashMap<String, Vec<String>> = HashMap::new();
	let mut spans = HashMap::new();
	for alias in aliases.values() {
		let mut refs = Vec::new();
		collect(&alias.ty, &aliases, &mut refs);
		graph.insert(alias.name.clone(), refs);
		spans.insert(alias.name.clone(), alias.span);
	}
	let mut state = HashMap::new();
	let mut reported = HashSet::new();
	for name in graph.keys() {
		visit_value(name, &graph, &spans, &mut state, &mut reported, errs);
	}
}

fn collect_direct_refs(ty: &Type, cards: &HashMap<String, Cardinality>, out: &mut Vec<String>) {
	match ty {
		Type::Named(name) if cards.contains_key(name) => out.push(name.clone()),
		Type::Option(inner) => collect_direct_refs(inner, cards, out),
		Type::Tuple(items) => {
			for item in items {
				collect_direct_refs(item, cards, out);
			}
		}
		Type::Result(ok, err) => {
			collect_direct_refs(ok, cards, out);
			collect_direct_refs(err, cards, out);
		}
		// Vec and stream sub-channels provide indirection, so their element cannot
		// make the containing Rust value infinitely sized.
		Type::List(_) | Type::Stream(_) => {}
		_ => {}
	}
}

fn visit_value(name: &str, graph: &HashMap<String, Vec<String>>, spans: &HashMap<String, Span>, state: &mut HashMap<String, u8>, reported: &mut HashSet<String>, errs: &mut Vec<Error>) {
	match state.get(name).copied().unwrap_or(0) {
		2 => return,
		1 => {
			if reported.insert(name.to_string()) {
				errs.push(Error::new(spans[name], format!("non-indirected recursive value cycle involving `{name}`")));
			}
			return;
		}
		_ => {}
	}
	state.insert(name.to_string(), 1);
	if let Some(next) = graph.get(name) {
		for child in next {
			visit_value(child, graph, spans, state, reported, errs);
		}
	}
	state.insert(name.to_string(), 2);
}
