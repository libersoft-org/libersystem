use super::*;

fn binding(watchdog: bool, depth: u32, bind_at: u64) -> Binding {
	Binding { watchdog, depth, bind_at }
}

// THE MACHINE: a block driver (bound first), the xHCI controller, a watchdog, and a class driver that consumes the xHCI's
// bus - bound BEFORE the watchdog but after the controller, and deeper.
fn machine() -> [Binding; 4] {
	[binding(false, 0, 10), binding(false, 0, 20), binding(true, 0, 40), binding(false, 1, 30)]
}

#[test]
fn a_consumer_is_suspended_before_its_provider_and_a_watchdog_last() {
	assert_eq!(suspend_order(&machine()), [3, 1, 0, 2]);
}

#[test]
fn the_resume_takes_the_watchdog_first_and_providers_before_consumers() {
	assert_eq!(resume_order(&machine(), &[3, 1, 0, 2]), [2, 0, 1, 3]);
}

#[test]
fn a_consumer_bound_before_its_provider_still_goes_first() {
	// A rebound provider is younger than its consumer; the depth, not the bind order, decides.
	let bindings = [binding(false, 1, 10), binding(false, 0, 50)];
	assert_eq!(suspend_order(&bindings), [0, 1]);
	assert_eq!(resume_order(&bindings, &[0, 1]), [1, 0]);
}

#[test]
fn only_the_bindings_suspended_are_resumed() {
	assert_eq!(resume_order(&machine(), &[3, 1]), [1, 3]);
}

#[test]
fn a_watchdog_publisher_that_consumes_a_controller_goes_before_it_and_comes_back_after_it() {
	// THE SSIF SHAPE: a BMC reached over the SMBus controller (0), bound after it and one level deeper, declaring a
	// watchdog; and a KCS BMC's watchdog on no controller (2).
	let bindings = [binding(false, 0, 10), binding(true, 1, 20), binding(true, 0, 30)];
	assert_eq!(suspend_order(&bindings), [1, 0, 2], "the child before its controller, the leaf's watchdog last");
	assert_eq!(resume_order(&bindings, &[1, 0, 2]), [2, 0, 1], "the leaf's watchdog first, the child after its controller");
}
