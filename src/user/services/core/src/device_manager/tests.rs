use super::*;

// Development boot exercises the real catalogue and the real kernel handle table. The provider
// has no consumer, so its offered endpoint remains the catalogue's responsibility at withdrawal.
// A recorder in a host test cannot establish that this syscall closes the actual endpoint.
pub unsafe fn unopened_provider_withdrawal() {
	unsafe {
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
}

// Real waitable handles drive the production shutdown composition. The ready endpoint represents
// a process-exit event without spawning another driver or touching a real device in this fixture.
pub unsafe fn pending_shutdown_outcomes() {
	unsafe {
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
		debug_write(b"DeviceManager: pending shutdown confirmations and timeout classified\n");
	}
}
