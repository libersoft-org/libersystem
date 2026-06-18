// build.rs - link the userspace programs at the fixed base their loader expects.

fn main() {
	println!("cargo:rustc-link-arg=-Tlinker/user.ld");
	println!("cargo:rerun-if-changed=linker/user.ld");
	println!("cargo:rerun-if-changed=build.rs");
}
