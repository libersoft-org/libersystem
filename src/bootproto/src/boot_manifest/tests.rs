use super::{Verdict, verify};
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
