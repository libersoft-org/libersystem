use super::*;

#[test]
fn the_smallest_live_request_is_the_bound_and_a_closed_one_releases_it() {
	let mut requests = Requests::new();
	assert_eq!(requests.bound(), None);
	requests.add(1, 10, 500).unwrap();
	requests.add(2, 11, 80).unwrap();
	assert_eq!(requests.bound(), Some(80));
	assert!(requests.remove(11));
	assert_eq!(requests.bound(), Some(500));
	assert!(!requests.remove(11), "once");
}

#[test]
fn four_per_process_and_sixty_four_in_all() {
	let mut requests = Requests::new();
	for id in 0..PER_PROCESS as u64 {
		requests.add(7, id, 100).unwrap();
	}
	assert_eq!(requests.add(7, 99, 100), Err(Full::Process));
	for id in PER_PROCESS as u64..IN_ALL as u64 {
		requests.add(1000 + id, id, 100).unwrap();
	}
	assert_eq!(requests.len(), IN_ALL);
	assert_eq!(requests.add(5000, 5000, 100), Err(Full::System));
	assert!(requests.remove(0));
	assert_eq!(requests.add(7, 99, 100), Ok(()), "a released request makes room for its process again");
}
