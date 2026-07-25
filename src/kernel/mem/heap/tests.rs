crate::tagged_test!(heap_box_vec, [Memory, Smoke]);
fn heap_box_vec() {
	let boxed = alloc::boxed::Box::new(42u64);
	assert_eq!(*boxed, 42);
	let mut values = alloc::vec::Vec::new();
	for value in 0u64..1000 {
		values.push(value);
	}
	let sum: u64 = values.iter().sum();
	assert_eq!(sum, 1000 * 999 / 2);
}
