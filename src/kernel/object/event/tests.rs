use super::Event;

crate::tagged_test!(event_object_latches_and_clears, [Object, Kernel]);
fn event_object_latches_and_clears() {
	let event = Event::create();
	assert!(!event.is_signaled());
	event.signal();
	assert!(event.is_signaled());
	event.clear();
	assert!(!event.is_signaled());
}
