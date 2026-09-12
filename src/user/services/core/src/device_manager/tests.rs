use super::*;

// Development boot exercises the real catalogue and the real kernel handle table. The provider
// has no consumer, so its offered endpoint remains the catalogue's responsibility at withdrawal.
// A recorder in a host test cannot establish that this syscall closes the actual endpoint.
pub fn unopened_provider_withdrawal() {
	let (server, offered) = channel().expect("withdrawal fixture channel");
	let binding = BindingId::new(0, 0, 0, 1);
	let mut catalogue = Catalogue::new();
	catalogue.entries.push(Some(Provider { id: ProviderId::new(binding, 0, 1), kind: driver_protocol::provider::BLOCK, token: 1, handle: offered, consumers: 0 }));
	let mut bytes = [0; 128];
	assert!(matches!(try_recv(server, &mut bytes), Polled::Empty));
	assert_eq!(catalogue.withdraw_binding(binding), 1);
	assert!(catalogue.entries.iter().all(Option::is_none));
	let closed = matches!(try_recv(server, &mut bytes), Polled::Closed);
	if !closed {
		debug_write(b"DeviceManager: unopened provider withdrawal failed to close its real channel\n");
	}
	assert!(closed, "an unopened provider's real endpoint must close when its binding ends");
	assert_eq!(catalogue.withdraw_binding(binding), 0, "duplicate withdrawal owns nothing");
	let (subscriber, stream) = channel().expect("late subscriber channel");
	assert!(catalogue.subscribe_stream(driver_protocol::provider::BLOCK, subscriber));
	assert!(matches!(try_recv(stream, &mut bytes), Polled::Empty), "a late subscriber sees no stale publication");
	// A late subscriber gets no stale publication. Reusing the slot under the next binding
	// does not let another withdrawal of the old binding close the replacement's endpoint.
	let (replacement_server, replacement_offered) = channel().expect("replacement fixture channel");
	let replacement = binding.rebound(2);
	catalogue.entries[0] = Some(Provider { id: ProviderId::new(replacement, 0, 2), kind: driver_protocol::provider::BLOCK, token: 1, handle: replacement_offered, consumers: 0 });
	assert_eq!(catalogue.withdraw_binding(binding), 0);
	assert!(matches!(try_recv(replacement_server, &mut bytes), Polled::Empty));
	assert_eq!(catalogue.withdraw_binding(replacement), 1);
	assert!(matches!(try_recv(replacement_server, &mut bytes), Polled::Closed));
	let PolledCaps::Message { len, handles } = try_recv_caps(stream, &mut bytes) else { panic!("withdrawal announcement missing") };
	assert!(handles.is_empty());
	let mut frame_handles = wire::Handles::new();
	let gone = proto::system::provider_catalogue::subscribe_read(&bytes[..len], &mut frame_handles).expect("withdrawal frame");
	assert!(!gone.live && gone.binding_generation == replacement.generation);
	catalogue.close_subscriptions();
	close(stream);
	close(server);
	close(replacement_server);
	debug_write(b"DeviceManager: unopened provider withdrawal closed its real channel\n");
}

// Real waitable handles drive the production shutdown composition. The ready endpoint represents
// a process-exit event without spawning another driver or touching a real device in this fixture.
pub fn pending_shutdown_outcomes() {
	for ready in [true, false] {
		let (event, peer) = channel().expect("shutdown fixture channel");
		if ready {
			assert!(try_send(peer, b"exited", 0));
		}
		let mut node = Node::new(0, &DeviceInfo::default(), Vec::new());
		node.id = node.id.rebound(1);
		node.record.move_to(BindingState::Binding, None);
		node.record.move_to(BindingState::Stopping, None);
		let deadline = clock().saturating_add(1);
		node.teardown = Some(Teardown { pending: driver_binding::Pending { process: event, claim: 0, domain: 0, exited: false, state: Some(driver_binding::CLAIM_FREE) }, deadline, landed: None, cause: FailureCause::Stopped, retrying: false, planned_stop: true, intent: driver_binding::StopIntent::Shutdown });
		let mut catalogue = Catalogue::new();
		let mut bytes = [0; 128];
		settle_shutdown_node(&mut node, &mut catalogue, &mut bytes, deadline);
		assert!(node.teardown.is_none());
		assert!(node.record.state == if ready { BindingState::Stopping } else { BindingState::Quarantined });
		assert!(matches!(try_recv(peer, &mut bytes), Polled::Closed));
		close(peer);
	}
	planned_stop_deadlines();
	debug_write(b"DeviceManager: pending shutdown confirmations and timeout classified\n");
}

// Exercise real STOPPED decoding before and after expiry through both supervision callers.
fn planned_stop_deadlines() {
	for shutdown in [false, true] {
		for timely in [true, false] {
			let (control, driver) = channel().expect("stop deadline fixture channel");
			let mut node = Node::new(0, &DeviceInfo::default(), Vec::new());
			node.id = node.id.rebound(1);
			assert!(node.record.move_to(BindingState::Binding, None));
			assert!(node.record.move_to(BindingState::Stopping, None));
			node.stop_intent = driver_binding::StopIntent::OperatorDisable;
			node.binding = Some(Binding { domain: 0, process: 0, channel: control, claim: 0, key: ClaimKey::default() });
			// Shutdown must apply its earlier overall bound before reading this reply.
			node.stop_deadline = if timely || shutdown { u64::MAX } else { clock().max(1) };
			assert!(send_frame(driver, driver_protocol::Opcode::Stopped, node.id.generation, &[], 0, 0));
			let mut catalogue = Catalogue::new();
			let mut bytes = [0; 128];
			if shutdown {
				let deadline = if timely { clock().saturating_add(100) } else { clock().max(1) };
				settle_shutdown_node(&mut node, &mut catalogue, &mut bytes, deadline);
			} else {
				tick_heartbeats(core::slice::from_mut(&mut node), &mut bytes);
				// A timely frame may be queued before the outer loop gets to its timer.
				expire_planned_stop(&mut node, u64::MAX);
				let _ = advance(&mut node, b"stop-deadline-fixture", &mut catalogue);
				let _ = advance(&mut node, b"stop-deadline-fixture", &mut catalogue);
			}
			assert!(node.binding.is_none() && node.teardown.is_none());
			assert!(node.record.state == BindingState::Disabled);
			assert_eq!(node.stop_deadline, 0);
			let forced = node.incident_report.as_ref().is_some_and(|report| report.cause == FailureCause::Hung);
			if forced == timely {
				debug_write(b"DeviceManager: STOPPED deadline ordering misclassified a planned stop\n");
			}
			assert_eq!(forced, !timely, "only a STOPPED read before expiry can certify a clean stop");
			assert_eq!(node.incident_report.is_none(), timely);
			assert!(matches!(try_recv(driver, &mut bytes), Polled::Closed));
			close(driver);
		}
	}
}

// Exercise the production Node admission, READY/fault reducer and live policy actions. Empty
// holdings keep the fixture away from real devices while still traversing the manager's paths.
pub fn boot_attempt_budget() {
	let mut node = Node::new(0, &DeviceInfo::default(), Vec::new());
	let mut catalogue = Catalogue::new();
	for spent in 1..=MAX_AUTOMATIC_ATTEMPTS {
		node.incident = Incident { opened: true, deadline: 0, teardown_reserve: 0 };
		assert!(node.admit_bind_attempt(clock()));
		node.claim_admitted();
		node.id = node.id.rebound(spent as u64);
		node.binding = Some(Binding { domain: 0, process: 0, channel: 0, claim: 0, key: ClaimKey::default() });
		assert!(node.record.move_to(BindingState::Binding, None));
		assert!(node.push(BindingEvent::Ready { generation: node.id.generation }));
		let _ = advance(&mut node, b"budget-fixture", &mut catalogue);
		assert!(node.record.state == BindingState::Online);
		if node.attempt != spent {
			debug_write(b"DeviceManager: READY refunded the boot automatic-attempt budget\n");
		}
		assert_eq!(node.attempt, spent, "READY cannot refund this boot's automatic attempts");
		assert!(!node.incident.opened);
		// The online fault must replace an expired deadline, without replacing the count.
		node.incident = Incident { opened: true, deadline: 1, teardown_reserve: 0 };
		assert!(node.push(BindingEvent::Exited { generation: node.id.generation }));
		let _ = advance(&mut node, b"budget-fixture", &mut catalogue);
		assert_eq!(node.attempt, spent, "an online fault opens time, not automatic attempts");
		assert!(node.incident.opened && (node.incident.deadline == 0 || node.incident.deadline > 1));
		let _ = advance(&mut node, b"budget-fixture", &mut catalogue);
		assert!(node.teardown.is_none());
		assert!(node.record.state == if spent < MAX_AUTOMATIC_ATTEMPTS { BindingState::Backoff } else { BindingState::Failed });
	}
	assert_eq!(node.record.attempts, MAX_AUTOMATIC_ATTEMPTS);
	assert!(!node.has_bind_allowance());
	assert!(!node.admit_bind_attempt(clock()));
	apply_policy(core::slice::from_mut(&mut node), 0, proto::system::PolicyVerb::Disable, "", &mut catalogue);
	apply_policy(core::slice::from_mut(&mut node), 0, proto::system::PolicyVerb::Enable, "", &mut catalogue);
	assert_eq!(node.attempt, MAX_AUTOMATIC_ATTEMPTS, "disable/enable cannot replenish the boot budget");
	assert!(!node.has_bind_allowance());

	for already_spent in [0, 1, MAX_AUTOMATIC_ATTEMPTS] {
		let mut node = Node::new(0, &DeviceInfo::default(), Vec::new());
		node.attempt = already_spent;
		assert!(node.record.record_failure(FailureCause::DriverMissing));
		apply_policy(core::slice::from_mut(&mut node), 0, proto::system::PolicyVerb::Retry, "", &mut catalogue);
		apply_policy(core::slice::from_mut(&mut node), 0, proto::system::PolicyVerb::Disable, "", &mut catalogue);
		assert!(!node.retry_pending && !node.retry_once && !node.restart_requested, "disable cancels a pending operator request");
		apply_policy(core::slice::from_mut(&mut node), 0, proto::system::PolicyVerb::Enable, "", &mut catalogue);
		assert_eq!(node.attempt, already_spent);
		assert!(!node.retry_once && !node.retry_pending, "enable cannot revive a cancelled allowance");
		assert_eq!(node.has_bind_allowance(), already_spent < MAX_AUTOMATIC_ATTEMPTS);
		assert!(node.record.record_failure(FailureCause::DriverMissing));
		apply_policy(core::slice::from_mut(&mut node), 0, proto::system::PolicyVerb::Retry, "", &mut catalogue);
		assert_eq!(node.attempt, already_spent, "an operator grant must preserve automatic spending");
		assert!(node.retry_once && node.retry_pending && node.has_bind_allowance());
		node.incident = Incident { opened: true, deadline: 300, teardown_reserve: 50 };
		assert!(!node.admit_bind_attempt(250), "an operator cannot spend the teardown reserve");
		assert!(node.retry_pending);
		assert!(node.admit_bind_attempt(20));
		node.refund_unclaimed_attempt();
		assert!(node.retry_pending && !node.finish_operator_attempt(), "a refused claim cannot spend the operator grant");
		assert_eq!(node.attempt, already_spent);
		spend_candidate(&mut node);
		assert!(node.has_bind_allowance(), "an unstarted candidate cannot spend the operator grant");
		assert!(node.admit_bind_attempt(30));
		node.claim_admitted();
		assert_eq!(node.attempt, already_spent);
		assert_eq!(node.record.attempts, 1);
		assert!(!node.retry_pending && node.retry_once);
		assert!(!node.has_bind_allowance() && !node.admit_bind_attempt(40), "the operator grant cannot admit a second attempt");
		// A retryable failure before READY must stop even when automatic spending is still 0/1.
		node.incident = Incident { opened: true, deadline: 0, teardown_reserve: 0 };
		node.id = node.id.rebound(1);
		node.binding = Some(Binding { domain: 0, process: 0, channel: 0, claim: 0, key: ClaimKey::default() });
		assert!(node.record.move_to(BindingState::Binding, None));
		assert!(node.push(BindingEvent::Exited { generation: node.id.generation }));
		let _ = advance(&mut node, b"operator-budget-fixture", &mut catalogue);
		assert!(matches!(advance(&mut node, b"operator-budget-fixture", &mut catalogue), Step::NextCandidate));
		assert!(node.record.state == BindingState::Failed, "a failed operator attempt cannot enter automatic Backoff");
		assert!(node.finish_operator_attempt(), "the completed operator attempt stops automatic fallback");
		assert!(!node.finish_operator_attempt());
		assert_eq!(node.attempt, already_spent);
	}
	debug_write(b"DeviceManager: boot attempt budget and one-shot operator retry verified\n");
}
