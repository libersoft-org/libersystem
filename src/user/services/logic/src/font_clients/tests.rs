use super::*;

#[test]
// THE BOUND IS TESTED AT ITS EXACT VALUE AND ONE PAST IT, because a bound tested only past is one
// that could be off by one in the direction nobody notices.
fn the_sixteenth_client_is_admitted_and_the_seventeenth_is_refused() {
	let mut endpoints = Endpoints::new();
	for expected in 0..MAX_FONT_CLIENTS {
		assert_eq!(endpoints.connect(), Ok(expected));
	}
	assert_eq!(endpoints.clients(), MAX_FONT_CLIENTS);
	assert_eq!(endpoints.connect(), Err(Refusal::TooManyClients), "over the bound is a typed refusal at the ask");
}

#[test]
// ONE SUBSCRIPTION PER CLIENT, and a second on the same connection is a refusal rather than a second
// slot - which is what makes the two bounds be the same number.
fn a_client_holds_at_most_one_subscription() {
	let mut endpoints = Endpoints::new();
	let slot = endpoints.connect().expect("a slot");
	assert_eq!(endpoints.subscribe(slot), Ok(()));
	assert_eq!(endpoints.subscribe(slot), Err(Refusal::AlreadySubscribed));
	assert_eq!(endpoints.subscribers(), 1);
	assert_eq!(endpoints.subscribe(MAX_FONT_CLIENTS + 1), Err(Refusal::NotConnected));
}

#[test]
// A CLIENT THAT GOES TAKES ITS SUBSCRIPTION WITH IT, on the same event that gives its place back.
// Without this, sixteen connect-and-vanish cycles exhaust a bound nothing is using.
fn a_disconnect_gives_back_the_place_and_the_subscription() {
	let mut endpoints = Endpoints::new();
	let mut slots = alloc::vec::Vec::new();
	for _ in 0..MAX_FONT_CLIENTS {
		let slot = endpoints.connect().expect("a slot");
		endpoints.subscribe(slot).expect("a subscription");
		slots.push(slot);
	}
	assert_eq!(endpoints.subscribers(), MAX_FONT_SUBSCRIBERS);
	assert_eq!(endpoints.connect(), Err(Refusal::TooManyClients));
	endpoints.disconnect(slots[3]).expect("a live client");
	assert_eq!(endpoints.subscribers(), MAX_FONT_SUBSCRIBERS - 1);
	assert_eq!(endpoints.connect(), Ok(3), "the place comes back");
	assert_eq!(endpoints.disconnect(slots[3]), Ok(()));
	assert_eq!(endpoints.disconnect(slots[3]), Err(Refusal::NotConnected), "a slot that is not live");
}

#[test]
// A GENERATION CHANGE GOES TO THE SUBSCRIBED SLOTS AND NO OTHERS.
fn a_generation_change_reaches_exactly_the_subscribers() {
	let mut endpoints = Endpoints::new();
	let first = endpoints.connect().expect("a slot");
	let second = endpoints.connect().expect("a slot");
	let third = endpoints.connect().expect("a slot");
	endpoints.subscribe(first).expect("subscribed");
	endpoints.subscribe(third).expect("subscribed");
	let reached: alloc::vec::Vec<usize> = endpoints.subscribed_slots().collect();
	assert_eq!(reached, alloc::vec![first, third]);
	assert!(!reached.contains(&second));
}
