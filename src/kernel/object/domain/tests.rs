use super::{Domain, UNLIMITED};

crate::tagged_test!(domain_hierarchy_charges_aggregate_and_refund, [Domain, Kernel]);
fn domain_hierarchy_charges_aggregate_and_refund() {
	// A child Domain's charges also count against its parent, and the parent's
	// aggregate limit binds even when the child itself is unbounded. The parent
	// caps memory at two pages; the unbounded child may charge two pages but not a
	// third.
	let parent = Domain::new(8192, UNLIMITED, UNLIMITED);
	let child = Domain::new_child(&parent, UNLIMITED, UNLIMITED, UNLIMITED);
	assert!(child.try_charge_memory(4096));
	assert_eq!(parent.account().memory().used(), 4096, "charge propagates to the parent");
	assert!(child.try_charge_memory(4096));
	assert!(!child.try_charge_memory(4096), "parent aggregate binds though the child is unbounded");
	assert_eq!(child.account().memory().used(), 8192, "the refused charge was rolled back at the child");
	assert_eq!(parent.account().memory().used(), 8192, "and left the parent unchanged");
	assert_eq!(child.account().memory().peak(), 8192, "a refused aggregate charge does not raise the child high-water mark");
	assert_eq!(parent.account().memory().peak(), 8192, "the parent records its successful aggregate high-water mark");
	child.uncharge_memory(8192);
	assert_eq!(parent.account().memory().used(), 0, "uncharge propagates to the parent");
	assert_eq!(parent.account().memory().peak(), 8192, "the high-water mark survives refunds");
}
