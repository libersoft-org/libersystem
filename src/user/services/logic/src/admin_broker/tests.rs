use super::*;
use alloc::string::ToString;

const SESSION: u64 = 77;

fn asked() -> Asked {
	Asked { action: Action::ProbeWrite, target: "probe-target".to_string(), parameters: alloc::vec![1, 2], payload_length: 16, label: "write sixteen bytes".to_string() }
}

fn frozen() -> Descriptor {
	Descriptor { version: descriptor::VERSION, action: Action::ProbeWrite, executor: "org.libersystem.admin-probe".to_string(), executor_epoch: 3, target: "probe-target".to_string(), target_generation: 1, parameters: alloc::vec![1, 2], payload_length: 16, payload_digest: alloc::vec![9; 32] }
}

fn scope() -> Scope {
	Scope { actions: alloc::vec![Action::ProbeWrite], target_prefix: "probe".to_string() }
}

fn dispatches(effects: &[Effect]) -> usize {
	effects.iter().filter(|effect| matches!(effect, Effect::Dispatch { .. })).count()
}

fn journal_of(effects: &[Effect]) -> Option<(RequestId, Event)> {
	effects.iter().rev().find_map(|effect| match effect {
		Effect::Journal { request, event, .. } => Some((*request, *event)),
		_ => None,
	})
}

fn answered(effects: &[Effect]) -> Option<bool> {
	effects.iter().find_map(|effect| match effect {
		Effect::Answer { granted, .. } => Some(*granted),
		_ => None,
	})
}

// A broker with a path to a person and one connection, and the connection's request brought to the prompt.
fn at_prompt() -> (Broker, ConnectionId, RequestId, Vec<Effect>) {
	let mut broker = Broker::new(1);
	broker.path = true;
	let connection = broker.open("adminreq".to_string(), scope(), 42).unwrap();
	let mut effects = Vec::new();
	let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
	assert_eq!(effects, [Effect::Prepare { request }]);
	broker.bind_executor(request, 5);
	broker.prepared(request, Ok((frozen(), 300, 900)), 1, &mut effects);
	assert_eq!(journal_of(&effects), Some((request, Event::Requested)));
	broker.journaled(request, Event::Requested, true, 2, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::AwaitingAttention);
	broker.attention(SESSION, 3, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::AwaitingDecision);
	(broker, connection, request, effects)
}

// Enter on the prompt, and the executor's revalidation answered: the approval's record is in flight.
fn approving() -> (Broker, ConnectionId, RequestId, Vec<Effect>) {
	let (mut broker, connection, request, mut effects) = at_prompt();
	broker.approve(SESSION, 10, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Approving);
	assert!(effects.contains(&Effect::Revalidate { request, executor: 5, operation: 900 }), "the target is checked again before anything is recorded");
	assert_eq!(journal_of(&effects), Some((request, Event::Requested)), "and nothing is recorded before it answers");
	broker.revalidated(request, true, 10, &mut effects);
	assert_eq!(journal_of(&effects), Some((request, Event::Granted)));
	(broker, connection, request, effects)
}

// The rest of the way to a grant.
fn granted() -> (Broker, ConnectionId, RequestId, Vec<Effect>) {
	let (mut broker, connection, request, mut effects) = approving();
	assert_eq!(answered(&effects), None, "no answer before the approval is recorded");
	broker.journaled(request, Event::Granted, true, 11, &mut effects);
	assert_eq!(answered(&effects), Some(true));
	(broker, connection, request, effects)
}

#[test]
fn one_confirmation_is_one_dispatch_and_the_grant_is_spent() {
	let (mut broker, _, request, mut effects) = granted();
	assert!(broker.redeem(request, 20, &mut effects));
	assert_eq!(dispatches(&effects), 0, "nothing is dispatched before consumption is recorded");
	assert!(!broker.redeem(request, 20, &mut effects), "a concurrent redemption finds the grant already being consumed");
	broker.journaled(request, Event::Consumed, true, 21, &mut effects);
	assert_eq!(dispatches(&effects), 1);
	assert!(!broker.redeem(request, 22, &mut effects), "and a replay after it finds it spent");
	broker.executed(request, Outcome::Completed, 23, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Completed);
	assert!(effects.contains(&Effect::Redeemed { request, outcome: Some(Outcome::Completed) }));
	assert!(effects.contains(&Effect::CloseGrant { request }));
	assert_eq!(journal_of(&effects), Some((request, Event::Completed)));
	assert!(!broker.redeem(request, 24, &mut effects));
	assert_eq!(dispatches(&effects), 1, "exactly one, whatever was asked afterwards");
}

#[test]
fn scope_contention_bounds_and_an_absent_path_decline_before_any_executor_is_asked() {
	let mut broker = Broker::new(1);
	let connection = broker.open("adminreq".to_string(), scope(), 1).unwrap();
	let mut effects = Vec::new();
	let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
	assert!(!effects.iter().any(|effect| matches!(effect, Effect::Prepare { .. })), "no path to a person: declined");
	assert_eq!(journal_of(&effects), Some((request, Event::Declined)), "and the decline is recorded before it is answered");
	assert_eq!(answered(&effects), None);
	broker.journaled(request, Event::Declined, true, 1, &mut effects);
	assert_eq!(answered(&effects), Some(false));
	broker.settle();
	broker.path = true;
	let declined = |broker: &mut Broker, connection: ConnectionId, asked: Asked| {
		let mut effects = Vec::new();
		broker.ask(connection, asked, 0, &mut effects).unwrap();
		let prepared = effects.iter().any(|effect| matches!(effect, Effect::Prepare { .. }));
		let request = broker.requests().last().unwrap().id;
		broker.journaled(request, Event::Declined, true, 1, &mut effects);
		broker.settle();
		!prepared
	};
	assert!(declined(&mut broker, connection, Asked { action: Action::FirmwareDownload, ..asked() }), "an action outside the scope");
	assert!(declined(&mut broker, connection, Asked { target: "dfu-0".to_string(), ..asked() }), "a target outside the prefix");
	assert!(declined(&mut broker, connection, Asked { label: "x".repeat(129), ..asked() }), "a label past its bound");
	// ONE REQUEST HOLDS THE CONFIRMATION PATH; a second from anyone is declined, not queued.
	let other = broker.open("adminreq".to_string(), scope(), 2).unwrap();
	let mut effects = Vec::new();
	broker.ask(connection, asked(), 0, &mut effects).unwrap();
	assert!(declined(&mut broker, other, asked()), "a competing request");
	// AND ONE REQUEST AT A TIME ON A CONNECTION.
	assert_eq!(broker.ask(connection, asked(), 0, &mut Vec::new()), None);
	// SIXTEEN CONNECTIONS.
	while broker.connections().len() < MAX_CONNECTIONS {
		broker.open("adminreq".to_string(), scope(), 3).unwrap();
	}
	assert_eq!(broker.open("adminreq".to_string(), scope(), 3), Err(Refusal::Exhausted));
}

#[test]
fn an_executor_that_prepared_something_else_is_refused_and_released() {
	for wrong in [
		Descriptor { action: Action::FirmwareDownload, ..frozen() },
		Descriptor { parameters: alloc::vec![9], ..frozen() },
		Descriptor { payload_length: 17, ..frozen() },
		Descriptor { payload_digest: alloc::vec![0; 31], ..frozen() },
		Descriptor { version: 2, ..frozen() },
	] {
		let mut broker = Broker::new(1);
		broker.path = true;
		let connection = broker.open("adminreq".to_string(), scope(), 1).unwrap();
		let mut effects = Vec::new();
		let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
		broker.bind_executor(request, 5);
		broker.prepared(request, Ok((wrong, 200, 900)), 1, &mut effects);
		assert!(effects.contains(&Effect::Release { request, executor: 5, operation: 900 }), "what it prepared is released");
		assert_eq!(journal_of(&effects), Some((request, Event::Declined)));
	}
	// And an encoding past 4096 bytes.
	let mut broker = Broker::new(1);
	broker.path = true;
	let connection = broker.open("adminreq".to_string(), scope(), 1).unwrap();
	let mut effects = Vec::new();
	let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
	broker.prepared(request, Ok((frozen(), 4097, 900)), 1, &mut effects);
	assert_eq!(journal_of(&effects), Some((request, Event::Declined)));
}

#[test]
fn owner_death_is_requester_loss_in_every_phase_and_nothing_revives_it() {
	// DURING PREPARATION: the late preparation is released, never shown.
	let mut broker = Broker::new(1);
	broker.path = true;
	let connection = broker.open("adminreq".to_string(), scope(), 1).unwrap();
	let mut effects = Vec::new();
	let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
	broker.bind_executor(request, 5);
	broker.owner_died(connection, 1, &mut effects);
	broker.prepared(request, Ok((frozen(), 300, 900)), 2, &mut effects);
	assert!(effects.contains(&Effect::Release { request, executor: 5, operation: 900 }));
	assert_ne!(broker.request(request).unwrap().state, State::Prepared);
	// ON THE PROMPT: declined, and the screen closes.
	let (mut broker, connection, request, mut effects) = at_prompt();
	broker.owner_died(connection, 5, &mut effects);
	assert!(effects.contains(&Effect::Hide));
	broker.approve(SESSION, 6, &mut effects);
	assert_ne!(broker.request(request).unwrap().state, State::Approving, "an approval after the death approves nothing");
	// WITH THE REVALIDATION IN FLIGHT: its late answer records nothing.
	let (mut broker, connection, request, mut effects) = at_prompt();
	broker.approve(SESSION, 10, &mut effects);
	broker.owner_died(connection, 11, &mut effects);
	broker.revalidated(request, true, 12, &mut effects);
	assert_ne!(journal_of(&effects), Some((request, Event::Granted)), "no approval is recorded for a dead owner");
	// WITH THE APPROVAL'S RECORD IN FLIGHT: its late success grants nothing.
	let (mut broker, connection, request, mut effects) = approving();
	broker.owner_died(connection, 11, &mut effects);
	broker.journaled(request, Event::Granted, true, 12, &mut effects);
	assert_eq!(answered(&effects), None, "no grant was issued");
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	// AN UNUSED GRANT, HELD BY A DELEGATE: the channel is open, the owner is not.
	let (mut broker, connection, request, mut effects) = granted();
	broker.owner_died(connection, 20, &mut effects);
	assert!(effects.contains(&Effect::CloseGrant { request }));
	assert!(!broker.redeem(request, 21, &mut effects));
	assert_eq!(dispatches(&effects), 0);
	// WITH THE CONSUMPTION'S RECORD IN FLIGHT: the late acknowledgment finds the owner gone - the grant stays
	// spent, the failure is recorded, and nothing is dispatched or retried.
	let (mut broker, connection, request, mut effects) = granted();
	assert!(broker.redeem(request, 20, &mut effects));
	broker.owner_died(connection, 21, &mut effects);
	broker.journaled(request, Event::Consumed, true, 22, &mut effects);
	assert_eq!(dispatches(&effects), 0);
	assert_eq!(broker.request(request).unwrap().state, State::Failed);
	assert_eq!(journal_of(&effects), Some((request, Event::Failed)));
	assert!(!broker.redeem(request, 23, &mut effects));
	// A CLOSED CONNECTION IS THE SAME LOSS, independently of the task - and it ends as cancelled.
	let (mut broker, connection, request, mut effects) = granted();
	broker.closed(connection, 20, &mut effects);
	assert!(!broker.redeem(request, 21, &mut effects));
	assert_eq!(dispatches(&effects), 0);
	broker.journaled(request, Event::Declined, true, 22, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Cancelled);
}

#[test]
fn a_live_owner_delegates_exactly_one_attempt_with_its_own_attribution() {
	// The grant is redeemed by whoever holds its channel; what is dispatched is the request's own operation,
	// through its own executor, and the record keeps the original connection.
	let (mut broker, connection, request, mut effects) = granted();
	assert!(broker.redeem(request, 20, &mut effects));
	broker.journaled(request, Event::Consumed, true, 21, &mut effects);
	assert!(effects.contains(&Effect::Dispatch { request, executor: 5, operation: 900 }));
	assert_eq!(broker.request(request).unwrap().connection, connection);
	assert_eq!(broker.connection(connection).unwrap().component, "adminreq");
	assert_eq!(dispatches(&effects), 1);
}

#[test]
fn expiry_and_replay_end_before_consumption() {
	// THE CONFIRMATION EXPIRES sixty seconds after the request was recorded.
	let (mut broker, _, request, mut effects) = at_prompt();
	broker.tick(2 + CONFIRM_TICKS, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	assert!(effects.contains(&Effect::Hide));
	broker.journaled(request, Event::Declined, true, 3 + CONFIRM_TICKS, &mut effects);
	assert_eq!((broker.request(request).unwrap().state, answered(&effects)), (State::Expired, Some(false)), "expired, and answered as every refusal is");
	// THE GRANT EXPIRES thirty seconds after the approval was recorded.
	let (mut broker, _, request, mut effects) = granted();
	broker.tick(11 + GRANT_TICKS, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Expired);
	assert!(effects.contains(&Effect::CloseGrant { request }));
	assert!(!broker.redeem(request, 11 + GRANT_TICKS, &mut effects));
	assert_eq!(dispatches(&effects), 0);
}

#[test]
fn a_journal_that_fails_or_never_answers_issues_no_authority() {
	// The request's own record.
	let mut broker = Broker::new(1);
	broker.path = true;
	let connection = broker.open("adminreq".to_string(), scope(), 1).unwrap();
	let mut effects = Vec::new();
	let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
	broker.prepared(request, Ok((frozen(), 300, 900)), 1, &mut effects);
	broker.journaled(request, Event::Requested, false, 2, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	// The approval's record: failed, or silent past five seconds.
	let (mut broker, _, request, mut effects) = approving();
	broker.journaled(request, Event::Granted, false, 11, &mut effects);
	assert_eq!(answered(&effects), None);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	let (mut broker, _, request, mut effects) = approving();
	broker.tick(10 + EXCHANGE_TICKS, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining, "a lost storage reply is not a commit");
	broker.journaled(request, Event::Granted, true, 10 + EXCHANGE_TICKS + 1, &mut effects);
	assert_eq!(answered(&effects), None, "and a late acknowledgment revives nothing");
	// The consumption's record: failed, or silent - no dispatch either way, and the grant is spent.
	let (mut broker, _, request, mut effects) = granted();
	broker.redeem(request, 20, &mut effects);
	broker.journaled(request, Event::Consumed, false, 21, &mut effects);
	assert_eq!((broker.request(request).unwrap().state, dispatches(&effects)), (State::Failed, 0));
	let (mut broker, _, request, mut effects) = granted();
	broker.redeem(request, 20, &mut effects);
	broker.tick(20 + EXCHANGE_TICKS, &mut effects);
	broker.journaled(request, Event::Consumed, true, 20 + EXCHANGE_TICKS + 1, &mut effects);
	assert_eq!((broker.request(request).unwrap().state, dispatches(&effects)), (State::Failed, 0));
	// An outcome that could not be recorded blocks further approvals until storage recovers.
	let (mut broker, connection, request, mut effects) = granted();
	broker.redeem(request, 20, &mut effects);
	broker.journaled(request, Event::Consumed, true, 21, &mut effects);
	broker.executed(request, Outcome::Completed, 22, &mut effects);
	broker.journaled(request, Event::Completed, false, 23, &mut effects);
	assert!(broker.blocked);
	broker.settle();
	let mut effects = Vec::new();
	let next = broker.ask(connection, asked(), 30, &mut effects).unwrap();
	assert!(!effects.iter().any(|effect| matches!(effect, Effect::Prepare { .. })), "declined while an outcome is unrecorded");
	broker.journaled(next, Event::Declined, true, 31, &mut effects);
	broker.settle();
	broker.recovered();
	let mut effects = Vec::new();
	broker.ask(connection, asked(), 40, &mut effects).unwrap();
	assert!(effects.iter().any(|effect| matches!(effect, Effect::Prepare { .. })), "and admitted again once it has recovered");
}

#[test]
fn a_lost_execution_reply_is_an_unknown_outcome_and_never_retried() {
	let (mut broker, _, request, mut effects) = granted();
	broker.redeem(request, 20, &mut effects);
	broker.journaled(request, Event::Consumed, true, 21, &mut effects);
	broker.executor_lost(5, 22, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::OutcomeUnknown);
	assert!(effects.contains(&Effect::Redeemed { request, outcome: Some(Outcome::Unknown) }));
	assert_eq!(journal_of(&effects), Some((request, Event::OutcomeUnknown)));
	assert!(!broker.redeem(request, 23, &mut effects));
	assert_eq!(dispatches(&effects), 1, "the one dispatch, and no second");
	// An executor lost before consumption voids the grant.
	let (mut broker, _, request, mut effects) = granted();
	broker.executor_lost(5, 20, &mut effects);
	assert!(!broker.redeem(request, 21, &mut effects));
	assert_eq!(dispatches(&effects), 0);
}

#[test]
fn escape_a_lost_path_and_a_stale_session_approve_nothing() {
	let (mut broker, _, request, mut effects) = at_prompt();
	broker.approve(SESSION + 1, 10, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::AwaitingDecision, "another session's Enter");
	broker.refuse(SESSION, "declined by the person", 11, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	let (mut broker, _, request, mut effects) = at_prompt();
	broker.set_path(false, 10, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	broker.approve(SESSION, 11, &mut effects);
	assert_eq!(dispatches(&effects), 0);
	// The chord with nothing waiting shows the idle screen.
	let mut broker = Broker::new(1);
	let mut effects = Vec::new();
	broker.attention(SESSION, 0, &mut effects);
	assert_eq!(effects, [Effect::Show { request: None, session: SESSION }]);
}

#[test]
fn a_target_replaced_after_preparation_is_declined_at_revalidation() {
	let (mut broker, _, request, mut effects) = at_prompt();
	broker.approve(SESSION, 10, &mut effects);
	broker.revalidated(request, false, 11, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	assert!(effects.contains(&Effect::Release { request, executor: 5, operation: 900 }), "the stale preparation is released");
	broker.journaled(request, Event::Declined, true, 12, &mut effects);
	assert_eq!(answered(&effects), Some(false));
	assert!(!broker.redeem(request, 13, &mut effects));
	assert_eq!(dispatches(&effects), 0);
	// A revalidation that never answers is a local exchange that failed.
	let (mut broker, _, request, mut effects) = at_prompt();
	broker.approve(SESSION, 10, &mut effects);
	broker.tick(10 + EXCHANGE_TICKS, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining);
	broker.revalidated(request, true, 11 + EXCHANGE_TICKS, &mut effects);
	assert_ne!(journal_of(&effects), Some((request, Event::Granted)), "and its late answer records nothing");
}

#[test]
fn an_owner_that_cannot_be_observed_is_an_owner_lost() {
	let (mut broker, connection, request, mut effects) = granted();
	broker.observer_failed(connection, 20, &mut effects);
	assert!(effects.contains(&Effect::CloseGrant { request }));
	assert!(!broker.redeem(request, 21, &mut effects));
	assert_eq!(dispatches(&effects), 0);
	// And a request asked on a connection whose owner can no longer be observed is declined outright.
	broker.journaled(request, Event::Declined, true, 22, &mut effects);
	broker.settle();
	let mut effects = Vec::new();
	let next = broker.ask(connection, asked(), 30, &mut effects).unwrap();
	assert!(!effects.iter().any(|effect| matches!(effect, Effect::Prepare { .. })));
	assert_eq!(journal_of(&effects), Some((next, Event::Declined)));
}

// THE SERVICE APPLIES A TERMINATION IT FINDS READY BEFORE ANYTHING ELSE THAT IS READY - it checks each owner
// explicitly rather than trusting the order a wait reports - and this is what that order has to guarantee:
// a queued redemption, a late approval record and a late consumption record revive nothing.
#[test]
fn termination_ready_beside_other_work_wins() {
	// Beside a queued redemption.
	let (mut broker, connection, request, mut effects) = granted();
	broker.owner_died(connection, 20, &mut effects);
	assert!(!broker.redeem(request, 20, &mut effects), "the redemption that was queued behind it is refused");
	assert_eq!(dispatches(&effects), 0);
	// Beside the approval's acknowledgment.
	let (mut broker, connection, request, mut effects) = approving();
	broker.owner_died(connection, 11, &mut effects);
	broker.journaled(request, Event::Granted, true, 11, &mut effects);
	assert_eq!(answered(&effects), None);
	// Beside the consumption's acknowledgment: the grant stays spent, and nothing is dispatched or retried.
	let (mut broker, connection, request, mut effects) = granted();
	assert!(broker.redeem(request, 20, &mut effects));
	broker.owner_died(connection, 21, &mut effects);
	broker.journaled(request, Event::Consumed, true, 21, &mut effects);
	assert_eq!((broker.request(request).unwrap().state, dispatches(&effects)), (State::Failed, 0));
	assert!(!broker.redeem(request, 22, &mut effects));
}

#[test]
fn a_redemption_repeated_or_concurrent_dispatches_once_and_delegation_keeps_scope_and_attribution() {
	let (mut broker, connection, request, mut effects) = granted();
	let scope_before = broker.connection(connection).unwrap().scope.clone();
	// Two holders of the one grant ask at once: one consumes it, the other finds it consuming.
	let first = broker.redeem(request, 20, &mut effects);
	let second = broker.redeem(request, 20, &mut effects);
	assert_eq!((first, second), (true, false));
	broker.journaled(request, Event::Consumed, true, 21, &mut effects);
	broker.executed(request, Outcome::Completed, 22, &mut effects);
	for later in 23..30 {
		assert!(!broker.redeem(request, later, &mut effects));
	}
	assert_eq!(dispatches(&effects), 1);
	assert_eq!(broker.connection(connection).unwrap().scope, scope_before, "delegating the grant changed nothing the connection was minted with");
	assert_eq!(broker.connection(connection).unwrap().launch, 42);
}

#[test]
fn an_executor_that_never_answers_its_preparation_is_a_decline_and_its_late_answer_is_released() {
	let mut broker = Broker::new(1);
	broker.path = true;
	let connection = broker.open("adminreq".to_string(), scope(), 1).unwrap();
	let mut effects = Vec::new();
	let request = broker.ask(connection, asked(), 0, &mut effects).unwrap();
	broker.bind_executor(request, 5);
	broker.tick(EXCHANGE_TICKS, &mut effects);
	assert_eq!(broker.request(request).unwrap().state, State::Declining, "five seconds of silence is a failed exchange");
	broker.journaled(request, Event::Declined, true, EXCHANGE_TICKS + 1, &mut effects);
	assert_eq!(answered(&effects), Some(false));
	// THE PREPARATION ARRIVES AFTERWARDS: nothing is shown, and what it prepared is released.
	broker.prepared(request, Ok((frozen(), 300, 900)), EXCHANGE_TICKS + 2, &mut effects);
	assert!(effects.contains(&Effect::Release { request, executor: 5, operation: 900 }));
	assert!(!effects.iter().any(|effect| matches!(effect, Effect::Show { .. } | Effect::Dispatch { .. })));
	// AND THE CONNECTION MAY ASK AGAIN once the decline is settled: capacity is released on a terminal failure.
	broker.settle();
	assert!(broker.ask(connection, asked(), EXCHANGE_TICKS + 3, &mut Vec::new()).is_some());
}
