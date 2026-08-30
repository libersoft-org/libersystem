use super::{Verdict, names, verify};
use std::format;
use std::string::String;
use std::vec::Vec;

// A manifest naming two files, exactly as `mkpackages` writes one.
fn manifest(rows: &[(&str, &[u8])]) -> Vec<u8> {
	let mut out = String::from("liberboot-manifest 1\n");
	for (path, bytes) in rows {
		let digest = crate::sha256::digest(bytes);
		for byte in digest {
			out.push_str(&format!("{byte:02x}"));
		}
		out.push_str(&format!("  {path}\n"));
	}
	out.into_bytes()
}

#[test]
fn a_good_boot_verifies_every_file_it_reads() {
	let kernel: &[u8] = b"the kernel image";
	let list: &[u8] = b"shell libexec/shell\n";
	let m = manifest(&[("kernel", kernel), ("etc/bootstrap.list", list)]);
	assert_eq!(verify(&m, b"kernel", kernel), Verdict::Ok);
	assert_eq!(verify(&m, b"etc/bootstrap.list", list), Verdict::Ok);
}

#[test]
fn an_altered_kernel_is_refused() {
	let kernel: &[u8] = b"the kernel image";
	let m = manifest(&[("kernel", kernel)]);
	// One byte, which is what a half-written image or a mixed build looks like from here.
	assert_eq!(verify(&m, b"kernel", b"the kernel imagf"), Verdict::Mismatch);
	// And a truncation, which is the other half of the same failure.
	assert_eq!(verify(&m, b"kernel", b"the kernel imag"), Verdict::Mismatch);
}

#[test]
fn an_altered_bootstrap_file_is_refused_and_named_apart_from_an_unnamed_one() {
	let list: &[u8] = b"shell libexec/shell\n";
	let shell: &[u8] = b"ELF...";
	let m = manifest(&[("etc/bootstrap.list", list), ("libexec/shell", shell)]);
	assert_eq!(verify(&m, b"libexec/shell", b"ELF..!"), Verdict::Mismatch, "changed content is a mismatch");
	// A file the build staged and did not record is a DIFFERENT failure from one that changed, and
	// the loader prints a different line for it: one points at the build, the other at the bytes.
	assert_eq!(verify(&m, b"libexec/init", shell), Verdict::NotNamed, "a file with no row is not named");
}

#[test]
fn a_manifest_that_is_corrupt_or_absent_is_refused() {
	let kernel: &[u8] = b"the kernel image";
	assert_eq!(verify(b"", b"kernel", kernel), Verdict::NotAManifest, "an empty file is not a manifest");
	assert_eq!(verify(b"liberboot-manifest 2\n", b"kernel", kernel), Verdict::NotAManifest, "a version this loader does not know");
	assert_eq!(verify(b"garbage\nrows\n", b"kernel", kernel), Verdict::NotAManifest, "no version line at all");
	// A row whose digest is not hexadecimal is a corrupt row, not a match.
	let mut broken = String::from("liberboot-manifest 1\n");
	broken.push_str(&format!("{}  kernel\n", "z".repeat(64)));
	assert_eq!(verify(broken.as_bytes(), b"kernel", kernel), Verdict::Mismatch, "a row that is not a digest cannot match");
}

#[test]
fn a_manifest_says_which_files_it_supplied_without_being_handed_them() {
	// "DID THIS MANIFEST SUPPLY THE KERNEL" IS A DIFFERENT QUESTION FROM "IS THIS THE KERNEL IT
	// RECORDS", and the loader needs the first to decide whether a source was SELECTED.
	//
	// A source whose manifest names `boot/kernel` is the source the running kernel came from, so a
	// missing bootstrap list on it is a refusal rather than an absence. `verify` cannot answer that:
	// it needs the bytes, and the question is asked about a file the caller is not holding.
	let mut manifest = String::from("liberboot-manifest 1\n");
	manifest.push_str(&format!("{}  boot/kernel\n", "ab".repeat(32)));
	manifest.push_str(&format!("{}  etc/bootstrap.list\n", "cd".repeat(32)));
	assert!(names(manifest.as_bytes(), b"boot/kernel"), "the manifest names the kernel it supplied");
	assert!(names(manifest.as_bytes(), b"etc/bootstrap.list"));
	assert!(!names(manifest.as_bytes(), b"boot/kernel2"), "a longer path is not the one named");
	assert!(!names(manifest.as_bytes(), b"kernel"), "and neither is a shorter one");
	// A file that is not recorded, and a manifest that is not one.
	assert!(!names(manifest.as_bytes(), b"etc/motd"));
	assert!(!names(b"", b"boot/kernel"), "an empty file names nothing");
	assert!(!names(b"garbage\nrows\n", b"boot/kernel"), "and neither does something that is not a manifest");
	// A row whose digest is unreadable still NAMES the path: the question is which files this
	// manifest is about, and a corrupt digest is `verify`'s to refuse.
	let mut broken = String::from("liberboot-manifest 1\n");
	broken.push_str(&format!("{}  boot/kernel\n", "z".repeat(64)));
	assert!(names(broken.as_bytes(), b"boot/kernel"));
}
