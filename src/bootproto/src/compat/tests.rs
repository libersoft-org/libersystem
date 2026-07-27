use super::*;
use crate::elf::{DT_HASH, DT_NEEDED, DT_NULL, DT_STRSZ, DT_STRTAB, DT_SYMENT, DT_SYMTAB, DynamicEntry, ET_DYN, PF_R, PT_DYNAMIC, PT_LOAD, ProgramHeader};
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

// The on-disk constants are written out here rather than imported, so a fixture that stops
// matching the format fails this test instead of quietly following elf.rs wherever it went.
const EM_X86_64: u16 = 62;
const SHT_STRTAB: u32 = 3;
const SHT_NOTE: u32 = 7;
const SHF_ALLOC: u64 = 1 << 1;
const HEADER_LEN: usize = 64;
const PHENT: usize = 56;
const SHENT: usize = 64;
const SYMENT: usize = 24;
const LOAD_ADDRESS: u64 = 0x8000;

// One export, as the fixture describes it before it becomes bytes.
#[derive(Clone, Copy)]
struct Export {
	name: &'static str,
	// binding << 4 | type
	info: u8,
	// the visibility bits
	other: u8,
	// 0 makes the symbol undefined, which is an import rather than an export
	section: u16,
	size: u64,
}

impl Export {
	fn function(name: &'static str) -> Export {
		Export { name, info: 0x12, other: 0, section: 1, size: 16 }
	}
}

fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
	bytes[at..at + value.len()].copy_from_slice(value);
}

// Build a shared library carrying an identity record, a DT_NEEDED list and a dynamic symbol
// table - the three things the rule reads. Everything is laid out at explicit offsets so the
// fixture is readable as the file format it is.
fn library(record: &str, needed: &[&str], exports: &[Export]) -> Vec<u8> {
	// The string table: a leading NUL, then every needed name and symbol name once.
	let mut strings: Vec<u8> = vec![0];
	let mut offset_of = |strings: &mut Vec<u8>, name: &str| -> u32 {
		let at = strings.len() as u32;
		strings.extend_from_slice(name.as_bytes());
		strings.push(0);
		at
	};
	let needed_offsets: Vec<u32> = needed.iter().map(|name| offset_of(&mut strings, name)).collect();
	let export_offsets: Vec<u32> = exports.iter().map(|export| offset_of(&mut strings, export.name)).collect();

	// The symbol table always leads with the reserved null entry.
	let symbol_count = exports.len() + 1;
	let mut symbols: Vec<u8> = vec![0u8; SYMENT];
	for (export, name_offset) in exports.iter().zip(&export_offsets) {
		let mut entry = [0u8; SYMENT];
		put(&mut entry, 0, &name_offset.to_le_bytes());
		entry[4] = export.info;
		entry[5] = export.other;
		put(&mut entry, 6, &export.section.to_le_bytes());
		put(&mut entry, 8, &0x120u64.to_le_bytes());
		put(&mut entry, 16, &export.size.to_le_bytes());
		symbols.extend_from_slice(&entry);
	}

	// A minimal SysV hash table: one bucket, and nchain bounding the symbol table.
	let mut hash: Vec<u8> = Vec::new();
	for word in [1u32, symbol_count as u32, 1] {
		hash.extend_from_slice(&word.to_le_bytes());
	}
	for _ in 0..symbol_count {
		hash.extend_from_slice(&0u32.to_le_bytes());
	}

	let payload_at = HEADER_LEN + 2 * PHENT;
	let symbols_at = strings.len();
	let hash_at = symbols_at + symbols.len();
	let dynamic_at = hash_at + hash.len();

	let mut dynamic: Vec<DynamicEntry> = needed_offsets.iter().map(|at| DynamicEntry { tag: DT_NEEDED, value: u64::from(*at) }).collect();
	dynamic.push(DynamicEntry { tag: DT_STRTAB, value: LOAD_ADDRESS });
	dynamic.push(DynamicEntry { tag: DT_STRSZ, value: strings.len() as u64 });
	dynamic.push(DynamicEntry { tag: DT_SYMTAB, value: LOAD_ADDRESS + symbols_at as u64 });
	dynamic.push(DynamicEntry { tag: DT_SYMENT, value: SYMENT as u64 });
	dynamic.push(DynamicEntry { tag: DT_HASH, value: LOAD_ADDRESS + hash_at as u64 });
	dynamic.push(DynamicEntry { tag: DT_NULL, value: 0 });
	let dynamic_bytes = unsafe { core::slice::from_raw_parts(dynamic.as_ptr() as *const u8, core::mem::size_of_val(dynamic.as_slice())) };

	let mut payload: Vec<u8> = Vec::new();
	payload.extend_from_slice(&strings);
	payload.extend_from_slice(&symbols);
	payload.extend_from_slice(&hash);
	payload.extend_from_slice(dynamic_bytes);

	let shstrtab: &[u8] = b"\0.shstrtab\0.note.liber.identity\0";
	let shstrtab_at = payload_at + payload.len();
	let note_at = shstrtab_at + shstrtab.len();
	let note_len = 20 + ((record.len() + 3) & !3);
	let sections_at = note_at + note_len;

	let mut bytes = vec![0u8; sections_at + 3 * SHENT];

	// ELF header.
	put(&mut bytes, 0, &[0x7f, b'E', b'L', b'F', 2, 1, 1]);
	put(&mut bytes, 16, &ET_DYN.to_le_bytes());
	put(&mut bytes, 18, &EM_X86_64.to_le_bytes());
	put(&mut bytes, 20, &1u32.to_le_bytes());
	put(&mut bytes, 32, &(HEADER_LEN as u64).to_le_bytes());
	put(&mut bytes, 40, &(sections_at as u64).to_le_bytes());
	put(&mut bytes, 52, &(HEADER_LEN as u16).to_le_bytes());
	put(&mut bytes, 54, &(PHENT as u16).to_le_bytes());
	put(&mut bytes, 56, &2u16.to_le_bytes());
	put(&mut bytes, 58, &(SHENT as u16).to_le_bytes());
	put(&mut bytes, 60, &3u16.to_le_bytes());
	put(&mut bytes, 62, &1u16.to_le_bytes());

	// Program headers: the loaded payload, and the dynamic table inside it.
	let segments = [
		ProgramHeader { p_type: PT_LOAD, p_flags: PF_R, p_offset: payload_at as u64, p_vaddr: LOAD_ADDRESS, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 1 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: (payload_at + dynamic_at) as u64, p_vaddr: LOAD_ADDRESS + dynamic_at as u64, p_paddr: 0, p_filesz: dynamic_bytes.len() as u64, p_memsz: dynamic_bytes.len() as u64, p_align: 8 },
	];
	unsafe {
		core::ptr::copy_nonoverlapping(segments.as_ptr() as *const u8, bytes.as_mut_ptr().add(HEADER_LEN), 2 * PHENT);
	}

	put(&mut bytes, payload_at, &payload);
	put(&mut bytes, shstrtab_at, shstrtab);

	// The identity note: name length, descriptor length, type, "LIBER\0", then the record.
	put(&mut bytes, note_at, &6u32.to_le_bytes());
	put(&mut bytes, note_at + 4, &(record.len() as u32).to_le_bytes());
	put(&mut bytes, note_at + 8, &1u32.to_le_bytes());
	put(&mut bytes, note_at + 12, b"LIBER\0");
	put(&mut bytes, note_at + 20, record.as_bytes());

	// Section headers: null, .shstrtab, .note.liber.identity.
	let mut section = |index: usize, name: u32, kind: u32, flags: u64, offset: u64, size: u64| {
		let at = sections_at + index * SHENT;
		put(&mut bytes, at, &name.to_le_bytes());
		put(&mut bytes, at + 4, &kind.to_le_bytes());
		put(&mut bytes, at + 8, &flags.to_le_bytes());
		put(&mut bytes, at + 24, &offset.to_le_bytes());
		put(&mut bytes, at + 32, &size.to_le_bytes());
	};
	section(1, 1, SHT_STRTAB, 0, shstrtab_at as u64, shstrtab.len() as u64);
	section(2, 11, SHT_NOTE, SHF_ALLOC, note_at as u64, note_len as u64);
	bytes
}

// The identity record a real `.lslib` carries, with the fields this rule reads.
fn record(providers: &[&str], overrides: &[(&str, &str)]) -> String {
	let mut fields: Vec<(String, String)> = vec![
		("format".to_string(), IDENTITY_FORMAT.to_string()),
		("kind".to_string(), "library".to_string()),
		("artifact".to_string(), "png".to_string()),
		("package".to_string(), "png".to_string()),
		("source-sha256".to_string(), "a".repeat(64)),
		("rustc-commit".to_string(), "01f6ddf7588f42ae2d7eb0a2f21d44e8e96674cf".to_string()),
		("target".to_string(), "x86_64-unknown-none".to_string()),
		("profile".to_string(), "release".to_string()),
		("rustflags".to_string(), "-C relocation-model=pic".to_string()),
		("features".to_string(), "-".to_string()),
	];
	for (key, value) in overrides {
		for field in fields.iter_mut() {
			if field.0 == *key {
				field.1 = (*value).to_string();
			}
		}
	}
	let mut text = String::new();
	for (key, value) in &fields {
		text.push_str(key);
		text.push('=');
		text.push_str(value);
		text.push('\n');
	}
	for provider in providers {
		text.push_str("provider=");
		text.push_str(provider);
		text.push_str(":b8b8b8b8\n");
	}
	text
}

fn baseline() -> Vec<u8> {
	library(&record(&["lsrt", "pix"], &[]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), Export::function("encode")])
}

#[test]
fn the_fixture_is_readable_and_a_rebuild_of_the_same_sources_is_compatible() {
	let installed = baseline();
	let image = Elf::parse(&installed).expect("the fixture parses");
	let identity = Identity::read(&image).expect("the fixture carries an identity record");
	assert_eq!(identity.field("artifact"), Some("png"));
	assert_eq!(identity.providers().collect::<Vec<_>>(), vec!["lsrt", "pix"]);
	assert_eq!(decide(&installed, &installed), Verdict::Compatible);
}

#[test]
fn a_new_content_digest_is_the_one_field_allowed_to_move() {
	let installed = baseline();
	let candidate = library(&record(&["lsrt", "pix"], &[(CONTENT_DIGEST_FIELD, &"c".repeat(64))]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), Export::function("encode")]);
	assert_eq!(decide(&installed, &candidate), Verdict::Compatible);
}

#[test]
fn an_added_export_is_admissible_because_nothing_has_resolved_against_it() {
	let installed = baseline();
	let candidate = library(&record(&["lsrt", "pix"], &[]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), Export::function("encode"), Export::function("decode_animated")]);
	assert_eq!(decide(&installed, &candidate), Verdict::Compatible);
}

#[test]
fn a_removed_export_requires_the_cold_path() {
	let installed = baseline();
	let candidate = library(&record(&["lsrt", "pix"], &[]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode")]);
	assert_eq!(decide(&installed, &candidate), Verdict::Incompatible(Reason::ExportRemoved { symbol: "encode" }));
}

#[test]
fn a_narrowed_export_is_a_removal_because_it_is_no_longer_resolvable() {
	let installed = baseline();
	let mut hidden = Export::function("encode");
	hidden.other = 2;
	let candidate = library(&record(&["lsrt", "pix"], &[]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), hidden]);
	assert_eq!(decide(&installed, &candidate), Verdict::Incompatible(Reason::ExportRemoved { symbol: "encode" }));
}

#[test]
fn every_retained_symbol_attribute_is_checked_and_names_itself() {
	let installed = baseline();
	for (field, mutate) in [
		("kind", (|e: &mut Export| e.info = 0x11) as fn(&mut Export)),
		("binding", |e: &mut Export| e.info = 0x22),
		("size", |e: &mut Export| e.size = 32),
		("visibility", |e: &mut Export| e.other = 3),
	] {
		let mut changed = Export::function("encode");
		mutate(&mut changed);
		let candidate = library(&record(&["lsrt", "pix"], &[]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), changed]);
		assert_eq!(decide(&installed, &candidate), Verdict::Incompatible(Reason::ExportChanged { symbol: "encode", field }), "a changed {field} must decide the verdict and name itself");
	}
}

#[test]
fn every_required_identity_field_is_checked_and_names_itself() {
	let installed = baseline();
	for (field, value) in [
		("kind", "executable"),
		("artifact", "apng"),
		("package", "apng"),
		("rustc-commit", "0000000000000000000000000000000000000000"),
		("target", "aarch64-unknown-none"),
		("profile", "debug"),
		("rustflags", "-C relocation-model=static"),
		("features", "wide"),
	] {
		let candidate = library(&record(&["lsrt", "pix"], &[(field, value)]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), Export::function("encode")]);
		let expected = Verdict::Incompatible(Reason::IdentityField { field, installed: Identity::read(&Elf::parse(&installed).unwrap()).unwrap().field(field).unwrap(), candidate: value });
		assert_eq!(decide(&installed, &candidate), expected, "a changed {field} must decide the verdict and name itself");
	}
}

#[test]
fn a_changed_provider_closure_or_order_requires_the_cold_path() {
	let installed = baseline();
	let exports = [Export::function("decode"), Export::function("encode")];

	let reordered = library(&record(&["pix", "lsrt"], &[]), &["lsrt.lslib", "pix.lslib"], &exports);
	assert_eq!(decide(&installed, &reordered), Verdict::Incompatible(Reason::ProviderList { position: 0, installed: Some("lsrt"), candidate: Some("pix") }));

	let added = library(&record(&["lsrt", "pix", "inflate"], &[]), &["lsrt.lslib", "pix.lslib"], &exports);
	assert_eq!(decide(&installed, &added), Verdict::Incompatible(Reason::ProviderList { position: 2, installed: None, candidate: Some("inflate") }));

	let removed = library(&record(&["lsrt"], &[]), &["lsrt.lslib", "pix.lslib"], &exports);
	assert_eq!(decide(&installed, &removed), Verdict::Incompatible(Reason::ProviderList { position: 1, installed: Some("pix"), candidate: None }));
}

#[test]
fn a_changed_needed_list_requires_the_cold_path_even_when_the_record_agrees() {
	let installed = baseline();
	let exports = [Export::function("decode"), Export::function("encode")];
	let candidate = library(&record(&["lsrt", "pix"], &[]), &["lsrt.lslib", "inflate.lslib"], &exports);
	assert_eq!(decide(&installed, &candidate), Verdict::Incompatible(Reason::NeededList { position: 1, installed: Some("pix.lslib"), candidate: Some("inflate.lslib") }));
}

#[test]
fn an_unreadable_or_unidentified_image_is_refused_rather_than_assumed() {
	let installed = baseline();
	assert_eq!(decide(b"not an elf at all", &installed), Verdict::Incompatible(Reason::NotAnElf { installed: true }));
	assert_eq!(decide(&installed, b"not an elf at all"), Verdict::Incompatible(Reason::NotAnElf { installed: false }));

	let unknown = library(&record(&["lsrt", "pix"], &[("format", "liber-image-identity-v2")]), &["lsrt.lslib", "pix.lslib"], &[Export::function("decode"), Export::function("encode")]);
	assert_eq!(decide(&unknown, &unknown), Verdict::Incompatible(Reason::UnknownIdentityFormat { format: "liber-image-identity-v2" }));
}

#[test]
fn a_cross_target_image_is_refused_as_a_machine_mismatch_not_as_a_broken_file() {
	const EM_AARCH64: u16 = 183;
	let installed = baseline();
	let mut candidate = baseline();
	// Restamp the candidate's machine, exactly as another target's build would carry it.
	put(&mut candidate, 18, &EM_AARCH64.to_le_bytes());
	assert_eq!(decide(&installed, &candidate), Verdict::Incompatible(Reason::MachineMismatch { installed: EM_X86_64, candidate: EM_AARCH64 }));

	// Two images for a machine this host is not is still a decidable comparison: the rule
	// reads the images, not the machine it happens to run on.
	let mut other_installed = baseline();
	put(&mut other_installed, 18, &EM_AARCH64.to_le_bytes());
	assert_eq!(decide(&other_installed, &candidate), Verdict::Compatible);
}

#[test]
fn an_import_is_not_an_export_so_it_never_decides_the_verdict() {
	let import = Export { name: "external_symbol", info: 0x12, other: 0, section: 0, size: 0 };
	let installed = library(&record(&["lsrt"], &[]), &["lsrt.lslib"], &[Export::function("decode"), import]);
	// The candidate drops the undefined entry entirely; only real exports are compared.
	let candidate = library(&record(&["lsrt"], &[]), &["lsrt.lslib"], &[Export::function("decode")]);
	assert_eq!(decide(&installed, &candidate), Verdict::Compatible);
}
