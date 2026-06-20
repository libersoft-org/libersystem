// build.rs - link every userspace program at the fixed base its loader expects,
// using the shared linker script in this directory. One shared build script for
// all the userspace crates; each points at it via `build = "../build.rs"` so the
// linker wiring lives in exactly one place. Build scripts run with the crate dir
// as the working directory, so the `../` paths resolve into this user/ directory.

fn main() {
	println!("cargo:rustc-link-arg=-T../user.ld");
	println!("cargo:rerun-if-changed=../user.ld");
	println!("cargo:rerun-if-changed=../build.rs");
}
