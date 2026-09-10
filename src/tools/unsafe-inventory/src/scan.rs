// The source-level scan: one crate root, every module file it reaches, every boundary in them.
//
// WHAT A SITE IS. A place in the SOURCE where the type system stops proving something and a
// person has to: an `unsafe fn`, an `unsafe` block, an `unsafe impl` or `unsafe trait`, a raw
// pointer in a signature or a field, a mutable static, inline or global assembly, an `extern`
// block or an `extern "C"` function, and a linkage attribute (`no_mangle`, `export_name`,
// `link_section`, `naked`). A `macro_rules!` whose template contains one of those is a site of its
// own kind - a TEMPLATE - because what it expands to is decided at every invocation and a scan of
// the source sees the template once.
//
// WHAT A SITE KNOWS. Its crate, file and line; the item it is in (a module path, an impl target
// and a function); the `cfg` chain above it, evaluated against a configuration row so a site
// behind `#[cfg(target_arch = "riscv64")]` is REACHABLE in the riscv64 row and not in the others,
// and a site behind `#[cfg(test)]` is test-only in every shipping row; and its provenance - a
// source file, a generated file included from `OUT_DIR`, or a macro template.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Expr, ImplItem, Item, Meta, Type};

// One configuration's answers to `cfg(...)` predicates: bare names and name = "value" pairs.
#[derive(Clone, Debug, Default)]
pub struct CfgSet {
	pub names: Vec<String>,
	pub pairs: Vec<(String, String)>,
}

impl CfgSet {
	pub fn parse(entries: &[String]) -> CfgSet {
		let mut set = CfgSet::default();
		for entry in entries {
			if let Some((name, value)) = entry.split_once('=') {
				let value = value.trim().trim_matches('"');
				set.pairs.push((name.trim().to_string(), value.to_string()));
			} else {
				set.names.push(entry.trim().to_string());
			}
		}
		set
	}

	fn has(&self, name: &str) -> bool {
		self.names.iter().any(|n| n == name)
	}

	fn has_pair(&self, name: &str, value: &str) -> bool {
		self.pairs.iter().any(|(n, v)| n == name && v == value)
	}
}

// Evaluate one `cfg` predicate. A name this row does not define is false - the conservative
// answer, and the row's `cfg` list is where a missing one is added.
pub fn eval_cfg(meta: &Meta, set: &CfgSet) -> bool {
	match meta {
		Meta::Path(path) => {
			let name = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
			set.has(&name)
		}
		Meta::NameValue(nv) => {
			let name = nv.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
			let value = match &nv.value {
				Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => s.value(),
				_ => return false,
			};
			set.has_pair(&name, &value)
		}
		Meta::List(list) => {
			let name = list.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
			let nested: Vec<Meta> = list.parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated).map(|p| p.into_iter().collect()).unwrap_or_default();
			match name.as_str() {
				"all" => nested.iter().all(|m| eval_cfg(m, set)),
				"any" => nested.iter().any(|m| eval_cfg(m, set)),
				"not" => nested.first().map(|m| !eval_cfg(m, set)).unwrap_or(false),
				_ => false,
			}
		}
	}
}

fn meta_text(meta: &Meta) -> String {
	quote_meta(meta)
}

fn quote_meta(meta: &Meta) -> String {
	use quote_to_string::ToTokensString;
	meta.to_token_string()
}

mod quote_to_string {
	pub trait ToTokensString {
		fn to_token_string(&self) -> String;
	}
	impl<T: syn::__private::ToTokens> ToTokensString for T {
		fn to_token_string(&self) -> String {
			let mut tokens = proc_macro2::TokenStream::new();
			self.to_tokens(&mut tokens);
			tokens.to_string()
		}
	}
}

// Whether `#[cfg(test)]` (or a predicate that mentions `test`) gates this item.
fn mentions_test(meta: &Meta) -> bool {
	meta_text(meta).split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "test")
}

// What the attributes on an item decide: the cfg chain, whether the item is active in this row,
// whether it is test-only, an explicit `#[path]`, and the linkage attributes on it.
#[derive(Clone, Debug, Default)]
pub struct AttrFacts {
	pub cfgs: Vec<String>,
	pub active: bool,
	pub test_only: bool,
	pub path: Option<String>,
	pub linkage: Vec<String>,
	// `#[automatically_derived]`: the compiler's own derive output, seen only in expanded code - a
	// `derive(Clone, Copy)` emits `unsafe impl TrivialClone` on this toolchain. Not a boundary of
	// this tree's, and skipped so the reconciliation compares like with like.
	pub derived: bool,
}

pub fn attr_facts(attrs: &[Attribute], set: &CfgSet) -> AttrFacts {
	let mut facts = AttrFacts { active: true, ..AttrFacts::default() };
	for attr in attrs {
		let ident = attr.path().segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
		match ident.as_str() {
			"cfg" => {
				if let Ok(meta) = attr.parse_args::<Meta>() {
					facts.cfgs.push(meta_text(&meta));
					if mentions_test(&meta) && !set.has("test") {
						facts.test_only = true;
					}
					if !eval_cfg(&meta, set) {
						facts.active = false;
					}
				}
			}
			"cfg_attr" => {
				// `cfg_attr(pred, path = "...")` is the one form this scan needs; anything else it
				// records as a cfg it saw and evaluates the predicate for.
				let parsed = attr.parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated);
				if let Ok(list) = parsed {
					let mut it = list.into_iter();
					if let Some(pred) = it.next() {
						facts.cfgs.push(format!("cfg_attr({})", meta_text(&pred)));
						if eval_cfg(&pred, set) {
							for inner in it {
								if let Meta::NameValue(nv) = &inner
									&& nv.path.is_ident("path") && let Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value
								{
									facts.path = Some(s.value());
								}
								let name = inner.path().segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
								if is_linkage_attr(&name) {
									facts.linkage.push(name);
								}
							}
						}
					}
				}
			}
			"automatically_derived" => facts.derived = true,
			"path" => {
				if let Meta::NameValue(nv) = &attr.meta
					&& let Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value
				{
					facts.path = Some(s.value());
				}
			}
			"unsafe" => {
				// `#[unsafe(no_mangle)]`, `#[unsafe(export_name = "...")]`, `#[unsafe(naked)]` ...
				if let Ok(inner) = attr.parse_args::<Meta>() {
					let name = inner.path().segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
					if is_linkage_attr(&name) {
						facts.linkage.push(format!("unsafe({name})"));
					}
				}
			}
			other if is_linkage_attr(other) => facts.linkage.push(other.to_string()),
			_ => {}
		}
	}
	facts
}

fn is_linkage_attr(name: &str) -> bool {
	matches!(name, "no_mangle" | "export_name" | "link_section" | "naked" | "link" | "link_name")
}

// The kinds of boundary this scan records.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
	UnsafeFn,
	UnsafeBlock,
	UnsafeImpl,
	UnsafeTrait,
	RawPointer,
	StaticMut,
	InlineAsm,
	GlobalAsm,
	NakedAsm,
	ExternBlock,
	ExternAbiFn,
	LinkageAttr,
	MacroTemplate,
}

impl Kind {
	pub fn name(self) -> &'static str {
		match self {
			Kind::UnsafeFn => "unsafe-fn",
			Kind::UnsafeBlock => "unsafe-block",
			Kind::UnsafeImpl => "unsafe-impl",
			Kind::UnsafeTrait => "unsafe-trait",
			Kind::RawPointer => "raw-pointer",
			Kind::StaticMut => "static-mut",
			Kind::InlineAsm => "inline-asm",
			Kind::GlobalAsm => "global-asm",
			Kind::NakedAsm => "naked-asm",
			Kind::ExternBlock => "extern-block",
			Kind::ExternAbiFn => "extern-abi-fn",
			Kind::LinkageAttr => "linkage-attr",
			Kind::MacroTemplate => "macro-template",
		}
	}

	pub fn all() -> [Kind; 13] {
		[
			Kind::UnsafeFn,
			Kind::UnsafeBlock,
			Kind::UnsafeImpl,
			Kind::UnsafeTrait,
			Kind::RawPointer,
			Kind::StaticMut,
			Kind::InlineAsm,
			Kind::GlobalAsm,
			Kind::NakedAsm,
			Kind::ExternBlock,
			Kind::ExternAbiFn,
			Kind::LinkageAttr,
			Kind::MacroTemplate,
		]
	}
}

// Where a scanned file came from.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Provenance {
	Source,
	// A file the crate's build script wrote into `OUT_DIR`, included by the named source site.
	Generated { generator: String, included_from: String },
}

// One boundary, as the scan found it in one row.
#[derive(Clone, Debug)]
pub struct Found {
	pub crate_name: String,
	pub file: String,
	pub line: usize,
	pub kind: Kind,
	pub item: String,
	// The ordinal of this kind within the item, so the id is stable under unrelated edits.
	pub ordinal: usize,
	pub detail: String,
	pub cfgs: Vec<String>,
	pub active: bool,
	pub test_only: bool,
	pub provenance: Provenance,
	// A macro template: the kinds its body can emit.
	pub emits: Vec<Kind>,
}

// What a whole scan of one crate in one row produced.
#[derive(Default, Debug)]
pub struct CrateScan {
	pub found: Vec<Found>,
	// `macro_rules!` names defined in this crate that carry a boundary in their template.
	pub templates: Vec<String>,
	// Invocations of macros by name, with the item they occur in - so a template's expansion
	// count can be reported beside the template.
	pub invocations: Vec<(String, String, String, usize)>,
	pub files: Vec<String>,
	pub problems: Vec<String>,
}

pub struct Scanner<'a> {
	pub repo: &'a Path,
	pub crate_name: String,
	pub cfg: CfgSet,
	// Where `OUT_DIR` files for this crate are found in this row, if a build has happened.
	pub out_dir: Option<PathBuf>,
	pub scan: CrateScan,
}

impl Scanner<'_> {
	pub fn scan_root(&mut self, root: &Path) {
		let file = match std::fs::read_to_string(root) {
			Ok(text) => text,
			Err(error) => {
				self.scan.problems.push(format!("cannot read {}: {error}", root.display()));
				return;
			}
		};
		let parsed = match syn::parse_file(&file) {
			Ok(parsed) => parsed,
			Err(error) => {
				self.scan.problems.push(format!("cannot parse {}: {error}", root.display()));
				return;
			}
		};
		let rel = self.relative(root);
		self.scan.files.push(rel.clone());
		let crate_attrs = attr_facts(&parsed.attrs, &self.cfg);
		let mut walk = Walk { scanner: self, file: rel, dir: root.parent().map(Path::to_path_buf).unwrap_or_default(), module: vec!["crate".to_string()], cfgs: crate_attrs.cfgs, active: crate_attrs.active, test_only: crate_attrs.test_only, item: String::from("crate"), ordinals: BTreeMap::new(), provenance: Provenance::Source, is_mod_rs: true };
		walk.items(&parsed.items);
	}

	fn relative(&self, path: &Path) -> String {
		path.strip_prefix(self.repo).unwrap_or(path).to_string_lossy().replace('\\', "/")
	}
}

struct Walk<'s, 'a> {
	scanner: &'s mut Scanner<'a>,
	file: String,
	dir: PathBuf,
	module: Vec<String>,
	cfgs: Vec<String>,
	active: bool,
	test_only: bool,
	item: String,
	ordinals: BTreeMap<(String, Kind), usize>,
	provenance: Provenance,
	// Whether this file is a `mod.rs`/root, which decides where `mod x;` looks for `x.rs`.
	is_mod_rs: bool,
}

impl Walk<'_, '_> {
	fn record(&mut self, kind: Kind, span: Span, detail: String, emits: Vec<Kind>) {
		let key = (self.item.clone(), kind);
		let ordinal = self.ordinals.entry(key).or_insert(0);
		*ordinal += 1;
		let found = Found { crate_name: self.scanner.crate_name.clone(), file: self.file.clone(), line: span.start().line, kind, item: self.item.clone(), ordinal: *ordinal, detail, cfgs: self.cfgs.clone(), active: self.active, test_only: self.test_only, provenance: self.provenance.clone(), emits };
		self.scanner.scan.found.push(found);
	}

	fn with_item<F: FnOnce(&mut Self)>(&mut self, facts: &AttrFacts, name: &str, f: F) {
		let saved = (self.cfgs.clone(), self.active, self.test_only, self.item.clone());
		self.cfgs.extend(facts.cfgs.iter().cloned());
		self.active = self.active && facts.active;
		self.test_only = self.test_only || facts.test_only;
		self.item = format!("{}::{}", self.module.join("::"), name);
		f(self);
		self.cfgs = saved.0;
		self.active = saved.1;
		self.test_only = saved.2;
		self.item = saved.3;
	}

	fn items(&mut self, items: &[Item]) {
		for item in items {
			self.item_(item);
		}
	}

	fn item_(&mut self, item: &Item) {
		match item {
			Item::Fn(f) => {
				let facts = attr_facts(&f.attrs, &self.scanner.cfg);
				let name = f.sig.ident.to_string();
				self.with_item(&facts, &name, |w| {
					for attr in &facts.linkage {
						w.record(Kind::LinkageAttr, f.sig.ident.span(), attr.clone(), vec![]);
					}
					if f.sig.unsafety.is_some() {
						w.record(Kind::UnsafeFn, f.sig.ident.span(), String::from("unsafe fn"), vec![]);
					}
					if let Some(abi) = &f.sig.abi {
						let name = abi.name.as_ref().map(|n| n.value()).unwrap_or_else(|| String::from("C"));
						w.record(Kind::ExternAbiFn, f.sig.ident.span(), format!("extern \"{name}\" fn"), vec![]);
					}
					w.signature_pointers(&f.sig);
					w.block(&f.block);
				});
			}
			Item::Mod(m) => {
				let facts = attr_facts(&m.attrs, &self.scanner.cfg);
				let name = m.ident.to_string();
				match &m.content {
					Some((_, items)) => {
						let saved = (self.cfgs.clone(), self.active, self.test_only, self.item.clone());
						self.cfgs.extend(facts.cfgs.iter().cloned());
						self.active = self.active && facts.active;
						self.test_only = self.test_only || facts.test_only;
						self.module.push(name);
						self.item = self.module.join("::");
						self.items(items);
						self.module.pop();
						self.cfgs = saved.0;
						self.active = saved.1;
						self.test_only = saved.2;
						self.item = saved.3;
					}
					None => self.module_file(&name, &facts),
				}
			}
			Item::Impl(i) => {
				let facts = attr_facts(&i.attrs, &self.scanner.cfg);
				if facts.derived {
					return;
				}
				let target = type_name(&i.self_ty);
				let trait_name = i.trait_.as_ref().map(|(_, path, _)| path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default());
				let name = match &trait_name {
					Some(t) => format!("<{target} as {t}>"),
					None => target.clone(),
				};
				self.with_item(&facts, &name, |w| {
					if i.unsafety.is_some() {
						w.record(Kind::UnsafeImpl, i.impl_token.span(), format!("unsafe impl {} for {target}", trait_name.clone().unwrap_or_default()), vec![]);
					}
					for inner in &i.items {
						if let ImplItem::Fn(f) = inner {
							let inner_facts = attr_facts(&f.attrs, &w.scanner.cfg);
							let fname = f.sig.ident.to_string();
							let prefix = w.item.clone();
							w.with_item(&inner_facts, &format!("{}::{fname}", prefix.rsplit("::").next().unwrap_or("")), |w| {
								w.item = format!("{prefix}::{fname}");
								for attr in &inner_facts.linkage {
									w.record(Kind::LinkageAttr, f.sig.ident.span(), attr.clone(), vec![]);
								}
								if f.sig.unsafety.is_some() {
									w.record(Kind::UnsafeFn, f.sig.ident.span(), String::from("unsafe fn"), vec![]);
								}
								if let Some(abi) = &f.sig.abi {
									let aname = abi.name.as_ref().map(|n| n.value()).unwrap_or_else(|| String::from("C"));
									w.record(Kind::ExternAbiFn, f.sig.ident.span(), format!("extern \"{aname}\" fn"), vec![]);
								}
								w.signature_pointers(&f.sig);
								w.block(&f.block);
							});
						}
					}
				});
			}
			Item::Trait(t) => {
				let facts = attr_facts(&t.attrs, &self.scanner.cfg);
				let name = t.ident.to_string();
				self.with_item(&facts, &name, |w| {
					if t.unsafety.is_some() {
						w.record(Kind::UnsafeTrait, t.ident.span(), String::from("unsafe trait"), vec![]);
					}
					for inner in &t.items {
						if let syn::TraitItem::Fn(f) = inner {
							let inner_facts = attr_facts(&f.attrs, &w.scanner.cfg);
							let fname = f.sig.ident.to_string();
							let prefix = w.item.clone();
							w.with_item(&inner_facts, &fname, |w| {
								w.item = format!("{prefix}::{fname}");
								if f.sig.unsafety.is_some() {
									w.record(Kind::UnsafeFn, f.sig.ident.span(), String::from("unsafe fn (trait)"), vec![]);
								}
								w.signature_pointers(&f.sig);
								if let Some(block) = &f.default {
									w.block(block);
								}
							});
						}
					}
				});
			}
			Item::Static(s) => {
				let facts = attr_facts(&s.attrs, &self.scanner.cfg);
				let name = s.ident.to_string();
				self.with_item(&facts, &name, |w| {
					for attr in &facts.linkage {
						w.record(Kind::LinkageAttr, s.ident.span(), attr.clone(), vec![]);
					}
					if matches!(s.mutability, syn::StaticMutability::Mut(_)) {
						w.record(Kind::StaticMut, s.ident.span(), format!("static mut {name}: {}", type_name(&s.ty)), vec![]);
					}
					w.type_pointers(&s.ty, "static");
					w.expr(&s.expr);
				});
			}
			Item::Const(c) => {
				let facts = attr_facts(&c.attrs, &self.scanner.cfg);
				let name = c.ident.to_string();
				self.with_item(&facts, &name, |w| {
					w.type_pointers(&c.ty, "const");
					w.expr(&c.expr);
				});
			}
			Item::Struct(s) => {
				let facts = attr_facts(&s.attrs, &self.scanner.cfg);
				let name = s.ident.to_string();
				self.with_item(&facts, &name, |w| {
					for field in s.fields.iter() {
						let ffacts = attr_facts(&field.attrs, &w.scanner.cfg);
						if !ffacts.active {
							continue;
						}
						let fname = field.ident.as_ref().map(|i| i.to_string()).unwrap_or_else(|| String::from("_"));
						w.type_pointers(&field.ty, &format!("field {fname}"));
					}
				});
			}
			Item::Union(u) => {
				let facts = attr_facts(&u.attrs, &self.scanner.cfg);
				let name = u.ident.to_string();
				self.with_item(&facts, &name, |w| {
					for field in u.fields.named.iter() {
						let fname = field.ident.as_ref().map(|i| i.to_string()).unwrap_or_else(|| String::from("_"));
						w.type_pointers(&field.ty, &format!("field {fname}"));
					}
				});
			}
			Item::Enum(e) => {
				let facts = attr_facts(&e.attrs, &self.scanner.cfg);
				let name = e.ident.to_string();
				self.with_item(&facts, &name, |w| {
					for variant in &e.variants {
						for field in variant.fields.iter() {
							w.type_pointers(&field.ty, &format!("variant {}", variant.ident));
						}
					}
				});
			}
			Item::ForeignMod(fm) => {
				let facts = attr_facts(&fm.attrs, &self.scanner.cfg);
				let abi = fm.abi.name.as_ref().map(|n| n.value()).unwrap_or_else(|| String::from("C"));
				let declared: Vec<String> = fm
					.items
					.iter()
					.filter_map(|i| match i {
						syn::ForeignItem::Fn(f) => Some(format!("fn {}", f.sig.ident)),
						syn::ForeignItem::Static(s) => Some(format!("static {}", s.ident)),
						syn::ForeignItem::Type(t) => Some(format!("type {}", t.ident)),
						_ => None,
					})
					.collect();
				let name = format!("extern[{}]", declared.first().cloned().unwrap_or_default());
				self.with_item(&facts, &name, |w| {
					w.record(Kind::ExternBlock, fm.abi.extern_token.span(), format!("extern \"{abi}\" {{ {} }}", declared.join(", ")), vec![]);
				});
			}
			Item::Macro(m) => {
				let facts = attr_facts(&m.attrs, &self.scanner.cfg);
				let path = m.mac.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
				if path == "macro_rules" {
					let name = m.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
					let emits = template_kinds(&m.mac.tokens);
					if !emits.is_empty() {
						self.with_item(&facts, &format!("macro_rules! {name}"), |w| {
							w.record(Kind::MacroTemplate, m.mac.path.span(), format!("macro_rules! {name} emits {}", emits.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")), emits.clone());
						});
						self.scanner.scan.templates.push(name);
					}
				} else {
					let saved = self.item.clone();
					self.item = self.module.join("::");
					let saved_cfg = (self.cfgs.clone(), self.active, self.test_only);
					self.cfgs.extend(facts.cfgs.iter().cloned());
					self.active = self.active && facts.active;
					self.test_only = self.test_only || facts.test_only;
					self.macro_invocation(&m.mac);
					self.cfgs = saved_cfg.0;
					self.active = saved_cfg.1;
					self.test_only = saved_cfg.2;
					self.item = saved;
				}
			}
			Item::Type(t) => {
				let facts = attr_facts(&t.attrs, &self.scanner.cfg);
				let name = t.ident.to_string();
				self.with_item(&facts, &name, |w| w.type_pointers(&t.ty, "type alias"));
			}
			_ => {}
		}
	}

	fn module_file(&mut self, name: &str, facts: &AttrFacts) {
		let candidates: Vec<PathBuf> = match &facts.path {
			Some(explicit) => vec![self.dir.join(explicit)],
			None => {
				let base = if self.is_mod_rs {
					self.dir.clone()
				} else {
					// `foo.rs` declaring `mod bar;` looks in `foo/bar.rs` or `foo/bar/mod.rs`.
					let stem = Path::new(&self.file).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
					self.dir.join(stem)
				};
				vec![base.join(format!("{name}.rs")), base.join(name).join("mod.rs")]
			}
		};
		let Some(path) = candidates.iter().find(|p| p.is_file()) else {
			if self.active {
				self.scanner.scan.problems.push(format!("{}: `mod {name};` names no file ({})", self.file, candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" or ")));
			}
			return;
		};
		let text = match std::fs::read_to_string(path) {
			Ok(text) => text,
			Err(error) => {
				self.scanner.scan.problems.push(format!("cannot read {}: {error}", path.display()));
				return;
			}
		};
		let parsed = match syn::parse_file(&text) {
			Ok(parsed) => parsed,
			Err(error) => {
				self.scanner.scan.problems.push(format!("cannot parse {}: {error}", path.display()));
				return;
			}
		};
		let rel = self.scanner.relative(path);
		self.scanner.scan.files.push(rel.clone());
		let inner = attr_facts(&parsed.attrs, &self.scanner.cfg);
		let is_mod_rs = path.file_name().map(|f| f == "mod.rs").unwrap_or(false);
		let mut module = self.module.clone();
		module.push(name.to_string());
		let mut cfgs = self.cfgs.clone();
		cfgs.extend(facts.cfgs.iter().cloned());
		cfgs.extend(inner.cfgs.iter().cloned());
		let mut child = Walk { scanner: self.scanner, file: rel, dir: path.parent().map(Path::to_path_buf).unwrap_or_default(), module: module.clone(), cfgs, active: self.active && facts.active && inner.active, test_only: self.test_only || facts.test_only || inner.test_only, item: module.join("::"), ordinals: BTreeMap::new(), provenance: self.provenance.clone(), is_mod_rs };
		child.items(&parsed.items);
	}

	fn signature_pointers(&mut self, sig: &syn::Signature) {
		for input in &sig.inputs {
			if let syn::FnArg::Typed(pat) = input {
				let pname = match &*pat.pat {
					syn::Pat::Ident(i) => i.ident.to_string(),
					_ => String::from("_"),
				};
				self.type_pointers(&pat.ty, &format!("parameter {pname}"));
			}
		}
		if let syn::ReturnType::Type(_, ty) = &sig.output {
			self.type_pointers(ty, "return");
		}
	}

	fn type_pointers(&mut self, ty: &Type, what: &str) {
		let mut pointers = Vec::new();
		collect_pointers(ty, &mut pointers);
		for (mutability, inner) in pointers {
			self.record(Kind::RawPointer, ty.span(), format!("{what}: *{mutability} {inner}"), vec![]);
		}
	}

	fn block(&mut self, block: &syn::Block) {
		let mut visitor = ExprVisitor { walk: self };
		visitor.visit_block(block);
	}

	fn expr(&mut self, expr: &Expr) {
		let mut visitor = ExprVisitor { walk: self };
		visitor.visit_expr(expr);
	}

	fn macro_invocation(&mut self, mac: &syn::Macro) {
		let name = mac.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
		match name.as_str() {
			"asm" => self.record(Kind::InlineAsm, mac.path.span(), String::from("asm!"), vec![]),
			"global_asm" => self.record(Kind::GlobalAsm, mac.path.span(), String::from("global_asm!"), vec![]),
			"naked_asm" => self.record(Kind::NakedAsm, mac.path.span(), String::from("naked_asm!"), vec![]),
			"include" => self.include(mac),
			_ => {
				let line = mac.path.span().start().line;
				self.scanner.scan.invocations.push((name, self.item.clone(), self.file.clone(), line));
			}
		}
	}

	// `include!(concat!(env!("OUT_DIR"), "/name.rs"))`: the generated file, scanned as this item's
	// content with its provenance recorded, when a build has produced it.
	fn include(&mut self, mac: &syn::Macro) {
		let text = mac.tokens.to_string();
		// `concat ! (env ! ("OUT_DIR") , "/name.rs")`: the first quote after OUT_DIR closes its own
		// string; the file name is the next quoted string.
		let Some(rest) = text.split("OUT_DIR").nth(1) else { return };
		let rest = rest.trim_start().trim_start_matches('"');
		let Some(start) = rest.find('"') else { return };
		let tail = &rest[start + 1..];
		let Some(end) = tail.find('"') else { return };
		let name = tail[..end].trim_start_matches('/').to_string();
		let include_site = format!("{}:{}", self.file, mac.path.span().start().line);
		let Some(out_dir) = self.scanner.out_dir.clone() else {
			self.scanner.scan.problems.push(format!("{include_site}: includes `{name}` from OUT_DIR and this row has no built OUT_DIR to read it from"));
			return;
		};
		let path = out_dir.join(&name);
		let text = match std::fs::read_to_string(&path) {
			Ok(text) => text,
			Err(error) => {
				self.scanner.scan.problems.push(format!("{include_site}: generated file {} cannot be read: {error}", path.display()));
				return;
			}
		};
		// A generated file may be a bare expression or a list of items; try items first.
		let items: Vec<Item> = match syn::parse_file(&text) {
			Ok(parsed) => parsed.items,
			Err(_) => match syn::parse_str::<Expr>(&text) {
				Ok(expr) => {
					let saved = self.provenance.clone();
					self.provenance = Provenance::Generated { generator: format!("{}/build.rs", self.scanner.crate_name), included_from: include_site.clone() };
					let saved_file = std::mem::replace(&mut self.file, self.scanner.relative(&path));
					self.expr(&expr);
					self.file = saved_file;
					self.provenance = saved;
					return;
				}
				Err(error) => {
					self.scanner.scan.problems.push(format!("{include_site}: generated file {} does not parse: {error}", path.display()));
					return;
				}
			},
		};
		let saved = self.provenance.clone();
		self.provenance = Provenance::Generated { generator: format!("{}/build.rs", self.scanner.crate_name), included_from: include_site };
		let saved_file = std::mem::replace(&mut self.file, self.scanner.relative(&path));
		let saved_dir = std::mem::replace(&mut self.dir, out_dir);
		self.scanner.scan.files.push(self.file.clone());
		self.items(&items);
		self.dir = saved_dir;
		self.file = saved_file;
		self.provenance = saved;
	}
}

struct ExprVisitor<'w, 's, 'a> {
	walk: &'w mut Walk<'s, 'a>,
}

impl<'ast> Visit<'ast> for ExprVisitor<'_, '_, '_> {
	fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
		let facts = attr_facts(&node.attrs, &self.walk.scanner.cfg);
		let saved = (self.walk.cfgs.clone(), self.walk.active, self.walk.test_only);
		self.walk.cfgs.extend(facts.cfgs.iter().cloned());
		self.walk.active = self.walk.active && facts.active;
		self.walk.test_only = self.walk.test_only || facts.test_only;
		self.walk.record(Kind::UnsafeBlock, node.unsafe_token.span(), String::from("unsafe block"), vec![]);
		syn::visit::visit_expr_unsafe(self, node);
		self.walk.cfgs = saved.0;
		self.walk.active = saved.1;
		self.walk.test_only = saved.2;
	}

	fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
		self.walk.macro_invocation(&node.mac);
		syn::visit::visit_expr_macro(self, node);
	}

	fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
		self.walk.macro_invocation(&node.mac);
		syn::visit::visit_stmt_macro(self, node);
	}

	fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
		// A statement behind a false `cfg` is not reachable in this row.
		if let syn::Stmt::Local(local) = node {
			let facts = attr_facts(&local.attrs, &self.walk.scanner.cfg);
			if !facts.active {
				return;
			}
		}
		if let syn::Stmt::Expr(expr, _) = node {
			let attrs = expr_attrs(expr);
			let facts = attr_facts(attrs, &self.walk.scanner.cfg);
			if !facts.active {
				return;
			}
		}
		if let syn::Stmt::Item(item) = node {
			// A nested item (an inner fn, a static) is walked as an item, with its own name.
			self.walk.item_(item);
			return;
		}
		syn::visit::visit_stmt(self, node);
	}

	fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
		syn::visit::visit_expr_closure(self, node);
	}

	fn visit_type_ptr(&mut self, node: &'ast syn::TypePtr) {
		// A raw pointer written inside a body - a cast target, a local's type - is a site in the
		// enclosing item.
		let mutability = if node.mutability.is_some() { "mut" } else { "const" };
		self.walk.record(Kind::RawPointer, node.star_token.span(), format!("in body: *{mutability} {}", type_name(&node.elem)), vec![]);
		syn::visit::visit_type_ptr(self, node);
	}
}

fn expr_attrs(expr: &Expr) -> &[Attribute] {
	match expr {
		Expr::Array(e) => &e.attrs,
		Expr::Assign(e) => &e.attrs,
		Expr::Async(e) => &e.attrs,
		Expr::Await(e) => &e.attrs,
		Expr::Binary(e) => &e.attrs,
		Expr::Block(e) => &e.attrs,
		Expr::Break(e) => &e.attrs,
		Expr::Call(e) => &e.attrs,
		Expr::Cast(e) => &e.attrs,
		Expr::Closure(e) => &e.attrs,
		Expr::Const(e) => &e.attrs,
		Expr::Continue(e) => &e.attrs,
		Expr::Field(e) => &e.attrs,
		Expr::ForLoop(e) => &e.attrs,
		Expr::Group(e) => &e.attrs,
		Expr::If(e) => &e.attrs,
		Expr::Index(e) => &e.attrs,
		Expr::Infer(e) => &e.attrs,
		Expr::Let(e) => &e.attrs,
		Expr::Lit(e) => &e.attrs,
		Expr::Loop(e) => &e.attrs,
		Expr::Macro(e) => &e.attrs,
		Expr::Match(e) => &e.attrs,
		Expr::MethodCall(e) => &e.attrs,
		Expr::Paren(e) => &e.attrs,
		Expr::Path(e) => &e.attrs,
		Expr::Range(e) => &e.attrs,
		Expr::RawAddr(e) => &e.attrs,
		Expr::Reference(e) => &e.attrs,
		Expr::Repeat(e) => &e.attrs,
		Expr::Return(e) => &e.attrs,
		Expr::Struct(e) => &e.attrs,
		Expr::Try(e) => &e.attrs,
		Expr::TryBlock(e) => &e.attrs,
		Expr::Tuple(e) => &e.attrs,
		Expr::Unary(e) => &e.attrs,
		Expr::Unsafe(e) => &e.attrs,
		Expr::While(e) => &e.attrs,
		Expr::Yield(e) => &e.attrs,
		_ => &[],
	}
}

fn collect_pointers(ty: &Type, out: &mut Vec<(&'static str, String)>) {
	match ty {
		Type::Ptr(p) => {
			out.push((if p.mutability.is_some() { "mut" } else { "const" }, type_name(&p.elem)));
			collect_pointers(&p.elem, out);
		}
		Type::Reference(r) => collect_pointers(&r.elem, out),
		Type::Array(a) => collect_pointers(&a.elem, out),
		Type::Slice(s) => collect_pointers(&s.elem, out),
		Type::Tuple(t) => {
			for elem in &t.elems {
				collect_pointers(elem, out);
			}
		}
		Type::Paren(p) => collect_pointers(&p.elem, out),
		Type::Group(g) => collect_pointers(&g.elem, out),
		Type::Path(p) => {
			for segment in &p.path.segments {
				if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
					for arg in &args.args {
						if let syn::GenericArgument::Type(inner) = arg {
							collect_pointers(inner, out);
						}
					}
				}
			}
		}
		Type::BareFn(f) => {
			for input in &f.inputs {
				collect_pointers(&input.ty, out);
			}
			if let syn::ReturnType::Type(_, ret) = &f.output {
				collect_pointers(ret, out);
			}
		}
		_ => {}
	}
}

pub fn type_name(ty: &Type) -> String {
	use quote_to_string::ToTokensString;
	let text = ty.to_token_string();
	// Tokens print with spaces; the ordinary spelling is closer to the source.
	text.replace(" :: ", "::").replace(" < ", "<").replace(" > ", ">").replace("& ", "&").replace(" ,", ",")
}

// The kinds a `macro_rules!` template can emit, from its token stream.
fn template_kinds(tokens: &proc_macro2::TokenStream) -> Vec<Kind> {
	let mut kinds = Vec::new();
	let mut previous: Option<String> = None;
	let mut stack: Vec<proc_macro2::TokenStream> = vec![tokens.clone()];
	while let Some(stream) = stack.pop() {
		for token in stream {
			match token {
				proc_macro2::TokenTree::Group(group) => stack.push(group.stream()),
				proc_macro2::TokenTree::Ident(ident) => {
					let text = ident.to_string();
					match text.as_str() {
						"unsafe" => push_unique(&mut kinds, Kind::UnsafeBlock),
						"asm" => push_unique(&mut kinds, Kind::InlineAsm),
						"global_asm" => push_unique(&mut kinds, Kind::GlobalAsm),
						"naked_asm" => push_unique(&mut kinds, Kind::NakedAsm),
						"no_mangle" | "export_name" | "link_section" => push_unique(&mut kinds, Kind::LinkageAttr),
						// `extern "C" fn` / `extern "x86-interrupt" fn` in a template: the interrupt
						// handler and entry-point stubs a kernel generates by the hundred.
						"extern" => push_unique(&mut kinds, Kind::ExternAbiFn),
						"mut" if previous.as_deref() == Some("static") => push_unique(&mut kinds, Kind::StaticMut),
						_ => {}
					}
					previous = Some(text);
				}
				proc_macro2::TokenTree::Punct(p) => {
					if p.as_char() == '*' {
						// `*const` / `*mut` is read at the next ident.
					}
					previous = Some(p.to_string());
				}
				_ => {}
			}
		}
	}
	kinds
}

fn push_unique(kinds: &mut Vec<Kind>, kind: Kind) {
	if !kinds.contains(&kind) {
		kinds.push(kind);
	}
}
