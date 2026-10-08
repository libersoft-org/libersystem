use super::*;

#[test]
fn a_failure_is_asked_again_at_a_doubling_delay_and_given_up_after_the_last() {
	let mut retry: ProviderRetry<u32> = ProviderRetry::new(100);
	let mut now = 1_000;
	let mut attempts = 0;
	let mut asked_at = Vec::new();
	loop {
		match retry.failed(7, attempts, now) {
			Next::At(at) => {
				asked_at.push(at - now);
				assert_eq!(retry.next_deadline(), Some(at));
				assert!(retry.due(at - 1).is_empty(), "not before its tick");
				let due = retry.due(at);
				assert_eq!(due.len(), 1);
				attempts = due[0].1;
				now = at;
			}
			Next::GivenUp => break,
		}
	}
	assert_eq!(asked_at, [100, 200, 400, 800, 1600, 3200, 6400], "one, two, four ... sixty-four bases");
	assert_eq!(attempts, RETRIES, "and given up after the last retry");
	assert!(retry.is_empty());
}

#[test]
fn a_withdrawn_provider_is_not_asked_again() {
	let mut retry: ProviderRetry<u32> = ProviderRetry::new(100);
	assert_eq!(retry.failed(1, 0, 0), Next::At(100));
	assert_eq!(retry.failed(2, 0, 50), Next::At(150));
	retry.withdraw(|item| *item == 1);
	assert_eq!(retry.len(), 1);
	assert_eq!(retry.next_deadline(), Some(150));
	let due = retry.due(1_000);
	assert_eq!(due, alloc::vec![(2, 1)], "only the one still published");
}

#[test]
fn several_due_at_once_all_come_out() {
	let mut retry: ProviderRetry<u32> = ProviderRetry::new(10);
	for item in 0..4 {
		retry.failed(item, 0, item as u64);
	}
	let mut due: Vec<u32> = retry.due(100).into_iter().map(|(item, _)| item).collect();
	due.sort();
	assert_eq!(due, [0, 1, 2, 3]);
	assert_eq!(retry.next_deadline(), None);
}

#[test]
fn a_full_opening_set_leaves_other_due_retries_queued_without_spending_attempts() {
	let mut retry = ProviderRetry::new(100);
	retry.failed(1, 0, 0);
	retry.failed(2, 1, 0);
	assert!(retry.due_one(99).is_none());
	assert_eq!(retry.due_one(200), Some((1, 1)));
	assert_eq!(retry.next_deadline(), Some(200));
	assert!(retry.contains(|item| *item == 2));
	// The second waits for the shared opening capacity, retaining the same due time and retry count.
	assert_eq!(retry.due_one(450), Some((2, 2)));
	assert!(retry.is_empty());
}
