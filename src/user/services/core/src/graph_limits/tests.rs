use super::can_visit;

#[test]
fn dependency_depth_is_bounded_before_recursion() {
	assert!(can_visit(15, 0, false));
	assert!(!can_visit(16, 0, false));
	assert!(!can_visit(usize::MAX, 0, false));
}

#[test]
fn loaded_module_count_is_bounded_before_allocation() {
	assert!(can_visit(0, 63, false));
	assert!(!can_visit(0, 64, false));
	assert!(!can_visit(0, usize::MAX, false));
}

#[test]
fn an_active_dependency_is_never_reentered() {
	assert!(can_visit(0, 0, false));
	assert!(!can_visit(0, 0, true));
}
