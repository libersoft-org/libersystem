use super::*;

#[test]
fn one_silent_publication_does_not_hold_another_open_or_extend_its_deadline() {
	let mut opens = Opens::new(2);
	let silent = opens.begin("silent", 0, 10, 100).unwrap();
	let ready = opens.begin("ready", 0, 20, 100).unwrap();
	let pending = opens.take_catalogue(ready).unwrap();
	opens.await_stream(pending, 77, 30);
	assert_eq!(opens.next_deadline(), Some(110));
	let expired = opens.expire(110);
	assert_eq!(expired.len(), 1);
	assert_eq!(expired[0].corr, silent);
	assert_eq!(expired[0].channel(), None);
	assert_eq!(opens.next_deadline(), Some(130));
	let connected = opens.take_stream(77, ready).unwrap();
	assert_eq!(connected.item, "ready");
	assert_eq!(connected.deadline, 130);
	assert_eq!(connected.channel(), Some(77));
}

#[test]
fn reply_identity_includes_the_stage_and_its_provider_channel() {
	let mut opens = Opens::new(2);
	let first = opens.begin("first", 0, 0, 100).unwrap();
	let second = opens.begin("second", 0, 0, 100).unwrap();
	assert!(opens.take_stream(77, first).is_none());
	let pending = opens.take_catalogue(first).unwrap();
	opens.await_stream(pending, 77, 1);
	assert!(opens.take_catalogue(first).is_none());
	assert!(opens.take_stream(78, first).is_none());
	assert!(opens.take_stream(77, second).is_none());
	assert_eq!(opens.len(), 2);
	assert_eq!(opens.take_stream(77, first).unwrap().item, "first");
	assert_eq!(opens.take_catalogue(second).unwrap().item, "second");
}

#[test]
fn closed_private_channel_returns_its_resource_without_removing_catalogue_work() {
	let mut opens = Opens::new(2);
	let first = opens.begin("first", 0, 0, 100).unwrap();
	let second = opens.begin("second", 0, 0, 100).unwrap();
	let pending = opens.take_catalogue(first).unwrap();
	opens.await_stream(pending, 77, 1);
	assert!(opens.take_channel(78).is_none());
	let closed = opens.take_channel(77).unwrap();
	assert_eq!(closed.item, "first");
	assert_eq!(closed.channel(), Some(77));
	assert!(opens.take_channel(77).is_none());
	assert_eq!(opens.take_catalogue(second).unwrap().item, "second");
}

#[test]
fn withdrawal_returns_owned_channels_and_late_replies_cannot_adopt_a_replacement() {
	let mut opens = Opens::new(2);
	let old = opens.begin((4, 1), 3, 10, 100).unwrap();
	let pending = opens.take_catalogue(old).unwrap();
	opens.await_stream(pending, 91, 11);
	let cancelled = opens.cancel(|item| *item == (4, 1));
	assert_eq!(cancelled.len(), 1);
	assert_eq!(cancelled[0].channel(), Some(91));
	assert_eq!(cancelled[0].attempts, 3);
	let replacement = opens.begin((4, 2), 0, 12, 100).unwrap();
	assert_ne!(old, replacement);
	assert!(opens.take_stream(91, old).is_none());
	assert!(opens.take_catalogue(old).is_none());
	assert!(opens.cancel(|item| *item == (4, 1)).is_empty());
	assert_eq!(opens.take_catalogue(replacement).unwrap().item, (4, 2));
}

#[test]
fn both_stages_expire_at_the_bound_and_return_exactly_the_owned_resource() {
	let mut opens = Opens::new(2);
	let first = opens.begin(1, 0, 10, 100).unwrap();
	let second = opens.begin(2, 1, 10, 100).unwrap();
	let pending = opens.take_catalogue(second).unwrap();
	opens.await_stream(pending, 91, 10);
	assert!(opens.expire(109).is_empty());
	let expired = opens.expire(110);
	assert_eq!(expired.len(), 2);
	assert_eq!(expired.iter().filter_map(Pending::channel).collect::<Vec<_>>(), [91]);
	assert!(opens.take_catalogue(first).is_none());
	assert!(opens.take_stream(91, second).is_none());
	assert!(opens.expire(111).is_empty());
}

#[test]
fn capacity_and_correlation_exhaustion_never_evict_or_reuse_an_opening() {
	let mut opens = Opens::new(1);
	let first = opens.begin(1, 0, 0, 100).unwrap();
	assert!(opens.begin(2, 0, 0, 100).is_none());
	assert_eq!(opens.take_catalogue(first).unwrap().item, 1);
	opens.next_corr = u32::MAX;
	let last = opens.begin(3, 0, 0, 100).unwrap();
	assert_eq!(last, u32::MAX);
	assert_eq!(opens.take_catalogue(last).unwrap().item, 3);
	assert!(opens.begin(4, 0, 0, 100).is_none());
}
