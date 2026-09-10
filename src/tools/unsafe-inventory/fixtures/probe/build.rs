// A build script that writes a boundary into OUT_DIR, as the kernel's and the services' do.
fn main() {
	let out = std::env::var("OUT_DIR").expect("OUT_DIR");
	std::fs::write(format!("{out}/generated_boundary.rs"), "pub unsafe fn generated_boundary() -> u32 { 7 }\n").expect("write");
	println!("cargo:rerun-if-changed=build.rs");
}
