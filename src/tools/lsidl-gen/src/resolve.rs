//! Whole-compilation-unit package registry and import resolution.

use crate::ast::*;
use crate::token::Error;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
	Value,
	Resource,
	Interface,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleCardinality {
	Zero,
	One,
	Many,
}

impl HandleCardinality {
	fn sum(self, other: HandleCardinality) -> HandleCardinality {
		use HandleCardinality::*;
		match (self, other) {
			(Many, _) | (_, Many) | (One, One) => Many,
			(One, Zero) | (Zero, One) => One,
			(Zero, Zero) => Zero,
		}
	}

	fn max(self, other: HandleCardinality) -> HandleCardinality {
		use HandleCardinality::*;
		match (self, other) {
			(Many, _) | (_, Many) => Many,
			(One, _) | (_, One) => One,
			(Zero, Zero) => Zero,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageId {
	pub path: Vec<String>,
	pub version: u32,
}

impl PackageId {
	pub fn display(&self) -> String {
		format!("{}@{}", self.path.join(":"), self.version)
	}

	pub fn rust_module(&self) -> String {
		let mut parts = self.rust_components();
		parts.push(format!("v{}", self.version));
		parts.join("::")
	}

	pub fn rust_components(&self) -> Vec<String> {
		self.path.iter().map(|part| rust_ident(part)).collect()
	}

	pub fn file_components(&self) -> Vec<String> {
		self.rust_components().into_iter().map(|component| component.strip_prefix("r#").unwrap_or(&component).to_string()).collect()
	}
}

#[derive(Clone, Debug)]
pub struct ResolvedSymbol {
	pub package: PackageId,
	pub source_name: String,
	pub kind: SymbolKind,
	pub cardinality: HandleCardinality,
	pub contains_again: bool,
	pub wire_type: Option<Type>,
}

#[derive(Clone, Debug)]
pub struct ResolvedPackage {
	pub file: usize,
	pub id: PackageId,
	pub imports: HashMap<String, ResolvedSymbol>,
}

#[derive(Clone, Debug)]
pub struct ResolveError {
	pub file: usize,
	pub error: Error,
}

#[derive(Clone)]
struct Export {
	kind: SymbolKind,
	cardinality: HandleCardinality,
	contains_again: bool,
	wire_type: Option<Type>,
}

pub fn resolve(files: &[File]) -> Result<Vec<ResolvedPackage>, Vec<ResolveError>> {
	let mut errors = Vec::new();
	let mut by_path: HashMap<String, usize> = HashMap::new();
	let ids: Vec<PackageId> = files.iter().map(|file| PackageId { path: file.package.path.clone(), version: file.package.version }).collect();
	for (index, id) in ids.iter().enumerate() {
		let key = id.path.join(":");
		if let Some(first) = by_path.insert(key.clone(), index) {
			errors.push(ResolveError { file: index, error: Error::new(files[index].package.span, format!("package path `{key}` is already loaded as {} (second identity {})", ids[first].display(), id.display())) });
		}
	}

	let mut exports: Vec<HashMap<String, Export>> = files.iter().map(base_exports).collect();
	let mut edges: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
	for (index, file) in files.iter().enumerate() {
		for import in &file.uses {
			let key = import.path.join(":");
			let Some(&target) = by_path.get(&key) else {
				let suggestion = suggest(&key, by_path.keys().map(String::as_str));
				errors.push(ResolveError { file: index, error: Error::new(import.span, format!("missing imported package `{}@{}`{}", key, import.version, suggestion.map(|value| format!("; did you mean `{value}`?")).unwrap_or_default())) });
				continue;
			};
			if target == index {
				errors.push(ResolveError { file: index, error: Error::new(import.span, format!("package `{}` cannot import itself", ids[index].display())) });
				continue;
			}
			if ids[target].version != import.version {
				errors.push(ResolveError { file: index, error: Error::new(import.span, format!("import requests `{key}@{}` but the compilation unit contains `{}`", import.version, ids[target].display())) });
				continue;
			}
			edges[index].push(target);
		}
	}
	if !errors.is_empty() {
		return Err(errors);
	}

	let order = topo_order(&edges, files, &ids)?;
	let mut resolved: Vec<Option<ResolvedPackage>> = vec![None; files.len()];
	for &index in &order {
		let mut imports = HashMap::new();
		for import in &files[index].uses {
			let target = by_path[&import.path.join(":")];
			for name in &import.names {
				let Some(export) = exports[target].get(&name.name) else {
					let suggestion = suggest(&name.name, exports[target].keys().map(String::as_str));
					errors.push(ResolveError { file: index, error: Error::new(name.span, format!("package `{}` does not export `{}`{}", ids[target].display(), name.name, suggestion.map(|value| format!("; did you mean `{value}`?")).unwrap_or_default())) });
					continue;
				};
				let local = name.local_name().to_string();
				if imports.contains_key(&local) {
					errors.push(ResolveError { file: index, error: Error::new(name.alias_span.unwrap_or(name.span), format!("imported name `{local}` is already defined")) });
					continue;
				}
				imports.insert(local, ResolvedSymbol { package: ids[target].clone(), source_name: name.name.clone(), kind: export.kind, cardinality: export.cardinality, contains_again: export.contains_again, wire_type: export.wire_type.clone() });
			}
		}
		if errors.is_empty() {
			exports[index] = resolved_exports(&files[index], &imports);
			resolved[index] = Some(ResolvedPackage { file: index, id: ids[index].clone(), imports });
		}
	}
	if !errors.is_empty() {
		return Err(errors);
	}
	Ok(order.into_iter().map(|index| resolved[index].take().expect("resolved package")).collect())
}

pub(crate) fn suggest<'a>(needle: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
	let limit = ((needle.len() + 2) / 3).clamp(1, 3);
	candidates.map(|candidate| (edit_distance(needle, candidate), candidate)).filter(|(distance, _)| *distance <= limit).min_by_key(|(distance, candidate)| (*distance, *candidate)).map(|(_, candidate)| candidate.to_string())
}

fn edit_distance(left: &str, right: &str) -> usize {
	let mut previous: Vec<usize> = (0..=right.len()).collect();
	for (row, left_byte) in left.bytes().enumerate() {
		let mut current = vec![row + 1; right.len() + 1];
		for (column, right_byte) in right.bytes().enumerate() {
			current[column + 1] = (previous[column + 1] + 1).min(current[column] + 1).min(previous[column] + usize::from(left_byte != right_byte));
		}
		previous = current;
	}
	previous[right.len()]
}

fn base_exports(file: &File) -> HashMap<String, Export> {
	let mut out = HashMap::new();
	for item in &file.items {
		let (name, kind, again, wire_type) = match item {
			Item::Alias(item) => (&item.name, SymbolKind::Value, false, Some(item.ty.clone())),
			Item::Record(item) => (&item.name, SymbolKind::Value, false, None),
			Item::Enum(item) => (&item.name, SymbolKind::Value, item.cases.iter().any(|case| case.name == "again"), None),
			Item::Variant(item) => (&item.name, SymbolKind::Value, false, None),
			Item::Flags(item) => (&item.name, SymbolKind::Value, false, None),
			Item::Resource(item) => (&item.name, SymbolKind::Resource, false, None),
			Item::Interface(item) => (&item.name, SymbolKind::Interface, false, None),
		};
		out.entry(name.clone()).or_insert(Export { kind, cardinality: HandleCardinality::Zero, contains_again: again, wire_type });
	}
	out
}

fn resolved_exports(file: &File, imports: &HashMap<String, ResolvedSymbol>) -> HashMap<String, Export> {
	let mut out = base_exports(file);
	loop {
		let mut changed = false;
		for item in &file.items {
			let (name, next) = match item {
				Item::Alias(alias) => (&alias.name, type_cardinality(&alias.ty, &out, imports)),
				Item::Record(record) => (&record.name, record.fields.iter().fold(HandleCardinality::Zero, |acc, field| acc.sum(type_cardinality(&field.ty, &out, imports)))),
				Item::Variant(variant) => (&variant.name, variant.cases.iter().filter_map(|case| case.payload.as_ref()).fold(HandleCardinality::Zero, |acc, ty| acc.max(type_cardinality(ty, &out, imports)))),
				Item::Enum(item) => (&item.name, HandleCardinality::Zero),
				Item::Flags(item) => (&item.name, HandleCardinality::Zero),
				Item::Resource(_) | Item::Interface(_) => continue,
			};
			let export = out.get_mut(name).expect("local export");
			if export.cardinality != next {
				export.cardinality = next;
				changed = true;
			}
		}
		if !changed {
			break;
		}
	}
	let aliases: Vec<(String, Type)> = file.items.iter().filter_map(|item| if let Item::Alias(alias) = item { Some((alias.name.clone(), alias.ty.clone())) } else { None }).collect();
	for (name, ty) in aliases {
		let expanded = expand_alias_type(&ty, &out, imports, 0);
		out.get_mut(&name).expect("alias export").wire_type = Some(expanded);
	}
	out
}

fn expand_alias_type(ty: &Type, locals: &HashMap<String, Export>, imports: &HashMap<String, ResolvedSymbol>, depth: usize) -> Type {
	if depth >= 64 {
		return ty.clone();
	}
	match ty {
		Type::Named(name) => {
			if let Some(next) = locals.get(name).and_then(|export| export.wire_type.as_ref()) {
				return expand_alias_type(next, locals, imports, depth + 1);
			}
			if let Some(next) = imports.get(name).and_then(|symbol| symbol.wire_type.as_ref()) {
				return expand_alias_type(next, locals, imports, depth + 1);
			}
			ty.clone()
		}
		Type::Option(inner) => Type::Option(Box::new(expand_alias_type(inner, locals, imports, depth + 1))),
		Type::List(inner) => Type::List(Box::new(expand_alias_type(inner, locals, imports, depth + 1))),
		Type::Stream(inner) => Type::Stream(Box::new(expand_alias_type(inner, locals, imports, depth + 1))),
		Type::Tuple(items) => Type::Tuple(items.iter().map(|item| expand_alias_type(item, locals, imports, depth + 1)).collect()),
		Type::Result(ok, err) => Type::Result(Box::new(expand_alias_type(ok, locals, imports, depth + 1)), Box::new(expand_alias_type(err, locals, imports, depth + 1))),
		_ => ty.clone(),
	}
}

fn type_cardinality(ty: &Type, locals: &HashMap<String, Export>, imports: &HashMap<String, ResolvedSymbol>) -> HandleCardinality {
	match ty {
		Type::Handle(_) | Type::Buffer | Type::Stream(_) => HandleCardinality::One,
		Type::Option(inner) => type_cardinality(inner, locals, imports),
		Type::List(inner) => match type_cardinality(inner, locals, imports) {
			HandleCardinality::Zero => HandleCardinality::Zero,
			HandleCardinality::One | HandleCardinality::Many => HandleCardinality::Many,
		},
		Type::Tuple(items) => items.iter().fold(HandleCardinality::Zero, |acc, item| acc.sum(type_cardinality(item, locals, imports))),
		Type::Result(ok, err) => type_cardinality(ok, locals, imports).max(type_cardinality(err, locals, imports)),
		Type::Named(name) => locals.get(name).map(|export| export.cardinality).or_else(|| imports.get(name).map(|symbol| symbol.cardinality)).unwrap_or(HandleCardinality::Zero),
		_ => HandleCardinality::Zero,
	}
}

fn topo_order(edges: &[Vec<usize>], files: &[File], ids: &[PackageId]) -> Result<Vec<usize>, Vec<ResolveError>> {
	fn visit(node: usize, edges: &[Vec<usize>], files: &[File], ids: &[PackageId], state: &mut [u8], stack: &mut Vec<usize>, order: &mut Vec<usize>) -> Result<(), ResolveError> {
		if state[node] == 2 {
			return Ok(());
		}
		if state[node] == 1 {
			let start = stack.iter().position(|entry| *entry == node).unwrap_or(0);
			let mut chain: Vec<String> = stack[start..].iter().map(|index| ids[*index].display()).collect();
			chain.push(ids[node].display());
			return Err(ResolveError { file: node, error: Error::new(files[node].package.span, format!("package import cycle: {}", chain.join(" -> "))) });
		}
		state[node] = 1;
		stack.push(node);
		for &dependency in &edges[node] {
			visit(dependency, edges, files, ids, state, stack, order)?;
		}
		stack.pop();
		state[node] = 2;
		order.push(node);
		Ok(())
	}

	let mut state = vec![0u8; edges.len()];
	let mut stack = Vec::new();
	let mut order = Vec::new();
	for node in 0..edges.len() {
		if let Err(error) = visit(node, edges, files, ids, &mut state, &mut stack, &mut order) {
			return Err(vec![error]);
		}
	}
	Ok(order)
}

pub fn rust_ident(name: &str) -> String {
	let mut out = name.replace('-', "_");
	const KEYWORDS: &[&str] = &[
		"as",
		"break",
		"const",
		"continue",
		"crate",
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
		"Self",
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
		"dyn",
	];
	if KEYWORDS.contains(&out.as_str()) {
		out = format!("r#{out}");
	}
	out
}

pub fn import_rust_path(symbol: &ResolvedSymbol) -> String {
	format!("crate::generated::{}::{}", symbol.package.rust_module(), camel(&symbol.source_name))
}

fn camel(name: &str) -> String {
	let mut out = String::new();
	for part in name.split('-') {
		let mut chars = part.chars();
		if let Some(first) = chars.next() {
			out.extend(first.to_uppercase());
			out.extend(chars);
		}
	}
	out
}
