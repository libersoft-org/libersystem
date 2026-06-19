// build.rs - link the userspace drivers at the fixed base their loader expects,
// using the shared linker script hoisted to the user/ directory.

fn main() {
	println!("cargo:rustc-link-arg=-T../user.ld");
	println!("cargo:rerun-if-changed=../user.ld");
	println!("cargo:rerun-if-changed=build.rs");
}
