use super::*;

fn edge(edges: &[(usize, usize)], dependent: usize, dependency: usize) -> bool {
	edges.contains(&(dependent, dependency))
}

#[test]
fn chain_and_diamond_stop_dependents_before_dependencies() {
	let chain = [(1, 0), (2, 1)];
	let chain_order = reverse_dependency_order(3, |_| true, |dependent, dependency| edge(&chain, dependent, dependency)).unwrap();
	assert_eq!(chain_order, [2, 1, 0]);
	assert!(verify_reverse_dependency_order(&chain_order, 3, |_| true, |dependent, dependency| edge(&chain, dependent, dependency)));

	let diamond = [(1, 0), (2, 0), (3, 1), (3, 2)];
	let diamond_order = reverse_dependency_order(4, |_| true, |dependent, dependency| edge(&diamond, dependent, dependency)).unwrap();
	assert_eq!(diamond_order, [3, 1, 2, 0]);
	assert!(verify_reverse_dependency_order(&diamond_order, 4, |_| true, |dependent, dependency| edge(&diamond, dependent, dependency)));
}

#[test]
fn partial_scope_ignores_external_nodes_but_preserves_internal_edges() {
	let edges = [(1, 0), (2, 1), (3, 1)];
	let selected = |node| matches!(node, 0 | 1 | 3);
	let order = reverse_dependency_order(4, selected, |dependent, dependency| edge(&edges, dependent, dependency)).unwrap();
	assert_eq!(order, [3, 1, 0]);
	assert!(depends_on_any(3, 4, |node| node == 1, |dependent, dependency| edge(&edges, dependent, dependency)));
	assert!(has_active_dependent(1, 4, selected, |dependent, dependency| edge(&edges, dependent, dependency)));
}

#[test]
fn cycles_and_malformed_orders_are_rejected() {
	let cycle = [(0, 1), (1, 0)];
	assert_eq!(reverse_dependency_order(2, |_| true, |dependent, dependency| edge(&cycle, dependent, dependency)), None);
	assert!(!verify_reverse_dependency_order(&[0, 1], 2, |_| true, |dependent, dependency| edge(&cycle, dependent, dependency)));
	assert!(!verify_reverse_dependency_order(&[1, 1], 2, |_| true, |_, _| false));
	assert!(!verify_reverse_dependency_order(&[0], 2, |_| true, |_, _| false));
}
