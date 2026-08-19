//! Rust code generation: a validated `ast::File` becomes the source of a `proto`
//! module - the wire types and their binary codec.
//!
//! The emitted code depends only on `crate::codec` (the hand-written codec
//! primitives) and `alloc`, so it links into the kernel and userspace alike.
//! Encoding writes into a caller buffer (heap-free); decoding allocates owned
//! `String`/`Vec`.

use crate::ast::*;
use crate::resolve::{self, ResolvedSymbol, SymbolKind};
use crate::token::Error;
use std::collections::HashMap;
use std::fmt::Write as _;

// Generate the Rust source of the `proto` module for one file.
pub fn rust(file: &File, source: &str, imports: &HashMap<String, ResolvedSymbol>) -> Result<String, Error> {
	// The enums that can express the dispatch overflow fallback: any error enum
	// with an `again` case (the reply-too-big degradation needs a variant to name).
	let mut again_enums: std::collections::HashSet<String> = std::collections::HashSet::new();
	let mut denied_enums: std::collections::HashSet<String> = std::collections::HashSet::new();
	for item in &file.items {
		if let Item::Enum(e) = item {
			if e.cases.iter().any(|c| c.name == "again") {
				again_enums.insert(e.name.clone());
			}
			if e.cases.iter().any(|c| c.name == "denied") {
				denied_enums.insert(e.name.clone());
			}
		}
	}
	for (local, symbol) in imports {
		if symbol.contains_again {
			again_enums.insert(local.clone());
		}
		if symbol.contains_denied {
			denied_enums.insert(local.clone());
		}
	}
	let mut aliases: HashMap<String, Type> = file.items.iter().filter_map(|item| if let Item::Alias(alias) = item { Some((alias.name.clone(), alias.ty.clone())) } else { None }).collect();
	for (local, symbol) in imports {
		if let Some(wire_type) = &symbol.wire_type {
			aliases.insert(local.clone(), wire_type.clone());
		}
	}
	let mut cg = Cg { out: String::new(), tmp: 0, package: file.package.path.join("_"), package_id: file.package.path.join(":"), package_version: file.package.version, again_enums, denied_enums, aliases };
	cg.file(file, source);
	cg.imports(imports);
	for item in &file.items {
		cg.item(item)?;
	}
	for item in &file.items {
		cg.render_item(item)?;
	}
	cg.line("#[cfg(test)]");
	cg.line("mod compat;");
	cg.line("");
	Ok(cg.out)
}

pub fn compat_rust(file: &File, imports: &HashMap<String, ResolvedSymbol>) -> String {
	let mut aliases: HashMap<String, Type> = file.items.iter().filter_map(|item| if let Item::Alias(alias) = item { Some((alias.name.clone(), alias.ty.clone())) } else { None }).collect();
	for (local, symbol) in imports {
		if let Some(wire_type) = &symbol.wire_type {
			aliases.insert(local.clone(), wire_type.clone());
		}
	}
	let mut cg = Cg { out: String::new(), tmp: 0, package: file.package.path.join("_"), package_id: file.package.path.join(":"), package_version: file.package.version, again_enums: std::collections::HashSet::new(), denied_enums: std::collections::HashSet::new(), aliases };
	cg.compat_tests(file);
	cg.out
}

struct Cg {
	out: String,
	tmp: u32,
	package: String,
	// The package identity as it goes on the wire for PROTOCOL_INFO: colon-joined path and the
	// declared version. Kept beside `package` (underscore-joined, used for symbol names)
	// because the two spellings are not interchangeable.
	package_id: String,
	package_version: u32,
	// Error-enum names that carry an `again` case, so a dispatch whose reply
	// overflows the caller's buffer can degrade to a typed error.
	again_enums: std::collections::HashSet<String>,
	denied_enums: std::collections::HashSet<String>,
	aliases: HashMap<String, Type>,
}

// Emit doc comments (/// doc) from the AST into the generated output.
fn emit_doc(out: &mut String, docs: &[Doc]) {
	for doc in docs {
		let _source_span = doc.span;
		let line = &doc.text;
		out.push_str("///");
		if !line.is_empty() && !line.starts_with(' ') {
			out.push(' ');
		}
		out.push_str(line);
		out.push('\n');
	}
}

fn evolution_text(evolution: Evolution) -> String {
	let mut parts = Vec::new();
	if let Some(version) = evolution.since {
		parts.push(format!("Since package version {version}."));
	}
	if let Some(version) = evolution.deprecated {
		parts.push(format!("Deprecated since package version {version}."));
	}
	parts.join(" ")
}

fn emit_evolution(out: &mut String, evolution: Evolution) {
	let text = evolution_text(evolution);
	if !text.is_empty() {
		out.push_str("/// ");
		out.push_str(&text);
		out.push('\n');
	}
}

fn markdown_doc(docs: &[Doc]) -> String {
	docs.iter().map(|doc| doc.text.trim().replace('|', "\\|")).collect::<Vec<_>>().join(" ")
}

fn markdown_description(docs: &[Doc], evolution: Evolution) -> String {
	let mut text = markdown_doc(docs);
	let metadata = evolution_text(evolution);
	if !metadata.is_empty() {
		if !text.is_empty() {
			text.push(' ');
		}
		text.push_str(&metadata);
	}
	text
}

fn write_markdown_doc(out: &mut String, docs: &[Doc]) {
	if !docs.is_empty() {
		let _ = writeln!(out, "{}\n", markdown_doc(docs));
	}
}

// The kernel's rights, by the name a schema spells, as a bit.
//
// Derived from `validate::RIGHTS` rather than listed again: that array is in the ABI's order, where
// `read` is `1 << 0` and each follows, so the index is the shift. A second copy here would be a
// second thing to keep in step, and the failure mode is silent - a mask that enforces the wrong
// right reads exactly like one that enforces the right one. Unknown names never reach this;
// `validate` rejects them with a suggestion.
fn right_bit(name: &str) -> Option<u32> {
	crate::validate::RIGHTS.iter().position(|known| *known == name).map(|index| 1u32 << index)
}

impl Cg {
	// A fresh binder name, unique within the file.
	fn fresh(&mut self) -> String {
		let n = self.tmp;
		self.tmp += 1;
		format!("v{n}")
	}

	fn line(&mut self, s: &str) {
		self.out.push_str(s);
		self.out.push('\n');
	}

	// Emit the encode / encode_vec / decode methods shared by every codec type.
	fn codec_methods(&mut self, ty: &str) {
		// THE SAME REFUSAL AS `encode_vec`, and it was missing here for as long as that one existed.
		//
		// `encode` returns a LENGTH, so a value that recorded a capability produced a byte count and
		// left the capability in a writer the caller then drops: `OpenResult { file: 123, .. }
		// .encode(&mut buf)` answered `Some(n)` and the handle was gone. `encode_vec` was corrected
		// for exactly this - "half a wire representation could be passed around by accident, and for
		// a capability-bearing type the half that was dropped was the live one" - and its twin was
		// left carrying the other half of the same defect.
		//
		// Refusing rather than not generating it: the signature stays, so a caller that reaches for
		// the wrong function is told at the call rather than at the type. `encode_message` is the one
		// that carries both halves.
		self.line("\tpub fn encode(&self, out: &mut [u8]) -> Option<usize> {");
		self.line("\t\tlet mut w = SliceWriter::new(out);");
		self.line("\t\tself.write(&mut w)?;");
		self.line("\t\t// `finish` refuses while a capability is recorded, because returning the");
		self.line("\t\t// length alone would drop it.");
		self.line("\t\tw.finish()");
		self.line("\t}");
		// `encode_vec` REFUSES a type that carries a capability, and `encode_message` is the shape
		// that carries both halves. The pair the generator advertised as a round trip was not one:
		// `encode_vec` built a `VecWriter`, called `write` - which may call `set_handle` - and then
		// returned `w.into_inner()`, which is `self.buf`, discarding the handles; `decode` mirrored
		// it with no capabilities at all. Half a wire representation could be passed around by
		// accident, and for a capability-bearing type the half that was dropped was the live one.
		//
		// THE REFUSAL MOVED INTO THE CODEC (WIRE-001). It used to be an `if` this generator emitted
		// at each `encode_vec`, which made the property a property of the GENERATOR: a hand-written
		// encoder calling `into_inner` got nothing. `VecWriter::into_inner` is fallible now and
		// refuses while it holds a handle, so this emits the call and inherits the check.
		self.line("\tpub fn encode_vec(&self) -> Option<Vec<u8>> {");
		self.line("\t\tlet mut w = VecWriter::new();");
		self.line("\t\tself.write(&mut w)?;");
		self.line("\t\t// `into_inner` refuses while a capability is recorded, because returning");
		self.line("\t\t// the bytes alone would drop it.");
		self.line("\t\tw.into_inner()");
		self.line("\t}");
		self.line("\tpub fn encode_message(&self) -> Option<(Vec<u8>, Handles)> {");
		self.line("\t\tlet mut w = VecWriter::new();");
		self.line("\t\tself.write(&mut w)?;");
		self.line("\t\tSome(w.into_message())");
		self.line("\t}");
		// `decode` requires the reader to have FINISHED: `01 02 03` and `01 02 03 DE AD BE EF` used
		// to decode to the same value, because nothing asked whether the bytes were all consumed.
		self.line(&format!("\tpub fn decode(bytes: &[u8]) -> Option<{ty}> {{"));
		self.line("\t\tlet mut r = Reader::new(bytes);");
		self.line(&format!("\t\tlet value = {ty}::read(&mut r)?;"));
		self.line("\t\tr.finish()?;");
		self.line("\t\tSome(value)");
		self.line("\t}");
		// `&mut Handles` AND A CLEAR, the same shape the framing boundaries already have.
		//
		// This took `&Handles` and cleared nothing, so after a successful decode the returned value
		// and the caller's list named the SAME kernel handles - and the rule "success means do not
		// close these, failure means close them" lived in whoever remembered it rather than in the
		// signature. `Handles` is non-owning and `Copy` on purpose, so nothing in the type system
		// was going to catch the caller that got it backwards.
		//
		// The dispatch, the stream frame and the reply readers all take `&mut Handles` and clear
		// once the frame is known good (`request_handles.clear()`, `frame_handles.clear()`), which
		// leaves a successful read holding nothing and a failed one holding everything. The generic
		// helper is the same question with the same answer; it is only a hazard rather than a leak
		// because no production caller uses it yet, which is exactly why it is cheap to change.
		self.line(&format!("\tpub fn decode_message(bytes: &[u8], handles: &mut Handles) -> Option<{ty}> {{"));
		self.line("\t\tlet mut r = Reader::with_handles(bytes, handles);");
		self.line(&format!("\t\tlet value = {ty}::read(&mut r)?;"));
		self.line("\t\tr.finish()?;");
		self.line("\t\t// The frame is good, so the capabilities it carried are the value's now. A");
		self.line("\t\t// refusal above leaves them in the caller's list, which is the half that closes.");
		self.line("\t\thandles.clear();");
		self.line("\t\tSome(value)");
		self.line("\t}");
	}

	fn file(&mut self, file: &File, source: &str) {
		let pkg = file.package.path.join(":");
		self.line(&format!("// @generated by lsidl-gen from {source} (package {pkg}@{}).", file.package.version));
		self.line("// Do not edit by hand; regenerate with `./gen.sh`.");
		for doc in &file.package_doc {
			self.line(&format!("//!{}{}", if doc.text.is_empty() || doc.text.starts_with(' ') { "" } else { " " }, doc.text));
		}
		self.line("#![allow(dead_code, unused_imports, unused_variables, unused_mut, clippy::all)]");
		self.line("");
		self.line("use crate::codec::{Handles, PROTOCOL_INFO_OP, Reader, Sink, SliceWriter, VecWriter};");
		self.line("use alloc::string::String;");
		self.line("use alloc::vec::Vec;");
		self.line("use core::fmt::Write as _;");
		self.line("");
	}

	fn imports(&mut self, imports: &HashMap<String, ResolvedSymbol>) {
		let mut imports: Vec<(&String, &ResolvedSymbol)> = imports.iter().filter(|(_, symbol)| symbol.kind == SymbolKind::Value).collect();
		imports.sort_by(|a, b| a.0.cmp(b.0));
		for (local, symbol) in &imports {
			self.line(&format!("use {} as {};", resolve::import_rust_path(symbol), camel(local)));
		}
		if !imports.is_empty() {
			self.line("");
		}
	}

	fn item(&mut self, item: &Item) -> Result<(), Error> {
		match item {
			Item::Alias(alias) => {
				emit_doc(&mut self.out, &alias.doc);
				emit_evolution(&mut self.out, alias.evolution);
				let ty = rust_ty(&alias.ty).map_err(|message| Error::new(alias.span, message))?;
				self.line(&format!("pub type {} = {ty};", camel(&alias.name)));
				self.line("");
				Ok(())
			}
			Item::Record(r) => self.record(r),
			Item::Enum(e) => {
				self.enum_decl(e);
				Ok(())
			}
			Item::Variant(v) => self.variant(v),
			Item::Flags(f) => {
				self.flags(f);
				Ok(())
			}
			Item::Resource(_) => Ok(()),
			Item::Interface(i) => self.interface(i),
		}
	}

	fn record(&mut self, r: &Record) -> Result<(), Error> {
		let ty = camel(&r.name);
		emit_doc(&mut self.out, &r.doc);
		emit_evolution(&mut self.out, r.evolution);
		self.line("#[derive(Clone, Debug, PartialEq)]");
		self.line(&format!("pub struct {ty} {{"));
		for f in &r.fields {
			let fty = rust_ty(&f.ty).map_err(|m| Error::new(f.span, m))?;
			emit_doc(&mut self.out, &f.doc);
			emit_evolution(&mut self.out, f.evolution);
			self.line(&format!("\tpub {}: {fty},", field_ident(&f.name)));
		}
		self.line("}");
		self.line("");
		self.line(&format!("impl {ty} {{"));
		self.codec_methods(&ty);
		self.line("\tpub fn write<W: Sink>(&self, w: &mut W) -> Option<()> {");
		for f in &r.fields {
			let place = format!("self.{}", field_ident(&f.name));
			let code = self.write_place(&f.ty, &place, false).map_err(|m| Error::new(f.span, m))?;
			self.line(&format!("\t\t{code}"));
		}
		self.line("\t\tSome(())");
		self.line("\t}");
		self.line(&format!("\tpub fn read(r: &mut Reader) -> Option<{ty}> {{"));
		for f in &r.fields {
			let expr = self.read_value(&f.ty).map_err(|m| Error::new(f.span, m))?;
			self.line(&format!("\t\tlet {} = {expr};", field_ident(&f.name)));
		}
		let inits: Vec<String> = r.fields.iter().map(|f| field_ident(&f.name)).collect();
		self.line(&format!("\t\tSome({ty} {{ {} }})", inits.join(", ")));
		self.line("\t}");
		self.line("}");
		self.line("");
		Ok(())
	}

	fn enum_decl(&mut self, e: &Enum) {
		let ty = camel(&e.name);
		let ords = effective_ordinals(e);
		emit_doc(&mut self.out, &e.doc);
		emit_evolution(&mut self.out, e.evolution);
		self.line("#[derive(Clone, Copy, Debug, PartialEq, Eq)]");
		self.line("#[repr(u8)]");
		self.line(&format!("pub enum {ty} {{"));
		for (c, ord) in e.cases.iter().zip(&ords) {
			emit_doc(&mut self.out, &c.doc);
			emit_evolution(&mut self.out, c.evolution);
			self.line(&format!("\t{} = {ord},", camel(&c.name)));
		}
		self.line("}");
		self.line("");
		self.line(&format!("impl {ty} {{"));
		self.codec_methods(&ty);
		self.line("\tpub fn write<W: Sink>(&self, w: &mut W) -> Option<()> {");
		self.line("\t\tw.u8(*self as u8)");
		self.line("\t}");
		self.line(&format!("\tpub fn read(r: &mut Reader) -> Option<{ty}> {{"));
		self.line("\t\tmatch r.u8()? {");
		for (c, ord) in e.cases.iter().zip(&ords) {
			self.line(&format!("\t\t\t{ord} => Some({ty}::{}),", camel(&c.name)));
		}
		self.line("\t\t\t_ => None,");
		self.line("\t\t}");
		self.line("\t}");
		self.line("}");
		self.line("");
	}

	fn variant(&mut self, v: &Variant) -> Result<(), Error> {
		let ty = camel(&v.name);
		emit_doc(&mut self.out, &v.doc);
		emit_evolution(&mut self.out, v.evolution);
		self.line("#[derive(Clone, Debug, PartialEq)]");
		self.line(&format!("pub enum {ty} {{"));
		for c in &v.cases {
			emit_doc(&mut self.out, &c.doc);
			emit_evolution(&mut self.out, c.evolution);
			match &c.payload {
				Some(p) => {
					let pty = rust_ty(p).map_err(|m| Error::new(c.span, m))?;
					self.line(&format!("\t{}({pty}),", camel(&c.name)));
				}
				None => self.line(&format!("\t{},", camel(&c.name))),
			}
		}
		self.line("}");
		self.line("");
		self.line(&format!("impl {ty} {{"));
		self.codec_methods(&ty);
		self.line("\tpub fn write<W: Sink>(&self, w: &mut W) -> Option<()> {");
		self.line("\t\tmatch self {");
		for (tag, c) in v.cases.iter().enumerate() {
			match &c.payload {
				Some(p) => {
					let b = self.fresh();
					let body = self.write_place(p, &b, true).map_err(|m| Error::new(c.span, m))?;
					self.line(&format!("\t\t\t{ty}::{}({b}) => {{ w.u8({tag})?; {body} }}", camel(&c.name)));
				}
				None => self.line(&format!("\t\t\t{ty}::{} => {{ w.u8({tag})?; }}", camel(&c.name))),
			}
		}
		self.line("\t\t}");
		self.line("\t\tSome(())");
		self.line("\t}");
		self.line(&format!("\tpub fn read(r: &mut Reader) -> Option<{ty}> {{"));
		self.line("\t\tmatch r.u8()? {");
		for (tag, c) in v.cases.iter().enumerate() {
			match &c.payload {
				Some(p) => {
					let expr = self.read_value(p).map_err(|m| Error::new(c.span, m))?;
					self.line(&format!("\t\t\t{tag} => Some({ty}::{}({expr})),", camel(&c.name)));
				}
				None => self.line(&format!("\t\t\t{tag} => Some({ty}::{}),", camel(&c.name))),
			}
		}
		self.line("\t\t\t_ => None,");
		self.line("\t\t}");
		self.line("\t}");
		self.line("}");
		self.line("");
		Ok(())
	}

	fn flags(&mut self, f: &Flags) {
		let ty = camel(&f.name);
		let width = flags_width(f.flags.len());
		emit_doc(&mut self.out, &f.doc);
		emit_evolution(&mut self.out, f.evolution);
		self.line("#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]");
		self.line(&format!("pub struct {ty}(pub {width});"));
		self.line("");
		self.line(&format!("impl {ty} {{"));
		for (i, flag) in f.flags.iter().enumerate() {
			emit_doc(&mut self.out, &flag.doc);
			emit_evolution(&mut self.out, flag.evolution);
			self.line(&format!("\tpub const {}: {width} = 1 << {i};", screaming(&flag.name)));
		}
		self.codec_methods(&ty);
		self.line("\tpub fn write<W: Sink>(&self, w: &mut W) -> Option<()> {");
		self.line(&format!("\t\tw.{width}(self.0)"));
		self.line("\t}");
		self.line(&format!("\tpub fn read(r: &mut Reader) -> Option<{ty}> {{"));
		self.line(&format!("\t\tSome({ty}(r.{width}()?))"));
		self.line("\t}");
		self.line("}");
		self.line("");
	}

	fn interface(&mut self, i: &Interface) -> Result<(), Error> {
		let modname = field_ident(&i.name);
		emit_doc(&mut self.out, &i.doc);
		emit_evolution(&mut self.out, i.evolution);
		self.line(&format!("// interface `{}` over a channel: opcodes, a Service trait + dispatch, and a Client.", i.name));
		self.line(&format!("pub mod {modname} {{"));
		self.line("\tuse super::*;");
		self.line("\tuse crate::codec::{Reader, SliceWriter, Sink, Transport, TransportError, VecWriter};");
		self.line("\tuse alloc::vec::Vec;");
		self.line("");
		for m in &i.methods {
			self.line(&format!("\tpub const OP_{}: u16 = {};", screaming(&m.name), m.op));
		}
		for m in &i.methods {
			if !method_supported(m) {
				self.line(&format!("\t// `{}` (op {}) uses handle/buffer/stream; bindings deferred.", m.name, m.op));
			}
		}
		let supported: Vec<&Method> = i.methods.iter().filter(|m| method_supported(m)).collect();
		self.line("");
		self.line("\tpub trait Service {");
		for m in &supported {
			emit_doc(&mut self.out, &m.doc);
			emit_evolution(&mut self.out, m.evolution);
			for param in &m.params {
				let description = markdown_description(&param.doc, param.evolution);
				if !description.is_empty() {
					self.line(&format!("\t\t/// Parameter `{}`: {description}", param.name));
				}
			}
			let params = trait_params(m)?;
			if let Some((elem, err)) = stream_return(&m.ret) {
				let et = rust_ty(elem).map_err(|e| Error::new(m.span, e))?;
				// A stream in a `result` gives the service a way to say the stream never starts.
				// Without it the only refusal available is an empty stream, which a caller cannot
				// tell from an empty directory.
				match err {
					Some(errty) => {
						let ety = rust_ty(errty).map_err(|e| Error::new(m.span, e))?;
						self.line(&format!("\t\tfn {}(&mut self{params}) -> Result<Vec<{et}>, {ety}>;", field_ident(&m.name)));
					}
					None => self.line(&format!("\t\tfn {}(&mut self{params}) -> Vec<{et}>;", field_ident(&m.name))),
				}
			} else {
				let ret = rust_ty(&m.ret).map_err(|e| Error::new(m.span, e))?;
				self.line(&format!("\t\tfn {}(&mut self{params}) -> {ret};", field_ident(&m.name)));
			}
		}
		self.line("\t}");
		self.line("");
		let has_non_stream: bool = supported.iter().any(|m| stream_return(&m.ret).is_none());
		if has_non_stream {
			self.line("\tpub fn dispatch<S: Service>(service: &mut S, request: &[u8], request_handles: &mut Handles, out: &mut [u8], reply_handles: &mut Handles) -> Option<usize> {");
			self.line("\t\tlet mut reader = Reader::with_handle_list(request, request_handles);");
			self.line("\t\tlet r = &mut reader;");
			self.line("\t\tlet op = r.u16()?;");
			self.line("\t\tlet corr = r.u32()?;");
			self.line("\t\tlet mut writer = SliceWriter::new(out);");
			// The stateless identity query. Answered here rather than in rt's serve loops
			// because rt serves every interface and cannot know which package this service
			// implements; the generated code is the only layer that does. Emitted before the
			// typed match so an interface can never shadow it - the validator also refuses a
			// typed `@op` this high, so the two guards agree.
			self.line("\t\tif op == PROTOCOL_INFO_OP {");
			// A MESSAGE BOUNDARY LIKE ANY OTHER. This answered after reading the opcode and the
			// correlation id without looking at the rest, so `[OP][corr]` and `[OP][corr][DE AD]`
			// were the same request - the exact thing the typed arms below refuse, in the one arm
			// emitted above them. The identity query takes no arguments, so `finish` here means
			// "and nothing else", which is the whole of its contract.
			self.line("\t\t\tr.finish()?;");
			self.line("\t\t\trequest_handles.clear();");
			self.line("\t\t\tlet w = &mut writer;");
			self.line("\t\t\tw.u32(corr)?;");
			self.line(&format!("\t\t\tw.bytes_lp(b\"{}\")?;", self.package_id));
			self.line(&format!("\t\t\tw.u32({})?;", self.package_version));
			self.line("\t\t\tmatch Handles::try_from_slice(writer.handles()) { Some(taken) => *reply_handles = taken, None => return None }");
			self.line("\t\t\treturn Some(writer.pos());");
			self.line("\t\t}");
			self.line("\t\tmatch op {");
			for m in &supported {
				if stream_return(&m.ret).is_some() {
					continue;
				}
				self.line(&format!("\t\t\tOP_{} => {{", screaming(&m.name)));
				let mut args: Vec<String> = Vec::new();
				for p in &m.params {
					let expr = self.read_value(&p.ty).map_err(|e| Error::new(p.span, e))?;
					let pn = field_ident(&p.name);
					self.line(&format!("\t\t\t\tlet {pn} = {expr};"));
					args.push(pn);
				}
				// `finish`, NOT `has_handle`. The handle half was checked and the BYTE half was
				// not, so `[OP][corr]` and `[OP][corr][DE AD BE EF]` were the same request: a
				// message its writer and its reader disagree about, accepted. `Reader::finish`
				// answers both halves and is the primitive `decode` has always used - it was
				// called at the codec boundary and at none of the framing boundaries.
				self.line("\t\t\t\tr.finish()?;");
				self.line("\t\t\t\trequest_handles.clear();");
				// `@rights` ENFORCED HERE, between a well-formed request and the service seeing it.
				//
				// The annotation used to reach the ABI signature and no generated code, so what a
				// service could assume about a handle it was sent was prose that held while every
				// caller remembered. A handle that does not already carry what the schema declares
				// is refused with the method's own error type; the service is not called at all.
				let mut guards: Vec<String> = Vec::new();
				for p in &m.params {
					if p.rights.is_empty() || !matches!(p.ty, Type::Handle(_)) {
						continue;
					}
					let mut mask: u32 = 0;
					for right in &p.rights {
						match right_bit(right) {
							Some(bit) => mask |= bit,
							None => return Err(Error::new(p.span, format!("@rights names '{right}', which is not a kernel right"))),
						}
					}
					guards.push(format!("crate::codec::handle_carries({}, {mask}, crate::codec::NO_REQUIRED_TYPE)", field_ident(&p.name)));
				}
				let denied: Option<String> = if guards.is_empty() {
					None
				} else {
					match &m.ret {
						Type::Result(_, errty) => match errty.as_ref() {
							Type::Named(n) if self.denied_enums.contains(n) => Some(camel(n)),
							_ => None,
						},
						_ => None,
					}
				};
				match &denied {
					Some(error_type) => {
						self.line(&format!("\t\t\t\tlet authorized = {};", guards.join(" && ")));
						self.line(&format!("\t\t\t\tlet result = if authorized {{ service.{}({}) }} else {{ Err({error_type}::Denied) }};", field_ident(&m.name), args.join(", ")));
					}
					None => {
						self.line(&format!("\t\t\t\tlet result = service.{}({});", field_ident(&m.name), args.join(", ")));
					}
				}
				let code = self.write_place(&m.ret, "result", false).map_err(|e| Error::new(m.span, e))?;
				// A result whose error enum carries an `again` case can degrade an
				// oversized reply into a typed error instead of sending nothing
				// (which would strand the client waiting forever).
				let fallback: Option<String> = match &m.ret {
					Type::Result(_, errty) => match errty.as_ref() {
						Type::Named(n) if self.again_enums.contains(n) => Some(camel(n)),
						_ => None,
					},
					_ => None,
				};
				if let Some(err_enum) = fallback {
					self.line("\t\t\t\tlet encoded: Option<()> = (|| {");
					self.line("\t\t\t\t\tlet w = &mut writer;");
					self.line("\t\t\t\t\tw.u32(corr)?;");
					self.line(&format!("\t\t\t\t\t{code}"));
					self.line("\t\t\t\t\tSome(())");
					self.line("\t\t\t\t})();");
					self.line("\t\t\t\tif encoded.is_none() {");
					self.line("\t\t\t\t\tif writer.has_handle() { match Handles::try_from_slice(writer.handles()) { Some(taken) => *reply_handles = taken, None => {} } return None; }");
					self.line("\t\t\t\t\t// the reply outgrew the caller's buffer: replace it with a typed");
					self.line("\t\t\t\t\t// error, so the client sees a failure instead of hanging.");
					self.line("\t\t\t\t\twriter.reset();");
					self.line("\t\t\t\t\tlet w = &mut writer;");
					self.line("\t\t\t\t\tw.u32(corr)?;");
					self.line("\t\t\t\t\tw.u8(0)?;");
					self.line(&format!("\t\t\t\t\t{err_enum}::Again.write(w)?;"));
					self.line("\t\t\t\t}");
				} else {
					self.line("\t\t\t\tlet encoded: Option<()> = (|| {");
					self.line("\t\t\t\t\tlet w = &mut writer;");
					self.line("\t\t\t\t\tw.u32(corr)?;");
					self.line(&format!("\t\t\t\t\t{code}"));
					self.line("\t\t\t\t\tSome(())");
					self.line("\t\t\t\t})();");
					self.line("\t\t\t\tif encoded.is_none() {");
					self.line("\t\t\t\t\tif writer.has_handle() { match Handles::try_from_slice(writer.handles()) { Some(taken) => *reply_handles = taken, None => return None } }");
					self.line("\t\t\t\t\treturn None;");
					self.line("\t\t\t\t}");
				}
				self.line("\t\t\t}");
			}
			self.line("\t\t\t_ => return None,");
			self.line("\t\t}");
			self.line("\t\tmatch Handles::try_from_slice(writer.handles()) { Some(taken) => *reply_handles = taken, None => return None }");
			self.line("\t\tSome(writer.pos())");
			self.line("\t}");
		} else {
			// Every op is a stream, served out of band by the `<m>_open` helpers, so there
			// is nothing to dispatch in band - a trivial body avoids an unreachable match.
			self.line("\tpub fn dispatch<S: Service>(_service: &mut S, _request: &[u8], _request_handles: &mut Handles, _out: &mut [u8], _reply_handles: &mut Handles) -> Option<usize> {");
			self.line("\t\tNone");
			self.line("\t}");
		}
		self.line("");
		// stream methods: the wait-drained event stream. The serve loop calls
		// `<m>_open` to decode the request and run the service, creates a sub-channel,
		// replies with the consumer end, then frames each element with `<m>_frame`
		// onto the producer; the client drains the consumer with ordinary receives,
		// decoding each element with `<m>_read`, until the producer closes.
		for m in &supported {
			if let Some((elem, err)) = stream_return(&m.ret) {
				let mname = field_ident(&m.name);
				let et = rust_ty(elem).map_err(|e| Error::new(m.span, e))?;
				let opened = match err {
					Some(errty) => format!("Result<Vec<{et}>, {}>", rust_ty(errty).map_err(|e| Error::new(m.span, e))?),
					None => format!("Vec<{et}>"),
				};
				self.line(&format!("\tpub fn {mname}_open<S: Service>(service: &mut S, request: &[u8], request_handles: &mut Handles) -> Option<(u32, {opened})> {{"));
				self.line("\t\tlet mut reader = Reader::with_handle_list(request, request_handles);");
				self.line("\t\tlet r = &mut reader;");
				self.line("\t\tlet _op = r.u16()?;");
				self.line("\t\tlet corr = r.u32()?;");
				let mut args: Vec<String> = Vec::new();
				for p in &m.params {
					let expr = self.read_value(&p.ty).map_err(|e| Error::new(p.span, e))?;
					let pn = field_ident(&p.name);
					self.line(&format!("\t\tlet {pn} = {expr};"));
					args.push(pn);
				}
				// The same framing boundary as the dispatch above, and the same primitive.
				self.line("\t\tr.finish()?;");
				self.line("\t\trequest_handles.clear();");
				self.line(&format!("\t\tlet items = service.{mname}({});", args.join(", ")));
				self.line("\t\tSome((corr, items))");
				self.line("\t}");
				if let Some(errty) = err {
					// THE TWO REPLY BODIES A RESULT-WRAPPED STREAM HAS, emitted here because the
					// serve loop owns the sub-channel and cannot let the writer carry the handle.
					//
					// Ok is `corr`, the tag, and the handle's four-byte placeholder; the consumer
					// end travels in the reply's handle list, which is where the loop puts it. Err
					// is `corr`, the tag and the error - and NO handle, so a client that decodes an
					// error is not also left holding a channel nobody will write to.
					self.line(&format!("\tpub fn {mname}_reply_ok(corr: u32, out: &mut [u8]) -> Option<usize> {{"));
					self.line("\t\tlet mut writer = SliceWriter::new(out);");
					self.line("\t\tlet w = &mut writer;");
					self.line("\t\tw.u32(corr)?;");
					self.line("\t\tw.u8(1)?;");
					self.line("\t\tw.u32(0)?;");
					self.line("\t\twriter.finish()");
					self.line("\t}");
					let ety = rust_ty(errty).map_err(|e| Error::new(m.span, e))?;
					self.line(&format!("\tpub fn {mname}_reply_err(corr: u32, error: &{ety}, out: &mut [u8]) -> Option<usize> {{"));
					self.line("\t\tlet mut writer = SliceWriter::new(out);");
					self.line("\t\tlet w = &mut writer;");
					self.line("\t\tw.u32(corr)?;");
					self.line("\t\tw.u8(0)?;");
					let code = self.write_place(errty, "error", true).map_err(|e| Error::new(m.span, e))?;
					self.line(&format!("\t\t{code}"));
					// An error that will not encode must not become a silent Ok: the caller checks
					// for `None` and answers with nothing rather than with the wrong thing. `finish`
					// is also the refusal if the error somehow recorded a capability.
					self.line("\t\twriter.finish()");
					self.line("\t}");
				}
				// A FRAME CARRIES A BOUNDED LIST, like every other message on this wire.
				//
				// It carried exactly one: `*frame_handle = writer.handle()` took the FIRST and
				// discarded the rest, and the reader was `Reader::with_handle`. When the validator
				// was relaxed to let a record carry several capabilities, a stream element could
				// declare two - and the second would be created, written into the writer, dropped
				// by the encoder, and decoded by the client as a placeholder. The refusal in
				// `check_stream_frames` held that gap shut; this is what lets it be lifted.
				//
				// The out-parameter stays an out-parameter for the reason the reply's does: the
				// caller owns the capabilities either way, and on a failed encode it has to be able
				// to close what was already recorded.
				self.line(&format!("\tpub fn {mname}_frame(seq: u32, item: &{et}, out: &mut [u8], frame_handles: &mut Handles) -> Option<usize> {{"));
				self.line("\t\tlet mut writer = SliceWriter::new(out);");
				self.line("\t\tlet encoded: Option<()> = (|| {");
				self.line("\t\t\tlet w = &mut writer;");
				self.line("\t\t\tw.u32(seq)?;");
				let code = self.write_place(elem, "item", true).map_err(|e| Error::new(m.span, e))?;
				self.line(&format!("\t\t\t{code}"));
				self.line("\t\t\tSome(())");
				self.line("\t\t})();");
				self.line("\t\tif encoded.is_none() {");
				// Whatever the half-written frame recorded goes back to the caller, which is the
				// only party that can close it. `try_from_slice` cannot fail here - the writer is
				// bounded by the same `MAX_HANDLES` - and a failure is answered by leaving the
				// caller's list untouched rather than by inventing one.
				self.line("\t\t\tif let Some(taken) = Handles::try_from_slice(writer.handles()) { *frame_handles = taken; }");
				self.line("\t\t\treturn None;");
				self.line("\t\t}");
				self.line("\t\t*frame_handles = Handles::try_from_slice(writer.handles())?;");
				self.line("\t\tSome(writer.pos())");
				self.line("\t}");
				// `&mut`, SO THE CALLER'S LIST SAYS WHAT IT STILL OWNS.
				//
				// This took `&Handles`, copied them into the reader and left the caller's list
				// untouched - so after a successful read the same capability was named twice, once by
				// the decoded value that adopted it and once by the list the caller still held, and
				// which of the two owned it was a convention nobody could typecheck. The tree held
				// both readings of that convention: `tail` closed the list BEFORE decoding and `ls`
				// closed it AFTER, and both were correct only because their element types carry no
				// capability. The first one to grow a `handle<R>` would have broken one of them.
				//
				// `finish` already requires that every transferred handle was adopted, so on success
				// the list is provably spent and clearing it is exact rather than approximate. On
				// failure it is left whole: the partially built value is made of integers and drops
				// nothing, so everything is still the caller's to close. Either way the caller closes
				// `frame_handles` unconditionally and needs to know nothing about which case it is in.
				self.line(&format!("\tpub fn {mname}_read(msg: &[u8], frame_handles: &mut Handles) -> Option<{et}> {{"));
				self.line("\t\tlet mut reader = Reader::with_handles(msg, frame_handles);");
				self.line("\t\tlet r = &mut reader;");
				self.line("\t\tlet _seq = r.u32()?;");
				let expr = self.read_value(elem).map_err(|e| Error::new(m.span, e))?;
				self.line(&format!("\t\tlet value = {expr};"));
				// A frame is a message too: trailing bytes after the item are a frame its writer
				// and its reader disagree about.
				self.line("\t\treader.finish()?;");
				self.line("\t\tframe_handles.clear();");
				self.line("\t\tSome(value)");
				self.line("\t}");
				self.line("");
			}
		}
		self.line("\tpub struct Client<T: Transport> {");
		self.line("\t\ttransport: T,");
		self.line("\t\tcorr: u32,");
		// The deadline every call on this client carries, and the reason the last one failed.
		//
		// The methods keep returning `Option`, so the hundreds of existing call sites are unchanged;
		// what they could not do before is ASK WHY. A caller that needs to decide whether retrying is
		// safe reads `last_error`: `SendRefused` never left, `PeerClosed` will never succeed on this
		// channel, `TimedOut` may already have been acted on. Collapsing those into `None` is what
		// the audit found, and this is the smallest shape that undoes it without changing every
		// generated signature in fifteen protocol crates.
		self.line("\t\tdeadline: u64,");
		self.line("\t\tlast_error: Option<TransportError>,");
		self.line("\t}");
		self.line("");
		self.line("\timpl<T: Transport> Client<T> {");
		self.line("\t\tpub fn new(transport: T) -> Client<T> {");
		self.line("\t\t\tClient { transport, corr: 0, deadline: 0, last_error: None }");
		self.line("\t\t}");
		// An ABSOLUTE deadline, in the same ticks `rt::clock` reports. Zero - the default - keeps
		// the blocking behaviour every existing caller already has.
		self.line("\t\tpub fn with_deadline(transport: T, deadline: u64) -> Client<T> {");
		self.line("\t\t\tClient { transport, corr: 0, deadline, last_error: None }");
		self.line("\t\t}");
		self.line("\t\tpub fn set_deadline(&mut self, deadline: u64) {");
		self.line("\t\t\tself.deadline = deadline;");
		self.line("\t\t}");
		self.line("\t\tpub fn last_error(&self) -> Option<TransportError> {");
		self.line("\t\t\tself.last_error");
		self.line("\t\t}");
		self.line("\t\tpub fn into_transport(self) -> T {");
		self.line("\t\t\tself.transport");
		self.line("\t\t}");
		self.line("\t\tfn next_corr(&mut self) -> u32 {");
		self.line("\t\t\tlet c = self.corr;");
		self.line("\t\t\tself.corr = self.corr.wrapping_add(1);");
		self.line("\t\t\tc");
		self.line("\t\t}");
		// The caller side of the identity query. Deliberately NOT cached on `Client`: the
		// transport is synchronous and stateless and over a hundred call sites construct a
		// fresh `Client` inline, so a "once per connection" flag here would mean once per
		// temporary client - which is no saving and a misleading promise.
		self.line("\t\tpub fn protocol_info(&mut self) -> Option<(String, u32)> {");
		self.line("\t\t\tlet corr = self.next_corr();");
		self.line("\t\t\tlet mut writer = VecWriter::new();");
		self.line("\t\t\tlet w = &mut writer;");
		self.line("\t\t\tw.u16(PROTOCOL_INFO_OP)?;");
		self.line("\t\t\tw.u32(corr)?;");
		self.line("\t\t\t// No parameter, so no capability: `into_inner` says so rather than this");
		self.line("\t\t\t// line assuming it.");
		self.line("\t\t\tlet request = writer.into_inner()?;");
		self.line("\t\t\tlet mut reply_handles = Handles::new();");
		self.line("\t\t\tlet reply = self.transport.call(&request, &[], &mut reply_handles, self.deadline).map_err(|e| { self.last_error = Some(e); e }).ok()?;");
		self.line("\t\t\tif !reply_handles.is_empty() {");
		self.line("\t\t\t\tself.transport.discard_handles(reply_handles.as_slice());");
		self.line("\t\t\t\treturn None;");
		self.line("\t\t\t}");
		self.line("\t\t\tlet mut reader = Reader::new(&reply);");
		self.line("\t\t\tlet r = &mut reader;");
		self.line("\t\t\tif r.u32()? != corr {");
		self.line("\t\t\t\treturn None;");
		self.line("\t\t\t}");
		self.line("\t\t\tlet package = r.string_lp()?;");
		self.line("\t\t\tlet version = r.u32()?;");
		// The reply's framing boundary, for the reason the typed replies carry one: a package
		// name and a version followed by anything at all decoded as though they were not. This
		// path was written beside the typed one rather than through it, which is how it stayed
		// the only reply in the file that did not end here.
		self.line("\t\t\tr.finish()?;");
		self.line("\t\t\tSome((package, version))");
		self.line("\t\t}");
		for m in &supported {
			let params = client_params(m)?;
			if let Some((_, err)) = stream_return(&m.ret) {
				let ret = match err {
					Some(errty) => format!("Result<u64, {}>", rust_ty(errty).map_err(|e| Error::new(m.span, e))?),
					None => "u64".to_string(),
				};
				self.line(&format!("\t\tpub fn {}(&mut self{params}) -> Option<{ret}> {{", field_ident(&m.name)));
				self.line("\t\t\tlet corr = self.next_corr();");
				self.line("\t\t\tlet mut writer = VecWriter::new();");
				self.line("\t\t\tlet w = &mut writer;");
				self.line(&format!("\t\t\tw.u16(OP_{})?;", screaming(&m.name)));
				self.line("\t\t\tw.u32(corr)?;");
				for p in &m.params {
					let code = self.write_place(&p.ty, &field_ident(&p.name), true).map_err(|e| Error::new(p.span, e))?;
					self.line(&format!("\t\t\t{code}"));
				}
				self.line("\t\t\t// One call for both halves: the bytes cannot be taken without them.");
				self.line("\t\t\tlet (request, request_handles) = writer.into_message();");
				self.line("\t\t\tlet mut reply_handles = Handles::new();");
				self.line("\t\t\tlet reply = self.transport.call(&request, request_handles.as_slice(), &mut reply_handles, self.deadline).map_err(|e| { self.last_error = Some(e); e }).ok()?;");
				self.line("\t\t\tlet mut reader = Reader::new(&reply);");
				self.line("\t\t\tlet r = &mut reader;");
				// EXACTLY ONE, and the extras closed. This checked only that the list was NOT
				// EMPTY and then used `first()`, so a reply carrying `[channel, cap2, cap3]`
				// passed, one handle was used and the other two were never closed - `Handles` is
				// not an owner, so dropping it closes nothing. The ordinary reply path already
				// discards what the schema did not account for; this one did not.
				match err {
					None => {
						// `finish` HERE TOO. A stream-open reply is a message boundary, and this branch
						// checked the correlation id and the handle count and stopped - so a reply
						// carrying the right handle and trailing bytes opened the stream anyway. The
						// specialised path was written beside the ordinary reply rather than through it,
						// which is how it missed a rule the ordinary one states in a comment.
						self.line("\t\t\tif r.u32()? != corr || r.finish().is_none() || reply_handles.len() != 1 {");
						self.line("\t\t\t\tself.transport.discard_handles(reply_handles.as_slice());");
						self.line("\t\t\t\treturn None;");
						self.line("\t\t\t}");
						self.line("\t\t\tSome(reply_handles.first())");
					}
					Some(errty) => {
						// THE TAG DECIDES HOW MANY HANDLES THE REPLY MAY CARRY, and both arms are
						// bounded. An `Ok` with anything but one handle, or an `Err` with any, is a
						// reply this schema cannot have produced - and every one of those handles
						// is a capability, so they are discarded rather than dropped.
						self.line("\t\t\tlet decoded = (|| {");
						self.line("\t\t\t\tif r.u32()? != corr { return None; }");
						// `r.tag()?`, not `r.u8()? != 0`: one encoding per value, the same rule
						// `Reader::boolean` states. This spelling decoded `0xff` as `Ok`.
						self.line("\t\t\t\tif r.tag()? {");
						self.line("\t\t\t\t\tlet _ = r.u32()?;");
						// And the `Ok` arm ends at that placeholder: the stream itself arrives as the
						// handle, so the bytes are finished here.
						self.line("\t\t\t\t\tr.finish()?;");
						self.line("\t\t\t\t\tif reply_handles.len() != 1 { return None; }");
						self.line("\t\t\t\t\treturn Some(Ok(reply_handles.first()));");
						self.line("\t\t\t\t}");
						self.line("\t\t\t\tif !reply_handles.is_empty() { return None; }");
						let errexpr = self.read_value(errty).map_err(|e| Error::new(m.span, e))?;
						// AND THE ERROR ARM ENDS AT A BOUNDARY TOO. This read the error and returned
						// it, so a stream-open refusal with bytes after it decoded as an ordinary
						// refusal - the one arm of the one reply shape that did not check. The
						// error is bound before `finish` because the check is about what is LEFT
						// after the value, and a `?` inside the `Some(...)` would have ordered them
						// the other way round.
						self.line(&format!("\t\t\t\tlet error = {errexpr};"));
						self.line("\t\t\t\tr.finish()?;");
						self.line("\t\t\t\tSome(Err(error))");
						self.line("\t\t\t})();");
						self.line("\t\t\tif !matches!(decoded, Some(Ok(_))) {");
						self.line("\t\t\t\tself.transport.discard_handles(reply_handles.as_slice());");
						self.line("\t\t\t}");
						self.line("\t\t\tdecoded");
					}
				}
				self.line("\t\t}");
				continue;
			}
			let ret = rust_ty(&m.ret).map_err(|e| Error::new(m.span, e))?;
			self.line(&format!("\t\tpub fn {}(&mut self{params}) -> Option<{ret}> {{", field_ident(&m.name)));
			self.line("\t\t\tlet corr = self.next_corr();");
			self.line("\t\t\tlet mut writer = VecWriter::new();");
			self.line("\t\t\tlet w = &mut writer;");
			self.line(&format!("\t\t\tw.u16(OP_{})?;", screaming(&m.name)));
			self.line("\t\t\tw.u32(corr)?;");
			for p in &m.params {
				let code = self.write_place(&p.ty, &field_ident(&p.name), true).map_err(|e| Error::new(p.span, e))?;
				self.line(&format!("\t\t\t{code}"));
			}
			self.line("\t\t\t// One call for both halves: the bytes cannot be taken without them.");
			self.line("\t\t\tlet (request, request_handles) = writer.into_message();");
			self.line("\t\t\tlet mut reply_handles = Handles::new();");
			self.line("\t\t\tlet reply = self.transport.call(&request, request_handles.as_slice(), &mut reply_handles, self.deadline).map_err(|e| { self.last_error = Some(e); e }).ok()?;");
			self.line("\t\t\tlet mut reader = Reader::with_handle_list(&reply, &reply_handles);");
			let retexpr = self.read_value(&m.ret).map_err(|e| Error::new(m.span, e))?;
			self.line("\t\t\tlet decoded = (|| {");
			self.line("\t\t\t\tlet r = &mut reader;");
			self.line("\t\t\t\tif r.u32()? != corr { return None; }");
			self.line(&format!("\t\t\t\tlet value = {retexpr};"));
			// The reply's framing boundary. `has_handle` below caught an untaken capability and
			// nothing caught a trailing byte, so a reply could carry anything after its value and
			// decode as though it had not.
			self.line("\t\t\t\tr.finish()?;");
			self.line("\t\t\t\tSome(value)");
			self.line("\t\t\t})();");
			self.line("\t\t\tif decoded.is_none() {");
			self.line("\t\t\t\tself.transport.discard_handles(reply_handles.as_slice());");
			self.line("\t\t\t\treturn None;");
			self.line("\t\t\t}");
			self.line("\t\t\tdecoded");
			self.line("\t\t}");
		}
		self.line("\t}");
		self.line("");
		for m in &supported {
			let params = client_params(m)?;
			let args = m.params.iter().map(|param| field_ident(&param.name)).collect::<Vec<_>>().join(", ");
			let ret = match stream_return(&m.ret) {
				Some((_, None)) => "u64".to_string(),
				Some((_, Some(errty))) => format!("Result<u64, {}>", rust_ty(errty).map_err(|error| Error::new(m.span, error))?),
				None => rust_ty(&m.ret).map_err(|error| Error::new(m.span, error))?,
			};
			let symbol = format!("liber_channel_impl_{}_{}_{}", symbol_ident(&self.package), symbol_ident(&i.name), symbol_ident(&m.name));
			self.line("");
			self.line("\t#[cfg(feature = \"channel-client-impl\")]");
			self.line("\t#[inline(never)]");
			self.line(&format!("\t#[unsafe(export_name = \"{symbol}\")]"));
			self.line(&format!("\tfn channel_invoke_{}(chan: u64{params}) -> Option<{ret}> {{", field_ident(&m.name)));
			self.line("\t\tlet mut client = Client::new(ipc_client::ChannelTransport { chan });");
			self.line(&format!("\t\tclient.{}({args})", field_ident(&m.name)));
			self.line("\t}");
		}
		self.line("}");
		self.line("");
		Ok(())
	}

	// Emit statements that write the value at `place` (an expression of type `&ty`
	// when `is_ref`, otherwise of type `ty`) into the writer `w`.
	fn write_place(&mut self, ty: &Type, place: &str, is_ref: bool) -> Result<String, String> {
		let val = if is_ref { format!("*{place}") } else { place.to_string() };
		Ok(match ty {
			Type::Bool => format!("w.boolean({val})?;"),
			Type::U8 => format!("w.u8({val})?;"),
			Type::U16 => format!("w.u16({val})?;"),
			Type::U32 => format!("w.u32({val})?;"),
			Type::U64 => format!("w.u64({val})?;"),
			Type::I8 => format!("w.i8({val})?;"),
			Type::I16 => format!("w.i16({val})?;"),
			Type::I32 => format!("w.i32({val})?;"),
			Type::I64 => format!("w.i64({val})?;"),
			Type::F32 => format!("w.f32({val})?;"),
			Type::F64 => format!("w.f64({val})?;"),
			Type::String => format!("w.bytes_lp({place}.as_bytes())?;"),
			Type::Unit => String::new(),
			Type::Named(name) => match self.aliases.get(name).cloned() {
				Some(alias) => self.write_place(&alias, place, is_ref)?,
				None => format!("{place}.write(w)?;"),
			},
			Type::Option(inner) => {
				let refp = if is_ref { place.to_string() } else { format!("&{place}") };
				let b = self.fresh();
				let body = self.write_place(inner, &b, true)?;
				format!("match {refp} {{ Some({b}) => {{ w.u8(1)?; {body} }} None => {{ w.u8(0)?; }} }}")
			}
			Type::List(inner) => {
				let b = self.fresh();
				let body = self.write_place(inner, &b, true)?;
				format!("if {place}.len() > u16::MAX as usize {{ return None; }} w.u16({place}.len() as u16)?; for {b} in {place}.iter() {{ {body} }}")
			}
			Type::Tuple(elems) => {
				let refp = if is_ref { place.to_string() } else { format!("&{place}") };
				let binds: Vec<String> = (0..elems.len()).map(|_| self.fresh()).collect();
				let mut body = String::new();
				for (e, b) in elems.iter().zip(&binds) {
					let _ = write!(body, "{} ", self.write_place(e, b, true)?);
				}
				format!("{{ let ({}) = {refp}; {body}}}", binds.join(", "))
			}
			Type::Result(okty, errty) => {
				let refp = if is_ref { place.to_string() } else { format!("&{place}") };
				let bo = self.fresh();
				let be = self.fresh();
				let okb = self.write_place(okty, &bo, true)?;
				let errb = self.write_place(errty, &be, true)?;
				format!("match {refp} {{ Ok({bo}) => {{ w.u8(1)?; {okb} }} Err({be}) => {{ w.u8(0)?; {errb} }} }}")
			}
			Type::Handle(_) => format!("w.set_handle({val})?; w.u32(0)?;"),
			Type::Buffer => format!("w.set_handle({place}.handle)?; w.u64({place}.len)?;"),
			Type::Stream(_) => return Err("stream is not supported in a value position".into()),
		})
	}

	// Emit an expression that reads an owned value of `ty` from the reader `r`.
	fn read_value(&mut self, ty: &Type) -> Result<String, String> {
		Ok(match ty {
			Type::Bool => "r.boolean()?".into(),
			Type::U8 => "r.u8()?".into(),
			Type::U16 => "r.u16()?".into(),
			Type::U32 => "r.u32()?".into(),
			Type::U64 => "r.u64()?".into(),
			Type::I8 => "r.i8()?".into(),
			Type::I16 => "r.i16()?".into(),
			Type::I32 => "r.i32()?".into(),
			Type::I64 => "r.i64()?".into(),
			Type::F32 => "r.f32()?".into(),
			Type::F64 => "r.f64()?".into(),
			Type::String => "r.string_lp()?".into(),
			Type::Unit => "()".into(),
			Type::Named(n) => match self.aliases.get(n).cloned() {
				Some(alias) => self.read_value(&alias)?,
				None => format!("{}::read(r)?", camel(n)),
			},
			// `r.tag()?` - the strict discriminant. This was `r.u8()? != 0`, so every byte from 2 to
			// 255 decoded as `Some`, in a generator whose `bool` had been fixed for exactly that.
			Type::Option(inner) => format!("if r.tag()? {{ Some({}) }} else {{ None }}", self.read_value(inner)?),
			Type::List(inner) => {
				let n = self.fresh();
				let acc = self.fresh();
				// `try_reserve_exact` before the loop, the same rule `string_lp` next door was taught
				// and this was not. The count is bounded by `u16` and by the message size, so this
				// is not the unbounded-allocation class - it is the same model applied
				// inconsistently, in the file the fix was written for, and a decoder that aborts
				// rather than refuses is a decoder that hands a hostile message a way to end the
				// process reading it.
				format!("{{ let {n} = r.u16()? as usize; let mut {acc} = Vec::new(); {acc}.try_reserve_exact({n}).ok()?; for _ in 0..{n} {{ {acc}.push({}); }} {acc} }}", self.read_value(inner)?)
			}
			Type::Tuple(elems) => {
				let mut parts = Vec::new();
				for e in elems {
					parts.push(self.read_value(e)?);
				}
				format!("({})", parts.join(", "))
			}
			// The same strict tag: `0xff` used to decode as `Ok`, which is the malleability a reply
			// can least afford - it turns an error into a success with a garbage payload.
			Type::Result(okty, errty) => format!("if r.tag()? {{ Ok({}) }} else {{ Err({}) }}", self.read_value(okty)?, self.read_value(errty)?),
			Type::Handle(_) => "{ let _ = r.u32()?; r.take_handle()? }".into(),
			Type::Buffer => "{ let len = r.u64()?; let handle = r.take_handle()?; crate::codec::Buffer { handle, len } }".into(),
			Type::Stream(_) => return Err("stream is not supported in a value position".into()),
		})
	}

	// Human / JSON rendering, emitted as a second impl block per type.
	fn render_item(&mut self, item: &Item) -> Result<(), Error> {
		match item {
			Item::Alias(_) => Ok(()),
			Item::Record(r) => self.render_record(r),
			Item::Enum(e) => {
				self.render_enum(e);
				Ok(())
			}
			Item::Variant(v) => self.render_variant(v),
			Item::Flags(f) => {
				self.render_flags(f);
				Ok(())
			}
			Item::Resource(_) | Item::Interface(_) => Ok(()),
		}
	}

	// The public `to_json` wrapper shared by every renderable type.
	fn render_wrappers(&mut self) {
		self.line("\tpub fn to_json(&self) -> String {");
		self.line("\t\tlet mut s = String::new();");
		self.line("\t\tself.to_json_into(&mut s);");
		self.line("\t\ts");
		self.line("\t}");
		self.line("\tpub fn to_text(&self) -> String {");
		self.line("\t\tlet mut s = String::new();");
		self.line("\t\tself.to_text_into(&mut s);");
		self.line("\t\ts");
		self.line("\t}");
		self.line("\tpub fn to_cbor(&self) -> Vec<u8> {");
		self.line("\t\tlet mut v = Vec::new();");
		self.line("\t\tself.to_cbor_into(&mut v);");
		self.line("\t\tv");
		self.line("\t}");
	}

	fn render_record(&mut self, r: &Record) -> Result<(), Error> {
		let ty = camel(&r.name);
		self.line(&format!("impl {ty} {{"));
		self.render_wrappers();
		self.line("\tpub(crate) fn to_json_into(&self, out: &mut String) {");
		self.line("\t\tout.push('{');");
		for (idx, f) in r.fields.iter().enumerate() {
			if idx > 0 {
				self.line("\t\tout.push(',');");
			}
			self.line(&format!("\t\tout.push_str(\"\\\"{}\\\":\");", f.name));
			let code = self.json_value(&f.ty, &format!("self.{}", field_ident(&f.name)), false).map_err(|m| Error::new(f.span, m))?;
			self.line(&format!("\t\t{code}"));
		}
		self.line("\t\tout.push('}');");
		self.line("\t}");
		self.line("\tpub(crate) fn to_text_into(&self, out: &mut String) {");
		self.line("\t\tout.push('{');");
		for (idx, f) in r.fields.iter().enumerate() {
			if idx > 0 {
				self.line("\t\tout.push_str(\", \");");
			}
			self.line(&format!("\t\tout.push_str(\"{}=\");", f.name));
			let code = self.text_value(&f.ty, &format!("self.{}", field_ident(&f.name)), false).map_err(|m| Error::new(f.span, m))?;
			self.line(&format!("\t\t{code}"));
		}
		self.line("\t\tout.push('}');");
		self.line("\t}");
		self.line("\tpub(crate) fn to_cbor_into(&self, out: &mut Vec<u8>) {");
		self.line(&format!("\t\tcrate::codec::cbor::map(out, {});", r.fields.len()));
		for f in &r.fields {
			self.line(&format!("\t\tcrate::codec::cbor::text(out, \"{}\");", f.name));
			let code = self.cbor_value(&f.ty, &format!("self.{}", field_ident(&f.name)), false).map_err(|m| Error::new(f.span, m))?;
			self.line(&format!("\t\t{code}"));
		}
		self.line("\t}");
		self.line("}");
		self.line("");
		Ok(())
	}

	fn render_enum(&mut self, e: &Enum) {
		let ty = camel(&e.name);
		self.line(&format!("impl {ty} {{"));
		self.render_wrappers();
		self.line("\tpub(crate) fn to_json_into(&self, out: &mut String) {");
		self.line("\t\tmatch self {");
		for c in &e.cases {
			self.line(&format!("\t\t\t{ty}::{} => out.push_str(\"\\\"{}\\\"\"),", camel(&c.name), c.name));
		}
		self.line("\t\t}");
		self.line("\t}");
		self.line("\tpub(crate) fn to_text_into(&self, out: &mut String) {");
		self.line("\t\tmatch self {");
		for c in &e.cases {
			self.line(&format!("\t\t\t{ty}::{} => out.push_str(\"{}\"),", camel(&c.name), c.name));
		}
		self.line("\t\t}");
		self.line("\t}");
		self.line("\tpub(crate) fn to_cbor_into(&self, out: &mut Vec<u8>) {");
		self.line("\t\tmatch self {");
		for c in &e.cases {
			self.line(&format!("\t\t\t{ty}::{} => crate::codec::cbor::text(out, \"{}\"),", camel(&c.name), c.name));
		}
		self.line("\t\t}");
		self.line("\t}");
		self.line("}");
		self.line("");
	}

	fn render_variant(&mut self, v: &Variant) -> Result<(), Error> {
		let ty = camel(&v.name);
		self.line(&format!("impl {ty} {{"));
		self.render_wrappers();
		self.line("\tpub(crate) fn to_json_into(&self, out: &mut String) {");
		self.line("\t\tmatch self {");
		for c in &v.cases {
			match &c.payload {
				Some(p) => {
					let b = self.fresh();
					let body = self.json_value(p, &b, true).map_err(|m| Error::new(c.span, m))?;
					self.line(&format!("\t\t\t{ty}::{}({b}) => {{ out.push_str(\"{{\\\"{}\\\":\"); {body} out.push('}}'); }}", camel(&c.name), c.name));
				}
				None => self.line(&format!("\t\t\t{ty}::{} => out.push_str(\"\\\"{}\\\"\"),", camel(&c.name), c.name)),
			}
		}
		self.line("\t\t}");
		self.line("\t}");
		self.line("\tpub(crate) fn to_text_into(&self, out: &mut String) {");
		self.line("\t\tmatch self {");
		for c in &v.cases {
			match &c.payload {
				Some(p) => {
					let b = self.fresh();
					let body = self.text_value(p, &b, true).map_err(|m| Error::new(c.span, m))?;
					self.line(&format!("\t\t\t{ty}::{}({b}) => {{ out.push_str(\"{}(\"); {body} out.push(')'); }}", camel(&c.name), c.name));
				}
				None => self.line(&format!("\t\t\t{ty}::{} => out.push_str(\"{}\"),", camel(&c.name), c.name)),
			}
		}
		self.line("\t\t}");
		self.line("\t}");
		self.line("\tpub(crate) fn to_cbor_into(&self, out: &mut Vec<u8>) {");
		self.line("\t\tmatch self {");
		for c in &v.cases {
			match &c.payload {
				Some(p) => {
					let b = self.fresh();
					let body = self.cbor_value(p, &b, true).map_err(|m| Error::new(c.span, m))?;
					self.line(&format!("\t\t\t{ty}::{}({b}) => {{ crate::codec::cbor::map(out, 1); crate::codec::cbor::text(out, \"{}\"); {body} }}", camel(&c.name), c.name));
				}
				None => self.line(&format!("\t\t\t{ty}::{} => crate::codec::cbor::text(out, \"{}\"),", camel(&c.name), c.name)),
			}
		}
		self.line("\t\t}");
		self.line("\t}");
		self.line("}");
		self.line("");
		Ok(())
	}

	fn render_flags(&mut self, f: &Flags) {
		let ty = camel(&f.name);
		self.line(&format!("impl {ty} {{"));
		self.render_wrappers();
		self.line("\tpub(crate) fn to_json_into(&self, out: &mut String) {");
		self.line("\t\tout.push('[');");
		self.line("\t\tlet mut first = true;");
		for flag in &f.flags {
			self.line(&format!("\t\tif self.0 & Self::{} != 0 {{ if !first {{ out.push(','); }} first = false; out.push_str(\"\\\"{}\\\"\"); }}", screaming(&flag.name), flag.name));
		}
		self.line("\t\tout.push(']');");
		self.line("\t}");
		self.line("\tpub(crate) fn to_text_into(&self, out: &mut String) {");
		self.line("\t\tlet mut any = false;");
		for flag in &f.flags {
			self.line(&format!("\t\tif self.0 & Self::{} != 0 {{ if any {{ out.push('|'); }} any = true; out.push_str(\"{}\"); }}", screaming(&flag.name), flag.name));
		}
		self.line("\t\tif !any { out.push('-'); }");
		self.line("\t}");
		self.line("\tpub(crate) fn to_cbor_into(&self, out: &mut Vec<u8>) {");
		self.line("\t\tlet mut count = 0usize;");
		for flag in &f.flags {
			self.line(&format!("\t\tif self.0 & Self::{} != 0 {{ count += 1; }}", screaming(&flag.name)));
		}
		self.line("\t\tcrate::codec::cbor::array(out, count);");
		for flag in &f.flags {
			self.line(&format!("\t\tif self.0 & Self::{} != 0 {{ crate::codec::cbor::text(out, \"{}\"); }}", screaming(&flag.name), flag.name));
		}
		self.line("\t}");
		self.line("}");
		self.line("");
	}

	// Emit a statement appending the JSON of the value at `place` (`&ty` when
	// `is_ref`, else `ty`) to the in-scope `out: &mut String`.
	fn json_value(&mut self, ty: &Type, place: &str, is_ref: bool) -> Result<String, String> {
		let refplace = if is_ref { place.to_string() } else { format!("&{place}") };
		let boolexpr = if is_ref { format!("*{place}") } else { place.to_string() };
		Ok(match ty {
			Type::Bool => format!("if {boolexpr} {{ out.push_str(\"true\"); }} else {{ out.push_str(\"false\"); }}"),
			Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::F32 | Type::F64 => format!("let _ = write!(out, \"{{}}\", {place});"),
			Type::String => format!("crate::codec::json_escape({refplace}, out);"),
			Type::Unit => "out.push_str(\"null\");".to_string(),
			Type::Named(name) => match self.aliases.get(name).cloned() {
				Some(alias) => self.json_value(&alias, place, is_ref)?,
				None => format!("{place}.to_json_into(out);"),
			},
			Type::Option(inner) => {
				let v = self.fresh();
				let body = self.json_value(inner, &v, true)?;
				format!("match {refplace} {{ Some({v}) => {{ {body} }} None => {{ out.push_str(\"null\"); }} }}")
			}
			Type::List(inner) => {
				let v = self.fresh();
				let first = self.fresh();
				let body = self.json_value(inner, &v, true)?;
				format!("out.push('['); let mut {first} = true; for {v} in {place}.iter() {{ if !{first} {{ out.push(','); }} {first} = false; {body} }} out.push(']');")
			}
			Type::Tuple(elems) => {
				let binds: Vec<String> = (0..elems.len()).map(|_| self.fresh()).collect();
				let mut body = String::new();
				for (idx, (e, b)) in elems.iter().zip(&binds).enumerate() {
					if idx > 0 {
						body.push_str("out.push(','); ");
					}
					let _ = write!(body, "{} ", self.json_value(e, b, true)?);
				}
				format!("out.push('['); let ({}) = {refplace}; {body}out.push(']');", binds.join(", "))
			}
			Type::Result(okty, errty) => {
				let vo = self.fresh();
				let ve = self.fresh();
				let okb = self.json_value(okty, &vo, true)?;
				let errb = self.json_value(errty, &ve, true)?;
				format!("match {refplace} {{ Ok({vo}) => {{ out.push_str(\"{{\\\"ok\\\":\"); {okb} out.push('}}'); }} Err({ve}) => {{ out.push_str(\"{{\\\"err\\\":\"); {errb} out.push('}}'); }} }}")
			}
			Type::Handle(_) => format!("let _ = write!(out, \"{{}}\", {place});"),
			Type::Buffer => format!("let _ = write!(out, \"{{}}\", {place}.len);"),
			Type::Stream(_) => return Err("stream is not renderable".into()),
		})
	}

	// Emit a statement appending the human-readable text of the value at `place`
	// (`&ty` when `is_ref`, else `ty`) to the in-scope `out: &mut String`.
	fn text_value(&mut self, ty: &Type, place: &str, is_ref: bool) -> Result<String, String> {
		let refplace = if is_ref { place.to_string() } else { format!("&{place}") };
		let boolexpr = if is_ref { format!("*{place}") } else { place.to_string() };
		Ok(match ty {
			Type::Bool => format!("if {boolexpr} {{ out.push_str(\"true\"); }} else {{ out.push_str(\"false\"); }}"),
			Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::F32 | Type::F64 => format!("let _ = write!(out, \"{{}}\", {place});"),
			Type::String => format!("out.push_str({refplace});"),
			Type::Unit => String::new(),
			Type::Named(name) => match self.aliases.get(name).cloned() {
				Some(alias) => self.text_value(&alias, place, is_ref)?,
				None => format!("{place}.to_text_into(out);"),
			},
			Type::Option(inner) => {
				let v = self.fresh();
				let body = self.text_value(inner, &v, true)?;
				format!("match {refplace} {{ Some({v}) => {{ {body} }} None => {{ out.push('-'); }} }}")
			}
			Type::List(inner) => {
				let v = self.fresh();
				let first = self.fresh();
				let body = self.text_value(inner, &v, true)?;
				format!("out.push('['); let mut {first} = true; for {v} in {place}.iter() {{ if !{first} {{ out.push_str(\", \"); }} {first} = false; {body} }} out.push(']');")
			}
			Type::Tuple(elems) => {
				let binds: Vec<String> = (0..elems.len()).map(|_| self.fresh()).collect();
				let mut body = String::new();
				for (idx, (e, b)) in elems.iter().zip(&binds).enumerate() {
					if idx > 0 {
						body.push_str("out.push_str(\", \"); ");
					}
					let _ = write!(body, "{} ", self.text_value(e, b, true)?);
				}
				format!("out.push('('); let ({}) = {refplace}; {body}out.push(')');", binds.join(", "))
			}
			Type::Result(okty, errty) => {
				let vo = self.fresh();
				let ve = self.fresh();
				let okb = self.text_value(okty, &vo, true)?;
				let errb = self.text_value(errty, &ve, true)?;
				format!("match {refplace} {{ Ok({vo}) => {{ out.push_str(\"ok(\"); {okb} out.push(')'); }} Err({ve}) => {{ out.push_str(\"err(\"); {errb} out.push(')'); }} }}")
			}
			Type::Handle(_) => format!("let _ = write!(out, \"{{}}\", {place});"),
			Type::Buffer => format!("let _ = write!(out, \"{{}}\", {place}.len);"),
			Type::Stream(_) => return Err("stream is not renderable".into()),
		})
	}

	// Emit a statement appending the CBOR encoding of the value at `place` (`&ty`
	// when `is_ref`, else `ty`) to the in-scope `out: &mut Vec<u8>`. The CBOR shape
	// mirrors the JSON one: records are maps, enum cases are text, results are
	// single-pair maps, options collapse to `null`, lists/tuples are arrays.
	fn cbor_value(&mut self, ty: &Type, place: &str, is_ref: bool) -> Result<String, String> {
		let refplace = if is_ref { place.to_string() } else { format!("&{place}") };
		let valexpr = if is_ref { format!("*{place}") } else { place.to_string() };
		Ok(match ty {
			Type::Bool => format!("crate::codec::cbor::boolean(out, {valexpr});"),
			Type::U8 | Type::U16 | Type::U32 | Type::U64 => format!("crate::codec::cbor::uint(out, {valexpr} as u64);"),
			Type::I8 | Type::I16 | Type::I32 | Type::I64 => format!("crate::codec::cbor::int(out, {valexpr} as i64);"),
			Type::F32 => format!("crate::codec::cbor::f32(out, {valexpr});"),
			Type::F64 => format!("crate::codec::cbor::f64(out, {valexpr});"),
			Type::String => format!("crate::codec::cbor::text(out, {refplace});"),
			Type::Unit => "crate::codec::cbor::null(out);".to_string(),
			Type::Named(name) => match self.aliases.get(name).cloned() {
				Some(alias) => self.cbor_value(&alias, place, is_ref)?,
				None => format!("{place}.to_cbor_into(out);"),
			},
			Type::Option(inner) => {
				let v = self.fresh();
				let body = self.cbor_value(inner, &v, true)?;
				format!("match {refplace} {{ Some({v}) => {{ {body} }} None => {{ crate::codec::cbor::null(out); }} }}")
			}
			Type::List(inner) => {
				let v = self.fresh();
				let body = self.cbor_value(inner, &v, true)?;
				format!("crate::codec::cbor::array(out, {place}.len()); for {v} in {place}.iter() {{ {body} }}")
			}
			Type::Tuple(elems) => {
				let binds: Vec<String> = (0..elems.len()).map(|_| self.fresh()).collect();
				let mut body = String::new();
				for (e, b) in elems.iter().zip(&binds) {
					let _ = write!(body, "{} ", self.cbor_value(e, b, true)?);
				}
				format!("crate::codec::cbor::array(out, {}); let ({}) = {refplace}; {body}", elems.len(), binds.join(", "))
			}
			Type::Result(okty, errty) => {
				let vo = self.fresh();
				let ve = self.fresh();
				let okb = self.cbor_value(okty, &vo, true)?;
				let errb = self.cbor_value(errty, &ve, true)?;
				format!("match {refplace} {{ Ok({vo}) => {{ crate::codec::cbor::map(out, 1); crate::codec::cbor::text(out, \"ok\"); {okb} }} Err({ve}) => {{ crate::codec::cbor::map(out, 1); crate::codec::cbor::text(out, \"err\"); {errb} }} }}")
			}
			Type::Handle(_) => format!("crate::codec::cbor::uint(out, {valexpr} as u64);"),
			Type::Buffer => format!("crate::codec::cbor::uint(out, {place}.len);"),
			Type::Stream(_) => return Err("stream is not renderable".into()),
		})
	}

	// Emit a `#[cfg(test)] mod compat` pinning each type's wire bytes: a
	// deterministic sample, its golden encoding, and a round-trip check. The golden
	// bytes are computed here (host) and the test checks the generated codec agrees,
	// so an accidental ABI change shows up as a byte diff in review.
	fn compat_tests(&mut self, file: &File) {
		let defs = Defs::build(file, &self.aliases);
		self.line("use super::*;");
		self.line("use alloc::string::String;");
		self.line("");
		for item in &file.items {
			let name = match item {
				Item::Alias(_) => continue,
				Item::Record(r) => &r.name,
				Item::Enum(e) => &e.name,
				Item::Variant(v) => &v.name,
				Item::Flags(f) => &f.name,
				Item::Resource(_) | Item::Interface(_) => continue,
			};
			if let Some((expr, bytes)) = sample(&Type::Named(name.clone()), &defs) {
				let ty = camel(name);
				let golden = bytes.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", ");
				self.line("#[test]");
				self.line(&format!("fn {}_wire_is_stable() {{", field_ident(name)));
				self.line(&format!("\tlet sample = {expr};"));
				self.line("\tlet bytes = sample.encode_vec().expect(\"encode\");");
				self.line(&format!("\tlet golden: &[u8] = &[{golden}];"));
				self.line("\tassert_eq!(bytes, golden);");
				self.line(&format!("\tassert_eq!({ty}::decode(&bytes).unwrap(), sample);"));
				self.line("}");
			}
		}
	}
}

// Map a kebab-case name to a Rust type/variant identifier (CamelCase).
fn camel(name: &str) -> String {
	name.split('-')
		.map(|p| {
			let mut chars = p.chars();
			match chars.next() {
				Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
				None => String::new(),
			}
		})
		.collect()
}

// Map a kebab-case name to a Rust field/module identifier (snake_case), escaped
// as a raw identifier if it collides with a keyword.
fn field_ident(name: &str) -> String {
	let s = name.replace('-', "_");
	if is_rust_keyword(&s) { format!("r#{s}") } else { s }
}

fn symbol_ident(name: &str) -> String {
	name.bytes().map(|byte| if byte.is_ascii_alphanumeric() { byte as char } else { '_' }).collect()
}

// Map a kebab-case name to a SCREAMING_SNAKE_CASE constant identifier.
fn screaming(name: &str) -> String {
	name.replace('-', "_").to_ascii_uppercase()
}

// The Rust type for a value-position LSIDL type (used in struct fields).
fn rust_ty(ty: &Type) -> Result<String, String> {
	Ok(match ty {
		Type::Bool => "bool".into(),
		Type::U8 => "u8".into(),
		Type::U16 => "u16".into(),
		Type::U32 => "u32".into(),
		Type::U64 => "u64".into(),
		Type::I8 => "i8".into(),
		Type::I16 => "i16".into(),
		Type::I32 => "i32".into(),
		Type::I64 => "i64".into(),
		Type::F32 => "f32".into(),
		Type::F64 => "f64".into(),
		Type::String => "String".into(),
		Type::Unit => "()".into(),
		Type::Option(t) => format!("Option<{}>", rust_ty(t)?),
		Type::List(t) => format!("Vec<{}>", rust_ty(t)?),
		Type::Tuple(ts) => {
			let mut parts = Vec::new();
			for t in ts {
				parts.push(rust_ty(t)?);
			}
			format!("({})", parts.join(", "))
		}
		Type::Result(a, b) => format!("Result<{}, {}>", rust_ty(a)?, rust_ty(b)?),
		Type::Named(n) => camel(n),
		Type::Handle(_) => "u64".into(),
		Type::Buffer => "crate::codec::Buffer".into(),
		Type::Stream(_) => return Err("stream is not supported in a value position".into()),
	})
}

// The effective u8 ordinal of each enum case (explicit, else previous + 1).
fn effective_ordinals(e: &Enum) -> Vec<u32> {
	let mut out = Vec::with_capacity(e.cases.len());
	let mut next = 0u32;
	for c in &e.cases {
		let ord = c.ordinal.unwrap_or(next);
		out.push(ord);
		next = ord + 1;
	}
	out
}

// The smallest unsigned integer type that covers `count` flag bits.
fn flags_width(count: usize) -> &'static str {
	if count <= 8 {
		"u8"
	} else if count <= 16 {
		"u16"
	} else if count <= 32 {
		"u32"
	} else {
		"u64"
	}
}

// Whether a method's parameters and return type can all be carried by the codec. A
// `stream<T>` return is supported when its element type is (it is delivered over a
// sub-channel, not inline); a `buffer` is carried zero-copy as an out-of-band handle
// plus an in-stream length.
// The stream a method returns, if it returns one: its element type, and the error arm when the
// stream is wrapped in a `result`.
//
// `result<stream<T>, error>` PARSED AND GENERATED NOTHING. The generator matched `Type::Stream` on
// the return position directly, so a stream inside a result never reached that arm and the
// value-position paths refused it with "stream is not supported in a value position" - the method
// vanished from the trait, every consumer stopped compiling, and the schema could not express "this
// stream may fail before it starts". `Volume::list` therefore answered a directory it could not
// read with an empty listing, which is exactly the semantics this project spent a milestone
// removing everywhere else.
fn stream_return(ret: &Type) -> Option<(&Type, Option<&Type>)> {
	match ret {
		Type::Stream(elem) => Some((elem.as_ref(), None)),
		Type::Result(ok, err) => match ok.as_ref() {
			Type::Stream(elem) => Some((elem.as_ref(), Some(err.as_ref()))),
			_ => None,
		},
		_ => None,
	}
}

fn method_supported(m: &Method) -> bool {
	if !m.params.iter().all(|p| type_codec_ok(&p.ty)) {
		return false;
	}
	match stream_return(&m.ret) {
		Some((elem, err)) => type_codec_ok(elem) && err.is_none_or(type_codec_ok),
		None => type_codec_ok(&m.ret),
	}
}

fn type_codec_ok(ty: &Type) -> bool {
	match ty {
		Type::Buffer => true,
		Type::Stream(_) => false,
		Type::Handle(_) => true,
		Type::Option(t) | Type::List(t) => type_codec_ok(t),
		Type::Tuple(ts) => ts.iter().all(type_codec_ok),
		Type::Result(a, b) => type_codec_ok(a) && type_codec_ok(b),
		_ => true,
	}
}

// A by-reference Rust type for a client parameter (`string` -> `&str`).
fn param_ref_ty(ty: &Type) -> Result<String, String> {
	Ok(match ty {
		Type::String => "&str".into(),
		_ => format!("&{}", rust_ty(ty)?),
	})
}

// The owned-parameter list of a Service trait method (e.g. `, e: Entry`).
fn trait_params(m: &Method) -> Result<String, Error> {
	let mut s = String::new();
	for p in &m.params {
		let ty = rust_ty(&p.ty).map_err(|e| Error::new(p.span, e))?;
		let _ = write!(s, ", {}: {ty}", field_ident(&p.name));
	}
	Ok(s)
}

// The by-reference parameter list of a Client method (e.g. `, e: &Entry`).
fn client_params(m: &Method) -> Result<String, Error> {
	let mut s = String::new();
	for p in &m.params {
		let ty = param_ref_ty(&p.ty).map_err(|e| Error::new(p.span, e))?;
		let _ = write!(s, ", {}: {ty}", field_ident(&p.name));
	}
	Ok(s)
}

fn is_rust_keyword(s: &str) -> bool {
	const KW: &[&str] = &[
		"as",
		"break",
		"const",
		"continue",
		"crate",
		"dyn",
		"else",
		"enum",
		"extern",
		"false",
		"fn",
		"for",
		"if",
		"impl",
		"in",
		"let",
		"loop",
		"match",
		"mod",
		"move",
		"mut",
		"pub",
		"ref",
		"return",
		"self",
		"static",
		"struct",
		"super",
		"trait",
		"true",
		"type",
		"unsafe",
		"use",
		"where",
		"while",
		"async",
		"await",
		"abstract",
		"become",
		"box",
		"do",
		"final",
		"macro",
		"override",
		"priv",
		"typeof",
		"unsized",
		"virtual",
		"yield",
		"try",
	];
	KW.contains(&s)
}

// Render a type back to its LSIDL textual form (for documentation).
fn ty_to_lsidl(ty: &Type) -> String {
	match ty {
		Type::Bool => "bool".into(),
		Type::U8 => "u8".into(),
		Type::U16 => "u16".into(),
		Type::U32 => "u32".into(),
		Type::U64 => "u64".into(),
		Type::I8 => "i8".into(),
		Type::I16 => "i16".into(),
		Type::I32 => "i32".into(),
		Type::I64 => "i64".into(),
		Type::F32 => "f32".into(),
		Type::F64 => "f64".into(),
		Type::String => "string".into(),
		Type::Unit => "unit".into(),
		Type::Buffer => "buffer".into(),
		Type::Option(t) => format!("option<{}>", ty_to_lsidl(t)),
		Type::List(t) => format!("list<{}>", ty_to_lsidl(t)),
		Type::Stream(t) => format!("stream<{}>", ty_to_lsidl(t)),
		Type::Tuple(ts) => format!("tuple<{}>", ts.iter().map(ty_to_lsidl).collect::<Vec<_>>().join(", ")),
		Type::Result(a, b) => format!("result<{}, {}>", ty_to_lsidl(a), ty_to_lsidl(b)),
		Type::Handle(r) => format!("handle<{r}>"),
		Type::Named(n) => n.clone(),
	}
}

// The file's base name (no directory), for display in generated headers.
fn basename(path: &str) -> &str {
	path.rsplit('/').next().unwrap_or(path)
}

// Generate a Markdown reference for one file: its types and interfaces.
pub fn docs(file: &File, source: &str) -> String {
	let mut out = String::new();
	let pkg = file.package.path.join(":");
	let name = basename(source);
	let _ = writeln!(out, "<!-- @generated by lsidl-gen from {name}. Do not edit; run `./gen.sh`. -->");
	let _ = writeln!(out, "# `{pkg}@{}`", file.package.version);
	let _ = writeln!(out);
	let _ = writeln!(out, "Generated reference for the `{pkg}` package (`{name}`). See [the LSIDL language](../../../LSIDL.md).");
	let _ = writeln!(out);
	write_markdown_doc(&mut out, &file.package_doc);

	if !file.uses.is_empty() {
		let _ = writeln!(out, "## Imports");
		let _ = writeln!(out);
		for u in &file.uses {
			let names: Vec<String> = u
				.names
				.iter()
				.map(|name| match &name.alias {
					Some(alias) => format!("{} as {alias}", name.name),
					None => name.name.clone(),
				})
				.collect();
			let target = format!("/docs/gen/{}/v{}.md", u.path.join("/"), u.version);
			let _ = writeln!(out, "- `{}` from [`{}@{}`]({target})", names.join(", "), u.path.join(":"), u.version);
		}
		let _ = writeln!(out);
	}

	if file.items.iter().any(|i| !matches!(i, Item::Interface(_))) {
		let _ = writeln!(out, "## Types");
		let _ = writeln!(out);
		for item in &file.items {
			match item {
				Item::Alias(alias) => {
					let _ = writeln!(out, "### type `{}`", alias.name);
					let _ = writeln!(out);
					let _ = writeln!(out, "{}\n", markdown_description(&alias.doc, alias.evolution));
					let _ = writeln!(out, "Wire-transparent alias of `{}`.\n", ty_to_lsidl(&alias.ty));
				}
				Item::Record(r) => {
					let _ = writeln!(out, "### record `{}`", r.name);
					let _ = writeln!(out);
					let _ = writeln!(out, "{}\n", markdown_description(&r.doc, r.evolution));
					let _ = writeln!(out, "| field | type | description |");
					let _ = writeln!(out, "| --- | --- | --- |");
					for f in &r.fields {
						let _ = writeln!(out, "| `{}` | `{}` | {} |", f.name, ty_to_lsidl(&f.ty), markdown_description(&f.doc, f.evolution));
					}
					let _ = writeln!(out);
				}
				Item::Enum(e) => {
					let ords = effective_ordinals(e);
					let _ = writeln!(out, "### enum `{}`", e.name);
					let _ = writeln!(out);
					let _ = writeln!(out, "{}\n", markdown_description(&e.doc, e.evolution));
					let _ = writeln!(out, "| case | ordinal | description |");
					let _ = writeln!(out, "| --- | --- | --- |");
					for (c, ord) in e.cases.iter().zip(&ords) {
						let _ = writeln!(out, "| `{}` | {ord} | {} |", c.name, markdown_description(&c.doc, c.evolution));
					}
					let _ = writeln!(out);
				}
				Item::Variant(v) => {
					let _ = writeln!(out, "### variant `{}`", v.name);
					let _ = writeln!(out);
					let _ = writeln!(out, "{}\n", markdown_description(&v.doc, v.evolution));
					let _ = writeln!(out, "| tag | case | payload | description |");
					let _ = writeln!(out, "| --- | --- | --- | --- |");
					for (tag, c) in v.cases.iter().enumerate() {
						let payload = c.payload.as_ref().map(ty_to_lsidl).unwrap_or_else(|| "-".into());
						let _ = writeln!(out, "| {tag} | `{}` | `{}` | {} |", c.name, payload, markdown_description(&c.doc, c.evolution));
					}
					let _ = writeln!(out);
				}
				Item::Flags(f) => {
					let _ = writeln!(out, "### flags `{}`", f.name);
					let _ = writeln!(out);
					let _ = writeln!(out, "{}\n", markdown_description(&f.doc, f.evolution));
					let _ = writeln!(out, "| flag | bit | description |");
					let _ = writeln!(out, "| --- | --- | --- |");
					for (i, fl) in f.flags.iter().enumerate() {
						let _ = writeln!(out, "| `{}` | {i} | {} |", fl.name, markdown_description(&fl.doc, fl.evolution));
					}
					let _ = writeln!(out);
				}
				Item::Resource(r) => {
					let _ = writeln!(out, "### resource `{}`", r.name);
					let _ = writeln!(out);
					let _ = writeln!(out, "{}\n", markdown_description(&r.doc, r.evolution));
					let _ = writeln!(out, "An opaque kernel object, transferred as `handle<{}>`.", r.name);
					let _ = writeln!(out);
				}
				Item::Interface(_) => {}
			}
		}
	}

	let interfaces: Vec<&Interface> = file.items.iter().filter_map(|i| if let Item::Interface(x) = i { Some(x) } else { None }).collect();
	if !interfaces.is_empty() {
		let _ = writeln!(out, "## Interfaces");
		let _ = writeln!(out);
		for i in &interfaces {
			let _ = writeln!(out, "### interface `{}`", i.name);
			let _ = writeln!(out);
			let _ = writeln!(out, "{}\n", markdown_description(&i.doc, i.evolution));
			let _ = writeln!(out, "Request `[op u16][corr u32][args]`, reply `[corr u32][result]`.");
			let _ = writeln!(out);
			let _ = writeln!(out, "| op | method | signature | description |");
			let _ = writeln!(out, "| --- | --- | --- | --- |");
			for m in &i.methods {
				let params: Vec<String> = m.params.iter().map(|p| format!("{}: {}", p.name, ty_to_lsidl(&p.ty))).collect();
				let sig = format!("{}({}) -> {}", m.name, params.join(", "), ty_to_lsidl(&m.ret));
				let mut description = markdown_description(&m.doc, m.evolution);
				let param_docs: Vec<String> = m
					.params
					.iter()
					.filter_map(|param| {
						let description = markdown_description(&param.doc, param.evolution);
						if description.is_empty() { None } else { Some(format!("`{}`: {description}", param.name)) }
					})
					.collect();
				if !param_docs.is_empty() {
					if !description.is_empty() {
						description.push_str(" ");
					}
					description.push_str(&param_docs.join("; "));
				}
				let _ = writeln!(out, "| {} | `{}` | `{}` | {} |", m.op, m.name, sig, description);
			}
			let _ = writeln!(out);
		}
	}

	out
}

pub fn abi_manifest(file: &File, imports: &HashMap<String, ResolvedSymbol>) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "package {}@{}", file.package.path.join(":"), file.package.version);
	let aliases: HashMap<&str, &Type> = file.items.iter().filter_map(|item| if let Item::Alias(alias) = item { Some((alias.name.as_str(), &alias.ty)) } else { None }).collect();
	for item in &file.items {
		match item {
			Item::Alias(alias) => {
				let _ = writeln!(out, "alias {}={}", alias.name, abi_type(&alias.ty, &aliases, imports, 0));
				manifest_meta(&mut out, &format!("alias {}", alias.name), alias.evolution);
			}
			Item::Record(record) => {
				let fields: Vec<String> = record.fields.iter().map(|field| format!("{}:{}", field.name, abi_type(&field.ty, &aliases, imports, 0))).collect();
				let _ = writeln!(out, "record {}({})", record.name, fields.join(","));
				manifest_meta(&mut out, &format!("record {}", record.name), record.evolution);
				for field in &record.fields {
					manifest_meta(&mut out, &format!("field {}.{}", record.name, field.name), field.evolution);
				}
			}
			Item::Enum(item) => {
				let cases: Vec<String> = item.cases.iter().zip(effective_ordinals(item)).map(|(case, ordinal)| format!("{}={ordinal}", case.name)).collect();
				let _ = writeln!(out, "enum {}({})", item.name, cases.join(","));
				manifest_meta(&mut out, &format!("enum {}", item.name), item.evolution);
				for case in &item.cases {
					manifest_meta(&mut out, &format!("case {}.{}", item.name, case.name), case.evolution);
				}
				for reserved in &item.reserved {
					let _ = writeln!(out, "reserved enum {} {reserved}", item.name);
				}
			}
			Item::Variant(item) => {
				let cases: Vec<String> = item.cases.iter().enumerate().map(|(tag, case)| format!("{}={tag}:{}", case.name, case.payload.as_ref().map(|ty| abi_type(ty, &aliases, imports, 0)).unwrap_or_else(|| "unit".into()))).collect();
				let _ = writeln!(out, "variant {}({})", item.name, cases.join(","));
				manifest_meta(&mut out, &format!("variant {}", item.name), item.evolution);
			}
			Item::Flags(item) => {
				let names: Vec<&str> = item.flags.iter().map(|flag| flag.name.as_str()).collect();
				let _ = writeln!(out, "flags {} width={} ({})", item.name, flags_width(item.flags.len()), names.join(","));
				manifest_meta(&mut out, &format!("flags {}", item.name), item.evolution);
			}
			Item::Resource(item) => {
				let _ = writeln!(out, "resource {}", item.name);
				manifest_meta(&mut out, &format!("resource {}", item.name), item.evolution);
			}
			Item::Interface(item) => {
				let _ = writeln!(out, "interface {}", item.name);
				manifest_meta(&mut out, &format!("interface {}", item.name), item.evolution);
				for method in &item.methods {
					let params: Vec<String> = method.params.iter().map(|param| format!("{}:{}:rights={}", param.name, abi_type(&param.ty, &aliases, imports, 0), param.rights.join("+"))).collect();
					let _ = writeln!(out, "method {}.{} op={} ({}) -> {}", item.name, method.name, method.op, params.join(","), abi_type(&method.ret, &aliases, imports, 0));
					manifest_meta(&mut out, &format!("method {}.{}", item.name, method.name), method.evolution);
					for param in &method.params {
						manifest_meta(&mut out, &format!("param {}.{}.{}", item.name, method.name, param.name), param.evolution);
					}
				}
				for reserved in &item.reserved {
					let _ = writeln!(out, "reserved interface {} {reserved}", item.name);
				}
			}
		}
	}
	out
}

fn abi_type(ty: &Type, aliases: &HashMap<&str, &Type>, imports: &HashMap<String, ResolvedSymbol>, depth: usize) -> String {
	if depth >= 64 {
		return ty_to_lsidl(ty);
	}
	match ty {
		Type::Named(name) => {
			if let Some(alias) = aliases.get(name.as_str()) {
				return abi_type(alias, aliases, imports, depth + 1);
			}
			if let Some(symbol) = imports.get(name) {
				if let Some(wire_type) = &symbol.wire_type {
					return abi_type(wire_type, aliases, imports, depth + 1);
				}
				return format!("{}::{}", symbol.package.display(), symbol.source_name);
			}
			name.clone()
		}
		Type::Handle(resource) => imports.get(resource).map(|symbol| format!("handle<{}::{}>", symbol.package.display(), symbol.source_name)).unwrap_or_else(|| format!("handle<{resource}>")),
		Type::Option(inner) => format!("option<{}>", abi_type(inner, aliases, imports, depth + 1)),
		Type::List(inner) => format!("list<{}>", abi_type(inner, aliases, imports, depth + 1)),
		Type::Stream(inner) => format!("stream<{}>", abi_type(inner, aliases, imports, depth + 1)),
		Type::Tuple(items) => format!("tuple<{}>", items.iter().map(|item| abi_type(item, aliases, imports, depth + 1)).collect::<Vec<_>>().join(", ")),
		Type::Result(ok, err) => format!("result<{}, {}>", abi_type(ok, aliases, imports, depth + 1), abi_type(err, aliases, imports, depth + 1)),
		_ => ty_to_lsidl(ty),
	}
}

fn manifest_meta(out: &mut String, owner: &str, evolution: Evolution) {
	if evolution.since.is_some() || evolution.deprecated.is_some() {
		let _ = writeln!(out, "meta {owner} since={} deprecated={}", evolution.since.map(|value| value.to_string()).unwrap_or_else(|| "-".into()), evolution.deprecated.map(|value| value.to_string()).unwrap_or_else(|| "-".into()));
	}
}

// The named-type definitions of a file, for building deterministic samples.
struct Defs<'a> {
	aliases: HashMap<String, Type>,
	records: HashMap<&'a str, &'a Record>,
	enums: HashMap<&'a str, &'a Enum>,
	variants: HashMap<&'a str, &'a Variant>,
	flags: HashMap<&'a str, &'a Flags>,
}

impl<'a> Defs<'a> {
	fn build(file: &'a File, aliases: &HashMap<String, Type>) -> Defs<'a> {
		let mut d = Defs { aliases: aliases.clone(), records: HashMap::new(), enums: HashMap::new(), variants: HashMap::new(), flags: HashMap::new() };
		for item in &file.items {
			match item {
				Item::Alias(_) => {}
				Item::Record(r) => {
					d.records.insert(r.name.as_str(), r);
				}
				Item::Enum(e) => {
					d.enums.insert(e.name.as_str(), e);
				}
				Item::Variant(v) => {
					d.variants.insert(v.name.as_str(), v);
				}
				Item::Flags(f) => {
					d.flags.insert(f.name.as_str(), f);
				}
				Item::Resource(_) | Item::Interface(_) => {}
			}
		}
		d
	}
}

// Build a deterministic sample of `ty`: its Rust literal expression and the exact
// bytes that expression must encode to. Returns None for types the codec cannot
// carry (handle/buffer/stream) or that reference an unknown/imported type.
fn sample(ty: &Type, defs: &Defs) -> Option<(String, Vec<u8>)> {
	Some(match ty {
		Type::Bool => ("true".into(), vec![1]),
		Type::U8 => ("7".into(), vec![7]),
		Type::U16 => ("7".into(), vec![7, 0]),
		Type::U32 => ("7".into(), vec![7, 0, 0, 0]),
		Type::U64 => ("7".into(), vec![7, 0, 0, 0, 0, 0, 0, 0]),
		Type::I8 => ("7".into(), vec![7]),
		Type::I16 => ("7".into(), vec![7, 0]),
		Type::I32 => ("7".into(), vec![7, 0, 0, 0]),
		Type::I64 => ("7".into(), vec![7, 0, 0, 0, 0, 0, 0, 0]),
		Type::F32 => ("1.5f32".into(), 1.5f32.to_le_bytes().to_vec()),
		Type::F64 => ("1.5f64".into(), 1.5f64.to_le_bytes().to_vec()),
		Type::String => ("String::from(\"x\")".into(), vec![1, 0, b'x']),
		Type::Unit => ("()".into(), Vec::new()),
		Type::Option(inner) => {
			let (e, mut b) = sample(inner, defs)?;
			let mut bytes = vec![1u8];
			bytes.append(&mut b);
			(format!("Some({e})"), bytes)
		}
		Type::List(inner) => {
			let (e, mut b) = sample(inner, defs)?;
			let mut bytes = vec![1u8, 0u8];
			bytes.append(&mut b);
			(format!("alloc::vec![{e}]"), bytes)
		}
		Type::Tuple(elems) => {
			let mut exprs = Vec::new();
			let mut bytes = Vec::new();
			for e in elems {
				let (ex, mut b) = sample(e, defs)?;
				exprs.push(ex);
				bytes.append(&mut b);
			}
			(format!("({})", exprs.join(", ")), bytes)
		}
		Type::Result(okty, _errty) => {
			let (e, mut b) = sample(okty, defs)?;
			let mut bytes = vec![1u8];
			bytes.append(&mut b);
			(format!("Ok({e})"), bytes)
		}
		Type::Handle(_) | Type::Buffer | Type::Stream(_) => return None,
		Type::Named(n) => {
			if let Some(alias) = defs.aliases.get(n) {
				sample(alias, defs)?
			} else if let Some(e) = defs.enums.get(n.as_str()) {
				let ords = effective_ordinals(e);
				let first = e.cases.first()?;
				let ord = *ords.first()?;
				(format!("{}::{}", camel(n), camel(&first.name)), vec![ord as u8])
			} else if let Some(r) = defs.records.get(n.as_str()) {
				let mut parts = Vec::new();
				let mut bytes = Vec::new();
				for f in &r.fields {
					let (ex, mut b) = sample(&f.ty, defs)?;
					parts.push(format!("{}: {}", field_ident(&f.name), ex));
					bytes.append(&mut b);
				}
				(format!("{} {{ {} }}", camel(n), parts.join(", ")), bytes)
			} else if let Some(v) = defs.variants.get(n.as_str()) {
				let first = v.cases.first()?;
				match &first.payload {
					Some(p) => {
						let (ex, mut b) = sample(p, defs)?;
						let mut bytes = vec![0u8];
						bytes.append(&mut b);
						(format!("{}::{}({ex})", camel(n), camel(&first.name)), bytes)
					}
					None => (format!("{}::{}", camel(n), camel(&first.name)), vec![0u8]),
				}
			} else if let Some(f) = defs.flags.get(n.as_str()) {
				let width_bytes = match flags_width(f.flags.len()) {
					"u8" => 1,
					"u16" => 2,
					"u32" => 4,
					_ => 8,
				};
				(format!("{}(0)", camel(n)), vec![0u8; width_bytes])
			} else {
				return None;
			}
		}
	})
}
