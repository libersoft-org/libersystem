//! Pure parsing and expansion for the shell line language.

use alloc::string::String;
use alloc::vec::Vec;

pub fn trim(mut value: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = value {
		if first.is_ascii_whitespace() {
			value = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = value {
		if last.is_ascii_whitespace() {
			value = rest;
		} else {
			break;
		}
	}
	value
}

pub fn normalize_flags(line: &[u8]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(line.len());
	for (index, token) in line.split(|&byte: &u8| byte == b' ').enumerate() {
		if index > 0 {
			out.push(b' ');
		}
		match token {
			b"--json" => out.extend_from_slice(b"json"),
			b"--json-min" => out.extend_from_slice(b"json-min"),
			b"--cbor" => out.extend_from_slice(b"cbor"),
			_ => out.extend_from_slice(token),
		}
	}
	out
}

pub fn expand_vars(line: &[u8], vars: &[(String, String)]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(line.len());
	let mut index: usize = 0;
	while index < line.len() {
		if line[index] != b'$' {
			out.push(line[index]);
			index += 1;
			continue;
		}
		if index + 1 < line.len() && line[index + 1] == b'{' {
			let start: usize = index + 2;
			match line[start..].iter().position(|&byte: &u8| byte == b'}') {
				Some(relative) => {
					push_var_value(&mut out, &line[start..start + relative], vars);
					index = start + relative + 1;
				}
				None => {
					out.push(b'$');
					index += 1;
				}
			}
			continue;
		}
		let start: usize = index + 1;
		if start < line.len() && (line[start].is_ascii_alphabetic() || line[start] == b'_') {
			let mut end: usize = start + 1;
			while end < line.len() && (line[end].is_ascii_alphanumeric() || line[end] == b'_') {
				end += 1;
			}
			push_var_value(&mut out, &line[start..end], vars);
			index = end;
		} else {
			out.push(b'$');
			index += 1;
		}
	}
	out
}

fn push_var_value(out: &mut Vec<u8>, name: &[u8], vars: &[(String, String)]) {
	if let Some((_, value)) = vars.iter().find(|(candidate, _): &&(String, String)| candidate.as_bytes() == name) {
		out.extend_from_slice(value.as_bytes());
	}
}

pub fn parse_assignment(line: &[u8]) -> Option<(&str, &[u8])> {
	let equals: usize = line.iter().position(|&byte: &u8| byte == b'=')?;
	let name: &[u8] = &line[..equals];
	if name.is_empty() {
		return None;
	}
	let head: u8 = name[0];
	if !(head.is_ascii_alphabetic() || head == b'_') {
		return None;
	}
	if !name.iter().all(|&byte: &u8| byte.is_ascii_alphanumeric() || byte == b'_') {
		return None;
	}
	Some((core::str::from_utf8(name).ok()?, &line[equals + 1..]))
}

pub fn parse_and_expand(raw: &[u8], vars: &[(String, String)]) -> Vec<u8> {
	let expanded: Vec<u8> = expand_vars(trim(raw), vars);
	normalize_flags(&expanded)
}

#[cfg(test)]
#[path = "shell_language/tests.rs"]
mod tests;
