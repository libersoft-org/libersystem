// build.rs - link the userspace program at the fixed base its loader expects.

fn main() {
	println!("cargo:rustc-link-arg=-Tlinker/user.ld");
	println!("cargo:rerun-if-changed=linker/user.ld");
	println!("cargo:rerun-if-changed=build.rs");
}
