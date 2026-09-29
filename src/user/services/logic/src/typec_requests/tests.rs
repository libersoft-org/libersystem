use super::*;

fn connector(number: u32) -> SourceKey {
	SourceKey { slot: 3, generation: 1, binding_generation: 9, local: number - 1 }
}

#[test]
fn one_request_is_outstanding_per_connector_and_another_connector_is_not_held_up() {
	let mut requests = Requests::new();
	let first = requests.start(connector(1), ProviderId(1), 0).expect("idle");
	assert_eq!(requests.start(connector(1), ProviderId(1), 1), Err(Refusal::Busy));
	let other = requests.start(connector(2), ProviderId(1), 1).expect("another connector");
	assert_ne!(first, other);
	assert!(requests.answered(ProviderId(1), first));
	assert!(requests.start(connector(1), ProviderId(1), 2).is_ok(), "answered, the connector takes the next");
}

#[test]
fn an_answer_completes_once_and_a_late_or_foreign_one_is_dropped() {
	let mut requests = Requests::new();
	let corr = requests.start(connector(1), ProviderId(4), 0).expect("idle");
	assert!(!requests.answered(ProviderId(5), corr), "another provider's answer names nothing of its own");
	assert!(requests.answered(ProviderId(4), corr));
	assert!(!requests.answered(ProviderId(4), corr), "answered once");
}

#[test]
fn the_deadline_is_fifteen_seconds_and_what_passes_it_is_indeterminate() {
	let mut requests = Requests::new();
	let corr = requests.start(connector(1), ProviderId(1), 100).expect("idle");
	assert_eq!(requests.next_deadline(), Some(100 + REQUEST_TICKS));
	assert!(requests.tick(100 + REQUEST_TICKS - 1).is_empty());
	assert_eq!(requests.tick(100 + REQUEST_TICKS), [corr]);
	assert!(!requests.answered(ProviderId(1), corr), "the answer after the deadline is late, and dropped");
	assert_eq!(requests.next_deadline(), None);
}

#[test]
fn a_provider_that_goes_completes_its_requests_and_nobody_elses() {
	let mut requests = Requests::new();
	let mine = requests.start(connector(1), ProviderId(1), 0).expect("idle");
	let theirs = requests.start(SourceKey { slot: 4, ..connector(1) }, ProviderId(2), 0).expect("idle");
	assert_eq!(requests.provider_gone(ProviderId(1)), [mine]);
	assert!(requests.answered(ProviderId(2), theirs));
}

#[test]
fn a_request_never_sent_is_released_and_the_bound_holds() {
	let mut requests = Requests::new();
	let corr = requests.start(connector(1), ProviderId(1), 0).expect("idle");
	requests.abandon(corr);
	assert!(requests.start(connector(1), ProviderId(1), 0).is_ok());
	let mut full = Requests::new();
	for at in 0..MAX_OUTSTANDING as u32 {
		full.start(SourceKey { slot: at, ..connector(1) }, ProviderId(1), 0).expect("room");
	}
	assert_eq!(full.start(SourceKey { slot: 999, ..connector(1) }, ProviderId(1), 0), Err(Refusal::Exhausted));
}
