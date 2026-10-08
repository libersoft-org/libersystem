use crate::engine::*;
use crate::message::*;
use crate::pdo::*;
use crate::tcpci;
use crate::timer::*;
use alloc::vec;
use alloc::vec::Vec;

fn fixed(millivolts: u32, milliamps: u32) -> u32 {
	(millivolts / 50) << 10 | milliamps / 10
}

fn pps(max_mv: u32, min_mv: u32, ma: u32) -> u32 {
	3 << 30 | (max_mv / 100) << 17 | (min_mv / 100) << 8 | ma / 50
}

// The gate's board: Fixed 5 V at 3 A and 15 V at 2 A, operating at 15 W.
fn board() -> Sink {
	Sink { pdos: vec![SinkPdo::Fixed { millivolts: 5_000, milliamps: 3_000 }, SinkPdo::Fixed { millivolts: 15_000, milliamps: 2_000 }], operational_microwatts: 15_000_000 }
}

// A revision 3.1 charger: 5, 9, 12, 15 and 20 V at 3 A, and a PPS offer.
fn charger() -> Vec<u32> {
	vec![fixed(5_000, 3_000), fixed(9_000, 3_000), fixed(12_000, 3_000), fixed(15_000, 3_000), fixed(20_000, 3_000), pps(21_000, 3_300, 3_000)]
}

fn source_message(kind: Kind, id: u8, objects: &[u32], revision: Revision) -> Message {
	let mut bytes = encode(kind, revision, id, objects);
	// A source's header: power role source, data role DFP.
	bytes[1] |= 1;
	bytes[0] |= 0x20;
	decode(&bytes).expect("a message this test built")
}

// ------------------------------------------------------------------ the codec

#[test]
fn the_codec_believes_only_what_checks() {
	let bytes = encode(Kind::Data(Data::Request), Revision::R3, 5, &[0x1234_5678]);
	let message = decode(&bytes).expect("a request");
	assert_eq!((message.kind, message.revision, message.id, message.objects.as_slice()), (Kind::Data(Data::Request), Revision::R3, 5, [0x1234_5678].as_slice()));
	// An object count past the byte count.
	let mut short = bytes.clone();
	short.pop();
	assert_eq!(decode(&short), Err(Refusal::Length));
	// A reserved data message type.
	let mut reserved = encode(Kind::Data(Data::Request), Revision::R3, 0, &[1]);
	reserved[0] = (reserved[0] & !0x1F) | 13;
	assert_eq!(decode(&reserved), Err(Refusal::Reserved));
	// An extended message.
	let mut extended = encode(Kind::Control(Control::Accept), Revision::R3, 0, &[]);
	extended[1] |= 0x80;
	assert_eq!(decode(&extended), Err(Refusal::Extended));
	// A reserved revision.
	let mut revision = encode(Kind::Control(Control::Accept), Revision::R3, 0, &[]);
	revision[0] |= 0xC0;
	assert_eq!(decode(&revision), Err(Refusal::Reserved));
	assert_eq!(decode(&[0x01]), Err(Refusal::Length));
}

// ------------------------------------------------------------------ invariants 1 and 2: the selection

#[test]
fn a_board_describing_5_and_15_volts_takes_15_volts_at_its_own_current_and_never_9_12_20_or_pps() {
	let chosen = select(&board(), &charger()).expect("an offer");
	assert_eq!((chosen.position, chosen.millivolts, chosen.milliamps, chosen.mismatch), (4, 15_000, 2_000, false), "15 V at the sink PDO's 2 A, not the charger's 3 A");
	assert_eq!(chosen.request() >> 28, 4);
	assert_eq!(chosen.request() & 0x3FF, 200, "operating and maximum current are the limited current");
	assert_eq!((chosen.request() >> 10) & 0x3FF, 200);
	// Without the 15 V offer, 9 and 12 V are still never taken: 5 V at 3 A, which is 15 W exactly.
	let without = vec![fixed(5_000, 3_000), fixed(9_000, 3_000), fixed(12_000, 3_000), pps(21_000, 3_300, 3_000)];
	let fallback = select(&board(), &without).expect("position 1");
	assert_eq!((fallback.position, fallback.millivolts, fallback.milliamps, fallback.mismatch), (1, 5_000, 3_000, false));
	// And from a 2 A charger, 10 W misses 15 W: position 1 with capability mismatch.
	let weak = vec![fixed(5_000, 2_000), fixed(9_000, 2_000), fixed(12_000, 1_500)];
	let fallback = select(&board(), &weak).expect("position 1");
	assert_eq!((fallback.position, fallback.millivolts, fallback.milliamps, fallback.mismatch), (1, 5_000, 2_000, true));
	assert_eq!(fallback.request() >> 26 & 1, 1, "capability mismatch set");
}

#[test]
fn variable_and_battery_sink_pdos_admit_a_range_and_ties_go_to_the_lower_voltage() {
	let variable = Sink { pdos: vec![SinkPdo::Fixed { millivolts: 5_000, milliamps: 3_000 }, SinkPdo::Variable { min_millivolts: 9_000, max_millivolts: 12_000, milliamps: 1_000 }], operational_microwatts: 1 };
	let chosen = select(&variable, &charger()).expect("an offer");
	assert_eq!((chosen.millivolts, chosen.milliamps), (5_000, 3_000), "15 W at 5 V beats 12 W at 12 V");
	let battery = Sink { pdos: vec![SinkPdo::Fixed { millivolts: 5_000, milliamps: 1_000 }, SinkPdo::Battery { min_millivolts: 9_000, max_millivolts: 20_000, milliwatts: 27_000 }], operational_microwatts: 1 };
	let chosen = select(&battery, &charger()).expect("an offer");
	// 27 W: 9 V at 3 A is 27 W, 12 V at 2.25 A is 27 W, 15 V at 1.8 A is 27 W - the tie goes to 9 V.
	assert_eq!((chosen.millivolts, chosen.milliamps), (9_000, 3_000));
	assert_eq!(select(&battery, &[pps(21_000, 3_300, 3_000)]), None, "an augmented offer alone is nothing to request");
}

#[test]
fn a_capability_mismatch_falls_back_only_to_a_fixed_5v_first_offer() {
	let sink = Sink { operational_microwatts: 50_000_000, ..board() };
	assert_eq!(select(&sink, &[fixed(15_000, 3_000), fixed(5_000, 3_000)]), None, "a malformed first offer is not the 5 V fallback, even when the board admits its voltage");
	assert_eq!(select(&sink, &[]), None);
	let fallback = select(&sink, &[fixed(5_000, 5_000), fixed(15_000, 3_000)]).expect("5 V at position 1");
	assert_eq!(fallback, Selection { position: 1, millivolts: 5_000, milliamps: 3_000, mismatch: true });
}

// ------------------------------------------------------------------ the engine under a virtual clock

// A test harness around the engine: the actions accumulated, the timers armed, the sink path and alarms as set.
struct Run {
	engine: Engine,
	actions: Vec<Action>,
	sent: Vec<Message>,
	tx_id_from_source: u8,
}

impl Run {
	fn new() -> Run {
		Run { engine: Engine::new(board()), actions: Vec::new(), sent: Vec::new(), tx_id_from_source: 0 }
	}

	fn event(&mut self, event: Event) -> Vec<Action> {
		let actions = self.engine.event(event);
		for action in &actions {
			if let Action::Transmit(bytes) = action {
				self.sent.push(decode(bytes).expect("the engine sends only what decodes"));
			}
		}
		self.actions.extend(actions.clone());
		actions
	}

	fn from_source(&mut self, kind: Kind, objects: &[u32]) -> Vec<Action> {
		let id = self.tx_id_from_source;
		self.tx_id_from_source = (id + 1) % 8;
		self.event(Event::Received(source_message(kind, id, objects, Revision::R3)))
	}

	fn attach(&mut self) {
		self.event(Event::Bound { cc: Cc::Open, vbus: false, sinking: false });
		self.event(Event::Cc(Cc::Rp(Rp::High)));
		self.event(Event::Vbus(true));
		self.event(Event::Timer(Timer::CcDebounce));
	}

	fn contract_at_15v(&mut self) {
		self.from_source(Kind::Data(Data::SourceCapabilities), &charger());
		self.event(Event::Transmitted(Transmitted::Success));
		self.from_source(Kind::Control(Control::Accept), &[]);
		self.from_source(Kind::Control(Control::PsRdy), &[]);
	}

	fn last_sent(&self) -> &Message {
		self.sent.last().expect("something was sent")
	}
}

fn position_of(actions: &[Action], wanted: &Action) -> Option<usize> {
	actions.iter().position(|action| action == wanted)
}

#[test]
fn attach_sinks_only_with_vbus_after_the_debounce_and_the_contract_is_made_at_15_volts() {
	let mut run = Run::new();
	run.event(Event::Bound { cc: Cc::Open, vbus: false, sinking: false });
	let actions = run.event(Event::Cc(Cc::Rp(Rp::High)));
	assert!(!actions.contains(&Action::SinkPath(true)), "Rp alone attaches nothing");
	assert!(actions.contains(&Action::Arm(Timer::CcDebounce)));
	let actions = run.event(Event::Timer(Timer::CcDebounce));
	assert!(!actions.contains(&Action::SinkPath(true)), "debounced but no VBUS: not Attached");
	let actions = run.event(Event::Vbus(true));
	assert!(actions.contains(&Action::SinkPath(true)), "Attached with VBUS present: the sink path");
	assert!(actions.contains(&Action::Alarms(Some(window(SAFE_5V)))), "vSafe5V's range from attach");
	run.contract_at_15v();
	let report = run.engine.report();
	let (selection, object) = report.contract.expect("PS_RDY made it");
	assert_eq!((selection.millivolts, object, report.in_transition, report.pd), (15_000, fixed(15_000, 3_000), false, true));
	assert_eq!(run.actions.last(), Some(&Action::Publish));
	assert!(run.actions.contains(&Action::Alarms(Some(window(15_000)))), "narrowed to the contract's window at PS_RDY");
}

#[test]
fn no_contract_above_vsafe5v_is_reported_before_ps_rdy_and_the_alarms_span_the_transition() {
	let mut run = Run::new();
	run.attach();
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	assert_eq!(run.last_sent().kind, Kind::Data(Data::Request));
	run.event(Event::Transmitted(Transmitted::Success));
	assert_eq!(run.engine.report().contract, None, "requested is not made");
	let actions = run.from_source(Kind::Control(Control::Accept), &[]);
	let report = run.engine.report();
	assert!(report.in_transition, "between Accept and PS_RDY the contract is in transition");
	assert!(actions.contains(&Action::Alarms(Some((window(SAFE_5V).0, window(15_000).1)))), "the span of the old window and the new");
	assert!(actions.contains(&Action::Arm(Timer::PsTransition)));
}

#[test]
fn a_new_capabilities_voids_the_old_offers_and_is_requested_from_again() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	// Less power: only 5 and 9 V now.
	run.from_source(Kind::Data(Data::SourceCapabilities), &[fixed(5_000, 3_000), fixed(9_000, 3_000)]);
	let request = run.last_sent().clone();
	assert_eq!(request.kind, Kind::Data(Data::Request));
	assert_eq!(request.objects[0] >> 28, 1, "position 1 of the NEW capabilities - 9 V is not described");
	assert_eq!(run.engine.report().offers.len(), 2);
}

#[test]
fn detach_hard_reset_and_an_alarm_turn_the_sink_path_off_and_void_the_contract_first() {
	for trigger in [Event::Timer(Timer::PdDebounce), Event::HardResetReceived, Event::Alarm] {
		let mut run = Run::new();
		run.attach();
		run.contract_at_15v();
		if matches!(trigger, Event::Timer(Timer::PdDebounce)) {
			run.event(Event::Cc(Cc::Open));
		}
		let actions = run.event(trigger.clone());
		assert_eq!(actions.first(), Some(&Action::SinkPath(false)), "{trigger:?}: the sink path off before anything else");
		let off = position_of(&actions, &Action::SinkPath(false)).expect("off");
		for later in [Action::HardReset, Action::Publish] {
			if let Some(at) = position_of(&actions, &later) {
				assert!(at > off, "{trigger:?}: {later:?} before the sink path was off");
			}
		}
		assert_eq!(run.engine.report().contract, None, "{trigger:?}: the contract is void");
	}
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	run.event(Event::Alarm);
	assert!(run.engine.report().fault, "a VBUS alarm reports a source fault");
	assert!(run.actions.contains(&Action::HardReset));
	run.event(Event::Vbus(false));
	run.event(Event::Vbus(true));
	assert!(run.engine.report().fault, "still, through the hard reset");
	run.contract_at_15v();
	assert!(!run.engine.report().fault, "a contract made anew ends it");
}

#[test]
fn an_alarm_while_cc_is_open_is_the_detach_and_no_fault() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	let actions = run.event(Event::Cc(Cc::Open));
	assert!(actions.contains(&Action::Arm(Timer::PdDebounce)));
	let actions = run.event(Event::Alarm);
	assert_eq!(actions.first(), Some(&Action::SinkPath(false)), "the sink path off first");
	assert!(!actions.contains(&Action::HardReset), "no hard reset to a partner that is going");
	let report = run.engine.report();
	assert_eq!((report.attached, report.fault, report.contract), (false, false, None));
}

#[test]
fn a_bind_with_vbus_present_trusts_no_contract_and_negotiates_anew_through_soft_reset() {
	let mut run = Run::new();
	let actions = run.event(Event::Bound { cc: Cc::Rp(Rp::High), vbus: true, sinking: true });
	assert!(!actions.iter().any(|action| matches!(action, Action::SinkPath(_))), "the sink path is left as the registers show it");
	assert!(actions.contains(&Action::Alarms(Some((window(SAFE_5V).0, window(15_000).1)))), "vSafe5V's lower bound to the highest described sink voltage's upper");
	assert_eq!(run.last_sent().kind, Kind::Control(Control::SoftReset));
	assert_eq!(run.engine.report().contract, None, "no contract it did not see made");
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Control(Control::Accept), &[]);
	run.contract_at_15v();
	assert!(run.engine.report().contract.is_some());
	assert!(run.actions.contains(&Action::Alarms(Some(window(15_000)))), "the envelope holds until the first PS_RDY");
}

#[test]
fn a_hard_reset_with_vbus_cycled_keeps_the_partner_and_its_count_and_makes_the_contract_anew() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	let actions = run.event(Event::HardResetReceived);
	assert!(actions.contains(&Action::AutoDischarge(false)), "the automatic discharge held off");
	assert!(actions.contains(&Action::Alarms(None)), "the alarms off from the hard reset");
	assert!(run.engine.report().attached, "the partner stays attached");
	run.event(Event::Vbus(false));
	assert!(run.engine.report().attached, "VBUS at vSafe0V inside a hard reset is no detach");
	let actions = run.event(Event::Vbus(true));
	assert!(actions.contains(&Action::SinkPath(true)) && actions.contains(&Action::Alarms(Some(window(SAFE_5V)))), "back at vSafe5V: sinking, the alarms on");
	run.contract_at_15v();
	assert!(run.engine.report().contract.is_some());
}

#[test]
fn a_hard_reset_after_which_vbus_stays_off_detaches_once_its_return_bound_passes() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	run.event(Event::HardResetReceived);
	let actions = run.event(Event::Vbus(false));
	assert!(actions.contains(&Action::Arm(Timer::SrcRecover)));
	assert!(run.engine.report().attached);
	run.event(Event::Timer(Timer::SrcRecover));
	assert!(!run.engine.report().attached, "detached once tSrcRecover and tSrcTurnOn have passed");
}

#[test]
fn no_capabilities_sends_hard_resets_up_to_the_count_then_the_typec_current_stands() {
	let mut run = Run::new();
	run.attach();
	for sent in 1..=HARD_RESET_COUNT {
		run.event(Event::Timer(Timer::SinkWaitCap));
		assert_eq!(run.engine.report().hard_resets, sent);
		run.event(Event::Timer(Timer::Safe0V));
	}
	let before = run.actions.iter().filter(|action| **action == Action::HardReset).count();
	run.event(Event::Timer(Timer::SinkWaitCap));
	assert_eq!(run.actions.iter().filter(|action| **action == Action::HardReset).count(), before, "no third hard reset");
	let report = run.engine.report();
	assert_eq!((report.attached, report.pd, report.typec_current), (true, false, Some(Rp::High)), "the Type-C current");
}

#[test]
fn accept_without_ps_rdy_and_a_request_unanswered_each_end_in_a_hard_reset() {
	let mut run = Run::new();
	run.attach();
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	run.event(Event::Transmitted(Transmitted::Success));
	assert!(run.actions.contains(&Action::Arm(Timer::SenderResponse)), "the answer is timed from GoodCRC");
	run.event(Event::Timer(Timer::SenderResponse));
	assert!(run.actions.contains(&Action::HardReset));
	let mut run = Run::new();
	run.attach();
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Control(Control::Accept), &[]);
	run.event(Event::Timer(Timer::PsTransition));
	assert!(run.actions.contains(&Action::HardReset));
}

#[test]
fn wait_then_accept_and_reject_leave_the_contract_as_it_was() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	run.event(Event::Transmitted(Transmitted::Success));
	let actions = run.from_source(Kind::Control(Control::Wait), &[]);
	assert!(actions.contains(&Action::Arm(Timer::SinkRequest)), "asked again after tSinkRequest");
	assert!(run.engine.report().contract.is_some(), "the contract stands through a Wait");
	run.event(Event::Timer(Timer::SinkRequest));
	assert_eq!(run.last_sent().kind, Kind::Data(Data::Request));
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Control(Control::Accept), &[]);
	run.from_source(Kind::Control(Control::PsRdy), &[]);
	assert!(run.engine.report().contract.is_some());
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Control(Control::Reject), &[]);
	assert_eq!(run.engine.report().contract.map(|(selection, _)| selection.millivolts), Some(15_000), "rejected, the contract stands");
}

#[test]
fn swaps_and_vdms_are_not_supported_from_3_and_rejected_or_ignored_from_2() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	for swap in [Control::PrSwap, Control::DrSwap, Control::VconnSwap] {
		run.from_source(Kind::Control(swap), &[]);
		assert_eq!(run.last_sent().kind, Kind::Control(Control::NotSupported));
	}
	run.from_source(Kind::Data(Data::VendorDefined), &[0xFF00_8001]);
	assert_eq!(run.last_sent().kind, Kind::Control(Control::NotSupported), "Discover_Identity: not supported");
	assert!(run.engine.report().contract.is_some(), "the contract kept");
	let mut run = Run::new();
	run.attach();
	let two = |kind, id, objects: &[u32]| Event::Received(source_message(kind, id, objects, Revision::R2));
	run.event(two(Kind::Data(Data::SourceCapabilities), 0, &charger()));
	assert_eq!(run.last_sent().revision, Revision::R2, "a 2.0 partner is spoken to in 2.0");
	let sent = run.sent.len();
	run.event(two(Kind::Data(Data::VendorDefined), 1, &[0xFF00_8001]));
	assert_eq!(run.sent.len(), sent, "a VDM from 2.0 is ignored");
	run.event(two(Kind::Control(Control::DrSwap), 2, &[]));
	assert_eq!(run.last_sent().kind, Kind::Control(Control::Reject), "a swap from 2.0 is rejected");
}

#[test]
fn message_ids_count_on_from_a_soft_reset_and_never_repeat_one_the_source_stored() {
	// A BIND'S Soft_Reset goes with the counter reset, and the Request after its Accept is the next ID - the source
	// stored the Soft_Reset's and discards the same again as a retry.
	let mut run = Run::new();
	run.event(Event::Bound { cc: Cc::Rp(Rp::High), vbus: true, sinking: true });
	assert_eq!((run.last_sent().kind, run.last_sent().id), (Kind::Control(Control::SoftReset), 0));
	run.event(Event::Transmitted(Transmitted::Success));
	run.tx_id_from_source = 0;
	run.from_source(Kind::Control(Control::Accept), &[]);
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	assert_eq!((run.last_sent().kind, run.last_sent().id), (Kind::Data(Data::Request), 1));
	// A Reject with no contract awaits capabilities again, and resets nothing.
	let mut run = Run::new();
	run.attach();
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	assert_eq!(run.last_sent().id, 0, "the first message after attach");
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Control(Control::Reject), &[]);
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	assert_eq!((run.last_sent().kind, run.last_sent().id), (Kind::Data(Data::Request), 1));
	// A SOFT RESET RECEIVED resets this sink's counter: its Accept goes as 0, the Request after as 1.
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	run.tx_id_from_source = 0;
	run.from_source(Kind::Control(Control::SoftReset), &[]);
	assert_eq!((run.last_sent().kind, run.last_sent().id), (Kind::Control(Control::Accept), 0));
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	assert_eq!((run.last_sent().kind, run.last_sent().id), (Kind::Data(Data::Request), 1));
}

#[test]
fn a_repeated_message_id_is_discarded_and_a_malformed_message_changes_nothing() {
	let mut run = Run::new();
	run.attach();
	let caps = source_message(Kind::Data(Data::SourceCapabilities), 3, &charger(), Revision::R3);
	run.event(Event::Received(caps.clone()));
	let sent = run.sent.len();
	run.event(Event::Received(caps));
	assert_eq!(run.sent.len(), sent, "the retry of a message already taken");
	let before = run.engine.report();
	assert!(run.event(Event::Malformed).is_empty());
	assert_eq!(run.engine.report(), before);
}

#[test]
fn get_sink_cap_is_answered_with_the_described_sink_pdos() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	run.from_source(Kind::Control(Control::GetSinkCap), &[]);
	assert_eq!(run.last_sent().kind, Kind::Data(Data::SinkCapabilities));
	assert_eq!(run.last_sent().objects, vec![fixed(5_000, 3_000), fixed(15_000, 2_000)]);
}

#[test]
fn a_board_without_a_description_sinks_at_the_type_c_current_and_speaks_no_power_delivery() {
	let mut run = Run { engine: Engine::new(Sink { pdos: Vec::new(), operational_microwatts: 0 }), actions: Vec::new(), sent: Vec::new(), tx_id_from_source: 0 };
	run.attach();
	assert!(run.actions.contains(&Action::SinkPath(true)));
	assert!(!run.actions.contains(&Action::Receive(true)), "no message is received");
	assert!(!run.actions.contains(&Action::Arm(Timer::SinkWaitCap)), "no capabilities are awaited");
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	assert!(run.sent.is_empty(), "nothing is ever sent");
	let report = run.engine.report();
	assert_eq!((report.attached, report.pd, report.typec_current, report.contract), (true, false, Some(Rp::High), None));
	// AN ALARM CANNOT BE ANSWERED WITH A HARD RESET: the sink path off first, then error recovery.
	let actions = run.event(Event::Alarm);
	assert_eq!(actions.first(), Some(&Action::SinkPath(false)));
	assert!(actions.contains(&Action::CcOpen(true)) && !actions.contains(&Action::HardReset));
	assert!(run.engine.report().fault);
	// A BIND ON A POWERED CONNECTOR: a path found on stays on; one found off goes through error recovery.
	let mut on = Engine::new(Sink { pdos: Vec::new(), operational_microwatts: 0 });
	let actions = on.event(Event::Bound { cc: Cc::Rp(Rp::Medium), vbus: true, sinking: true });
	assert!(!actions.iter().any(|action| matches!(action, Action::SinkPath(_) | Action::Transmit(_) | Action::CcOpen(_))), "{actions:?}");
	assert!(on.report().attached);
	let mut off = Engine::new(Sink { pdos: Vec::new(), operational_microwatts: 0 });
	let actions = off.event(Event::Bound { cc: Cc::Rp(Rp::Medium), vbus: true, sinking: false });
	assert!(actions.contains(&Action::CcOpen(true)), "{actions:?}");
	let actions = off.event(Event::Timer(Timer::ErrorRecovery));
	assert!(actions.contains(&Action::CcOpen(false)) && actions.contains(&Action::Arm(Timer::CcDebounce)), "Rp still there: AttachWait at once - {actions:?}");
}

#[test]
fn messages_are_received_only_while_attached_and_the_orientation_comes_with_the_attach() {
	let mut run = Run::new();
	run.event(Event::Bound { cc: Cc::Open, vbus: false, sinking: false });
	run.event(Event::Cc(Cc::Rp(Rp::High)));
	run.event(Event::Timer(Timer::CcDebounce));
	assert!(!run.actions.contains(&Action::Receive(true)), "not before Attached");
	let actions = run.event(Event::Vbus(true));
	let on = position_of(&actions, &Action::Receive(true)).expect("on at attach");
	assert!(on > position_of(&actions, &Action::SinkPath(true)).expect("sinking"));
	run.contract_at_15v();
	for trigger in [Event::HardResetReceived, Event::Alarm] {
		let mut again = Run::new();
		again.attach();
		again.contract_at_15v();
		let actions = again.event(trigger.clone());
		assert!(actions.contains(&Action::Receive(false)), "{trigger:?}: off");
	}
	let actions = run.event(Event::Vbus(false));
	assert!(actions.contains(&Action::Receive(false)), "off at detach");
}

#[test]
fn vbus_lost_with_rp_still_on_cc_detaches_and_waits_to_attach_again() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	let actions = run.event(Event::Vbus(false));
	assert_eq!(actions.first(), Some(&Action::SinkPath(false)));
	assert!(!run.engine.report().attached);
	assert!(actions.contains(&Action::Arm(Timer::CcDebounce)), "AttachWait at once: the Rp is still there");
	let actions = run.event(Event::Timer(Timer::CcDebounce));
	assert!(!actions.contains(&Action::SinkPath(true)), "no VBUS: not Attached");
	let actions = run.event(Event::Vbus(true));
	assert!(actions.contains(&Action::SinkPath(true)));
}

#[test]
fn a_bind_that_found_the_path_off_sinks_once_the_contract_it_saw_made_stands() {
	let mut run = Run::new();
	run.event(Event::Bound { cc: Cc::Rp(Rp::High), vbus: true, sinking: false });
	assert!(!run.actions.contains(&Action::SinkPath(true)), "not at bind");
	run.event(Event::Transmitted(Transmitted::Success));
	run.from_source(Kind::Control(Control::Accept), &[]);
	run.from_source(Kind::Data(Data::SourceCapabilities), &charger());
	run.event(Event::Transmitted(Transmitted::Success));
	let actions = run.from_source(Kind::Control(Control::Accept), &[]);
	assert!(!actions.contains(&Action::SinkPath(true)), "not in transition");
	let actions = run.from_source(Kind::Control(Control::PsRdy), &[]);
	let narrowed = position_of(&actions, &Action::Alarms(Some(window(15_000)))).expect("narrowed");
	let on = position_of(&actions, &Action::SinkPath(true)).expect("on at PS_RDY");
	assert!(narrowed < on, "the alarms bound the contract before the path is on");
}

#[test]
fn a_message_that_expected_no_answer_and_went_unacknowledged_resets_the_protocol_softly() {
	let mut run = Run::new();
	run.attach();
	run.contract_at_15v();
	run.from_source(Kind::Control(Control::GetSinkCap), &[]);
	run.event(Event::Transmitted(Transmitted::Failed));
	assert_eq!(run.last_sent().kind, Kind::Control(Control::SoftReset));
	assert!(!run.actions.contains(&Action::HardReset));
	run.event(Event::Transmitted(Transmitted::Failed));
	assert!(run.actions.contains(&Action::HardReset), "the Soft_Reset unacknowledged: a hard reset");
}

#[test]
fn every_timer_is_armed_no_earlier_than_its_minimum_and_only_the_sender_response_timer_runs_past_its_maximum() {
	for timer in [
		Timer::CcDebounce,
		Timer::PdDebounce,
		Timer::SinkWaitCap,
		Timer::SenderResponse,
		Timer::PsTransition,
		Timer::SinkRequest,
		Timer::Safe0V,
		Timer::SrcRecover,
		Timer::ErrorRecovery,
	] {
		let (minimum, _) = timer.bounds();
		// The +1 tick: armed mid-tick, it still waits a whole minimum.
		assert!((timer.ticks() - 1) * TICK_MS >= minimum, "{timer:?} could expire before {minimum} ms");
		assert_eq!(timer.late(), timer == Timer::SenderResponse, "{timer:?}");
	}
	assert_eq!(Timer::SenderResponse.ticks(), 4, "24 ms: fires at 30 to 40 ms");
	assert_eq!(Timer::SinkWaitCap.ticks(), 32, "310 ms");
	assert_eq!(Timer::PsTransition.ticks(), 46, "450 ms");
}

#[test]
fn the_port_controller_s_registers_decode() {
	assert_eq!(tcpci::cc(0b0000_1100), Cc::Rp(Rp::High));
	assert_eq!(tcpci::cc(0b0000_0001), Cc::Rp(Rp::Default));
	assert_eq!(tcpci::cc(0), Cc::Open);
	assert_eq!((tcpci::flipped(0b0000_0011), tcpci::flipped(0b0000_1000), tcpci::flipped(0)), (Some(false), Some(true), None));
	assert_eq!(tcpci::vbus_millivolts(200), 5_000);
	assert_eq!(tcpci::vbus_millivolts(1 << 10 | 300), 15_000, "scaled by two");
	assert_eq!(tcpci::alarm_threshold(15_000), 600);
	let message = encode(Kind::Control(Control::Accept), Revision::R3, 1, &[]);
	let mut buffer = vec![3u8, 0];
	buffer.extend_from_slice(&message);
	assert_eq!(tcpci::received(&buffer), Some(message.as_slice()));
	assert_eq!(tcpci::received(&[9, 0, 1, 2]), None, "a count past what was read");
	assert_eq!(tcpci::received(&[3, 1, 0, 0]), None, "not SOP");
	assert_eq!(tcpci::transmit_buffer(&message), vec![2, message[0], message[1]]);
}
