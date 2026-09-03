// The loader has one compile-time knob, and cargo has to be told to watch it.
//
// `option_env!` is read when the crate is COMPILED, and cargo does not rebuild for an environment
// variable it was never told about - so a loader built once without `LIBER_NO_DT_PROFILE` would go
// on being reused for a run that sets it, and the profile would silently be the ordinary boot. See
// `withholds_device_tree`.
fn main() {
	println!("cargo:rerun-if-env-changed=LIBER_NO_DT_PROFILE");
}
