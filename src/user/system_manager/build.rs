// build.rs - link the userspace program at the fixed base its loader expects,
// using the shared linker script hoisted to the user/ directory.

fn main() {
	println!("cargo:rustc-link-arg=-T../user.ld");
	println!("cargo:rerun-if-changed=../user.ld");
	println!("cargo:rerun-if-changed=build.rs");
}
