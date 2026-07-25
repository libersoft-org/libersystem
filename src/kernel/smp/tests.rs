use super::{cpu_count, online_count};

crate::tagged_test!(smp_all_cores_online, [Smp, Kernel, Smoke]);
fn smp_all_cores_online() {
	// init_smp ran before the tests and waited for every core to report in, so
	// the online count must equal the managed core count (and exceed one when
	// QEMU is given more than a single CPU).
	assert_eq!(online_count(), cpu_count());
}
