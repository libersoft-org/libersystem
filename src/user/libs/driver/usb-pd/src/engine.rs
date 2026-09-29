//! THE TYPE-C SINK AND THE SINK POLICY ENGINE, as one transition function: an `Event` the port controller or the clock
//! produced goes in, the `Action`s the driver performs - in order - come out. USB Power Delivery 3.1 in the Standard
//! Power Range, revision 2.0 partners accepted, messages on SOP only.
//!
//! THE TYPE-C SINK: Unattached, AttachWait once Rp appears on CC, Attached once Rp has been stable for tCCDebounce with
//! VBUS present; detached on CC open for tPDDebounce, and on VBUS removal OUTSIDE A HARD RESET only. From a Hard Reset,
//! sent or received, the controller's automatic discharge is held off and VBUS is expected to fall within tSafe0V (some
//! sources keep it up) and to return within tSrcRecover and tSrcTurnOn; the engine detaches only if it does not return,
//! the partner stays attached throughout, and the hard-reset count survives the reset - cleared only by a contract made
//! or a detach.
//!
//! THE POLICY: Source_Capabilities evaluated (the `pdo` selection rule), Request, then Accept, Reject or Wait, then
//! PS_RDY; Get_Sink_Cap answered with the described sink PDOs; Soft_Reset and Hard_Reset in both directions; every
//! swap, every structured VDM and every other request answered `Not_Supported` - from a revision 2.0 partner a swap is
//! answered `Reject` and a VDM ignored.
//!
//! THE SAFETY INVARIANTS, each held here and each a host test:
//!   1-2. A request names a FIXED offer at a voltage inside one described sink PDO, its currents within both (`pdo`).
//!   3.   A request names a position of the LATEST Source_Capabilities; a new one voids every earlier offer.
//!   4.   No contract above vSafe5V is reported until its PS_RDY; between Accept and PS_RDY it is reported in transition.
//!   5.   The sink path is enabled only in Attached with VBUS present - a bind leaves it as it finds it - and on detach,
//!        Hard Reset, error recovery or a VBUS alarm it is disabled and the contract voided FIRST, before any message is
//!        sent or any state published; the next contract is negotiated from nothing.
//!   6.   The VBUS alarms bound VBUS to what the contract allows: vSafe5V's range from attach; from Accept to PS_RDY the
//!        span of the old and the new, narrowed at PS_RDY; after a bind that found VBUS present and trusts no contract,
//!        vSafe5V's lower bound to the highest described sink voltage's upper bound, until the first PS_RDY; off from a
//!        Hard Reset until VBUS is back. VBUS outside them disables the sink path, sends Hard Reset and reports a fault.
//!   7.   A message that fails the codec is not believed, and nothing is inferred from it.

use alloc::vec::Vec;

use crate::message::{Control, Data, Kind, Message, Revision, encode};
use crate::pdo::{Offer, Selection, Sink, select};
use crate::timer::{HARD_RESET_COUNT, Timer};

/// The current a source advertises on CC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rp {
	Default,
	/// 1.5 A.
	Medium,
	/// 3 A.
	High,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cc {
	Open,
	Rp(Rp),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transmitted {
	/// GoodCRC came back.
	Success,
	/// No GoodCRC after the controller's retries.
	Failed,
	/// A message arrived first, and the transmission was dropped.
	Discarded,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
	/// The binding started, with the controller's registers as it found them - the sink path among them.
	Bound {
		cc: Cc,
		vbus: bool,
		sinking: bool,
	},
	Cc(Cc),
	Vbus(bool),
	Received(Message),
	/// A message that failed the codec.
	Malformed,
	HardResetReceived,
	Transmitted(Transmitted),
	/// VBUS outside the alarm window.
	Alarm,
	Timer(Timer),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
	/// The sink path, through the controller's SinkVbus and DisableSinkVbus commands.
	SinkPath(bool),
	/// A message on SOP, encoded.
	Transmit(Vec<u8>),
	HardReset,
	/// The VBUS alarm window, in millivolts; `None` turns the alarms off.
	Alarms(Option<(u32, u32)>),
	AutoDischarge(bool),
	Arm(Timer),
	Cancel(Timer),
	/// CC held open (error recovery), or back to Rd.
	CcOpen(bool),
	/// Messages on SOP and hard resets received - with the plug's orientation - or not: on only while Attached with
	/// Power Delivery running.
	Receive(bool),
	/// Something a reader sees changed.
	Publish,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attach {
	Unattached,
	AttachWait { debounced: bool },
	Attached,
	HardReset { fell: bool },
	ErrorRecovery,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Policy {
	/// No Power Delivery: the Type-C current only.
	Idle,
	WaitCaps,
	/// A Request sent, its answer awaited.
	Select(Selection),
	/// Accepted; PS_RDY awaited.
	Transition(Selection),
	Ready,
	/// This sink's Soft_Reset sent, its Accept awaited.
	SoftReset,
}

/// What the engine reports: the driver builds its connector record and its supply from it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
	pub attached: bool,
	pub typec_current: Option<Rp>,
	/// Power Delivery spoken: Source_Capabilities received since attach.
	pub pd: bool,
	pub revision: Revision,
	pub offers: Vec<u32>,
	pub contract: Option<(Selection, u32)>,
	pub in_transition: bool,
	pub hard_resets: u8,
	pub fault: bool,
	pub sinking: bool,
	/// Inside a hard reset: VBUS awaited to fall and to come back, the partner still attached.
	pub resetting: bool,
}

pub struct Engine {
	sink: Sink,
	// A board that describes no sink PDOs runs no Power Delivery: the Type-C current only.
	runs_pd: bool,
	attach: Attach,
	policy: Policy,
	cc: Cc,
	vbus: bool,
	sinking: bool,
	offers: Vec<u32>,
	contract: Option<Selection>,
	contract_object: u32,
	revision: Revision,
	tx_id: u8,
	rx_id: Option<u8>,
	hard_resets: u8,
	// A contract seen made since bind: until then, a bind's VBUS is trusted no further than its envelope.
	trusted: bool,
	fault: bool,
	pd: bool,
	// The message awaiting GoodCRC is one whose answer is timed.
	awaiting_answer: bool,
	window: Option<(u32, u32)>,
}

/// vSafe5V.
pub const SAFE_5V: u32 = 5_000;

/// A contract's window: a source's output within 5% of its voltage, and half a volt more each way for the cable and
/// the measurement.
pub fn window(millivolts: u32) -> (u32, u32) {
	((millivolts * 95 / 100).saturating_sub(500), millivolts * 105 / 100 + 500)
}

fn span(a: (u32, u32), b: (u32, u32)) -> (u32, u32) {
	(a.0.min(b.0), a.1.max(b.1))
}

impl Engine {
	pub fn new(sink: Sink) -> Engine {
		let runs_pd = !sink.pdos.is_empty();
		Engine { sink, runs_pd, attach: Attach::Unattached, policy: Policy::Idle, cc: Cc::Open, vbus: false, sinking: false, offers: Vec::new(), contract: None, contract_object: 0, revision: Revision::R3, tx_id: 0, rx_id: None, hard_resets: 0, trusted: true, fault: false, pd: false, awaiting_answer: false, window: None }
	}

	pub fn report(&self) -> Report {
		let attached = matches!(self.attach, Attach::Attached | Attach::HardReset { .. });
		let (contract, in_transition) = match self.policy {
			Policy::Transition(selection) => (Some((selection, self.offers.get(usize::from(selection.position) - 1).copied().unwrap_or(0))), true),
			_ => (self.contract.map(|selection| (selection, self.contract_object)), false),
		};
		Report {
			attached,
			typec_current: match self.cc {
				Cc::Rp(rp) if attached => Some(rp),
				_ => None,
			},
			pd: self.pd && attached,
			revision: self.revision,
			offers: if attached { self.offers.clone() } else { Vec::new() },
			contract,
			in_transition,
			hard_resets: self.hard_resets,
			fault: self.fault,
			sinking: self.sinking,
			resetting: matches!(self.attach, Attach::HardReset { .. }),
		}
	}

	fn alarms(&mut self, window: Option<(u32, u32)>, out: &mut Vec<Action>) {
		if self.window != window {
			self.window = window;
			out.push(Action::Alarms(window));
		}
	}

	fn sink_path(&mut self, on: bool, out: &mut Vec<Action>) {
		if self.sinking != on {
			self.sinking = on;
			out.push(Action::SinkPath(on));
		}
	}

	// THE SINK PATH OFF AND THE CONTRACT VOID, FIRST.
	fn stop_sinking(&mut self, out: &mut Vec<Action>) {
		self.sink_path(false, out);
		self.contract = None;
		self.contract_object = 0;
		if matches!(self.policy, Policy::Transition(_) | Policy::Select(_)) {
			self.policy = Policy::Idle;
		}
	}

	fn transmit(&mut self, kind: Kind, objects: &[u32], answered: bool, out: &mut Vec<Action>) {
		self.awaiting_answer = answered;
		out.push(Action::Transmit(encode(kind, self.revision, self.tx_id, objects)));
	}

	fn cancel_policy_timers(out: &mut Vec<Action>) {
		for timer in [Timer::SinkWaitCap, Timer::SenderResponse, Timer::PsTransition, Timer::SinkRequest] {
			out.push(Action::Cancel(timer));
		}
	}

	// ------------------------------------------------------------------ the Type-C sink

	fn attach(&mut self, out: &mut Vec<Action>) {
		self.attach = Attach::Attached;
		self.sink_path(true, out);
		out.push(Action::AutoDischarge(true));
		self.alarms(Some(window(SAFE_5V)), out);
		if self.runs_pd {
			out.push(Action::Receive(true));
			self.wait_caps(out);
		}
		out.push(Action::Publish);
	}

	fn wait_caps(&mut self, out: &mut Vec<Action>) {
		self.policy = Policy::WaitCaps;
		self.tx_id = 0;
		self.rx_id = None;
		out.push(Action::Arm(Timer::SinkWaitCap));
	}

	fn detach(&mut self, out: &mut Vec<Action>) {
		self.stop_sinking(out);
		self.alarms(None, out);
		out.push(Action::Receive(false));
		Self::cancel_policy_timers(out);
		for timer in [Timer::Safe0V, Timer::SrcRecover, Timer::PdDebounce, Timer::CcDebounce] {
			out.push(Action::Cancel(timer));
		}
		self.attach = Attach::Unattached;
		self.policy = Policy::Idle;
		self.offers.clear();
		self.hard_resets = 0;
		self.fault = false;
		self.pd = false;
		self.trusted = true;
		// VBUS GONE WITH Rp STILL ON CC: Unattached sees the Rp at once, and waits for VBUS again.
		if matches!(self.cc, Cc::Rp(_)) {
			self.attach = Attach::AttachWait { debounced: false };
			out.push(Action::Arm(Timer::CcDebounce));
		}
		out.push(Action::Publish);
	}

	// A HARD RESET, SENT OR RECEIVED: the path off and the contract void first, the alarms off and the discharge held
	// off until VBUS is back, the partner kept attached.
	fn hard_reset(&mut self, send: bool, out: &mut Vec<Action>) {
		self.stop_sinking(out);
		self.alarms(None, out);
		out.push(Action::Receive(false));
		out.push(Action::AutoDischarge(false));
		Self::cancel_policy_timers(out);
		if send {
			self.hard_resets = self.hard_resets.saturating_add(1);
			out.push(Action::HardReset);
		}
		self.policy = Policy::Idle;
		self.offers.clear();
		self.tx_id = 0;
		self.rx_id = None;
		if self.vbus {
			self.attach = Attach::HardReset { fell: false };
			out.push(Action::Arm(Timer::Safe0V));
		} else {
			self.attach = Attach::HardReset { fell: true };
			out.push(Action::Arm(Timer::SrcRecover));
		}
		out.push(Action::Publish);
	}

	// VBUS BACK after a hard reset - or kept up through it: sinking again at vSafe5V, and capabilities awaited.
	fn hard_reset_done(&mut self, out: &mut Vec<Action>) {
		out.push(Action::Cancel(Timer::Safe0V));
		out.push(Action::Cancel(Timer::SrcRecover));
		self.attach = Attach::Attached;
		self.sink_path(true, out);
		out.push(Action::AutoDischarge(true));
		self.alarms(Some(window(SAFE_5V)), out);
		out.push(Action::Receive(true));
		self.wait_caps(out);
		out.push(Action::Publish);
	}

	fn error_recovery(&mut self, out: &mut Vec<Action>) {
		self.stop_sinking(out);
		self.alarms(None, out);
		out.push(Action::Receive(false));
		Self::cancel_policy_timers(out);
		self.attach = Attach::ErrorRecovery;
		self.policy = Policy::Idle;
		self.offers.clear();
		out.push(Action::CcOpen(true));
		out.push(Action::Arm(Timer::ErrorRecovery));
		out.push(Action::Publish);
	}

	// ------------------------------------------------------------------ the policy

	fn request(&mut self, out: &mut Vec<Action>) {
		match select(&self.sink, &self.offers) {
			Some(selection) => {
				self.policy = Policy::Select(selection);
				self.transmit(Kind::Data(Data::Request), &[selection.request()], true, out);
			}
			// Not even a 5 V offer this sink takes: nothing to ask for, the Type-C current stands.
			None => {
				self.policy = Policy::Idle;
				out.push(Action::Publish);
			}
		}
	}

	fn not_supported(&mut self, message: &Message, out: &mut Vec<Action>) {
		let swap = matches!(message.kind, Kind::Control(Control::PrSwap | Control::DrSwap | Control::VconnSwap | Control::FrSwap));
		if message.revision >= Revision::R3 {
			self.transmit(Kind::Control(Control::NotSupported), &[], false, out);
		} else if swap {
			self.transmit(Kind::Control(Control::Reject), &[], false, out);
		}
		// From a revision 2.0 partner, anything else - a VDM among it - is ignored.
	}

	fn received(&mut self, message: Message, out: &mut Vec<Action>) {
		if !self.runs_pd || !matches!(self.attach, Attach::Attached) || !message.from_source {
			return;
		}
		// A REPEATED MESSAGEID IS A RETRY THE CONTROLLER ALREADY ANSWERED: discarded, except a Soft_Reset, which resets
		// the counters it would be compared against.
		let soft_reset = message.kind == Kind::Control(Control::SoftReset);
		if self.rx_id == Some(message.id) && !soft_reset {
			return;
		}
		self.rx_id = Some(message.id);
		self.revision = message.revision.clamp(Revision::R2, Revision::R3);
		match message.kind {
			Kind::Data(Data::SourceCapabilities) => {
				if matches!(self.policy, Policy::Transition(_)) {
					// NEW CAPABILITIES WHILE THE SUPPLY IS MOVING: a protocol error the specification answers with a hard reset.
					self.hard_reset(true, out);
					return;
				}
				// THE LATEST CAPABILITIES VOID EVERY EARLIER OFFER.
				self.offers = message.objects;
				self.pd = true;
				out.push(Action::Cancel(Timer::SinkWaitCap));
				out.push(Action::Cancel(Timer::SinkRequest));
				self.request(out);
				out.push(Action::Publish);
			}
			Kind::Control(Control::Accept) => match self.policy {
				Policy::Select(selection) => {
					out.push(Action::Cancel(Timer::SenderResponse));
					self.policy = Policy::Transition(selection);
					// FROM ACCEPT TO PS_RDY: the span of the old window and the new.
					let old = match self.contract {
						Some(contract) if self.trusted => window(contract.millivolts),
						_ => self.window.unwrap_or(window(SAFE_5V)),
					};
					self.alarms(Some(span(old, window(selection.millivolts))), out);
					out.push(Action::Arm(Timer::PsTransition));
					out.push(Action::Publish);
				}
				Policy::SoftReset => {
					out.push(Action::Cancel(Timer::SenderResponse));
					self.wait_caps(out);
				}
				_ => self.unexpected(out),
			},
			Kind::Control(Control::Reject) | Kind::Control(Control::Wait) if matches!(self.policy, Policy::Select(_)) => {
				out.push(Action::Cancel(Timer::SenderResponse));
				if self.contract.is_some() {
					self.policy = Policy::Ready;
					if message.kind == Kind::Control(Control::Wait) {
						out.push(Action::Arm(Timer::SinkRequest));
					}
				} else {
					self.wait_caps(out);
				}
				out.push(Action::Publish);
			}
			Kind::Control(Control::PsRdy) => match self.policy {
				Policy::Transition(selection) => {
					out.push(Action::Cancel(Timer::PsTransition));
					self.contract = Some(selection);
					self.contract_object = self.offers.get(usize::from(selection.position) - 1).copied().unwrap_or(0);
					self.trusted = true;
					self.hard_resets = 0;
					// A CONTRACT MADE ANEW ENDS A FAULT: the source that tripped the alarm has been reset and answered.
					self.fault = false;
					self.policy = Policy::Ready;
					self.alarms(Some(window(selection.millivolts)), out);
					// A BIND THAT FOUND VBUS WITH THE PATH OFF sinks once a contract it saw made stands.
					if self.vbus {
						self.sink_path(true, out);
					}
					out.push(Action::Publish);
				}
				_ => self.unexpected(out),
			},
			Kind::Control(Control::GetSinkCap) => {
				let objects: Vec<u32> = self.sink.pdos.iter().take(crate::message::MAX_OBJECTS).map(|pdo| pdo.encode()).collect();
				self.transmit(Kind::Data(Data::SinkCapabilities), &objects, false, out);
			}
			Kind::Control(Control::SoftReset) => {
				self.stop_sinking_contract_only();
				self.tx_id = 0;
				self.transmit(Kind::Control(Control::Accept), &[], false, out);
				self.wait_caps(out);
				self.rx_id = Some(message.id);
				out.push(Action::Publish);
			}
			Kind::Control(Control::Ping) | Kind::Control(Control::GoodCrc) => {}
			_ => self.not_supported(&message, out),
		}
	}

	// A SOFT RESET KEEPS THE SUPPLY: the contract is renegotiated, and until it is none is claimed.
	fn stop_sinking_contract_only(&mut self) {
		self.contract = None;
		self.contract_object = 0;
		self.policy = Policy::Idle;
	}

	// A MESSAGE THE STATE DOES NOT EXPECT: during a transition a hard reset, otherwise a soft one.
	fn unexpected(&mut self, out: &mut Vec<Action>) {
		if matches!(self.policy, Policy::Transition(_)) {
			self.hard_reset(true, out);
		} else {
			self.policy = Policy::SoftReset;
			self.tx_id = 0;
			self.transmit(Kind::Control(Control::SoftReset), &[], true, out);
		}
	}

	// ------------------------------------------------------------------ the one entry point

	pub fn event(&mut self, event: Event) -> Vec<Action> {
		let mut out = Vec::new();
		match event {
			Event::Bound { cc, vbus, sinking } => {
				self.cc = cc;
				self.vbus = vbus;
				// AS THE REGISTERS SHOW IT: nothing is written to the path at bind.
				self.sinking = sinking;
				match (cc, vbus) {
					// A BIND THAT FINDS VBUS ALREADY PRESENT trusts no contract it did not see made: the sink path as the
					// registers show it (the driver says), the envelope of every described voltage, and Soft_Reset.
					(Cc::Rp(_), true) if self.runs_pd => {
						self.attach = Attach::Attached;
						self.trusted = false;
						let highest = self.sink.pdos.iter().map(|pdo| pdo.max_millivolts()).max().unwrap_or(SAFE_5V);
						self.alarms(Some((window(SAFE_5V).0, window(highest).1)), &mut out);
						out.push(Action::Receive(true));
						self.policy = Policy::SoftReset;
						self.tx_id = 0;
						self.transmit(Kind::Control(Control::SoftReset), &[], true, &mut out);
						out.push(Action::Publish);
					}
					// WITHOUT POWER DELIVERY nothing can be renegotiated: a path found on is left on at vSafe5V's alarms;
					// one found off - this machine runs from something else - is left off, and error recovery has the
					// source start again from vSafe5V, which the attach that follows sinks.
					(Cc::Rp(_), true) => {
						if sinking {
							self.attach = Attach::Attached;
							self.alarms(Some(window(SAFE_5V)), &mut out);
							out.push(Action::Publish);
						} else {
							self.attach = Attach::Attached;
							self.error_recovery(&mut out);
						}
					}
					(Cc::Rp(_), false) => {
						self.attach = Attach::AttachWait { debounced: false };
						out.push(Action::Arm(Timer::CcDebounce));
					}
					_ => self.attach = Attach::Unattached,
				}
			}
			Event::Cc(cc) => {
				self.cc = cc;
				match (self.attach, cc) {
					(Attach::Unattached, Cc::Rp(_)) => {
						self.attach = Attach::AttachWait { debounced: false };
						out.push(Action::Arm(Timer::CcDebounce));
					}
					(Attach::AttachWait { .. }, Cc::Open) => {
						self.attach = Attach::Unattached;
						out.push(Action::Cancel(Timer::CcDebounce));
					}
					(Attach::Attached | Attach::HardReset { .. }, Cc::Open) => out.push(Action::Arm(Timer::PdDebounce)),
					(Attach::Attached | Attach::HardReset { .. }, Cc::Rp(_)) => {
						out.push(Action::Cancel(Timer::PdDebounce));
						out.push(Action::Publish);
					}
					_ => {}
				}
			}
			Event::Vbus(present) => {
				self.vbus = present;
				match (self.attach, present) {
					(Attach::AttachWait { debounced: true }, true) if matches!(self.cc, Cc::Rp(_)) => self.attach(&mut out),
					(Attach::Attached, false) => self.detach(&mut out),
					(Attach::HardReset { fell: false }, false) => {
						self.attach = Attach::HardReset { fell: true };
						out.push(Action::Cancel(Timer::Safe0V));
						out.push(Action::Arm(Timer::SrcRecover));
					}
					(Attach::HardReset { fell: true }, true) => self.hard_reset_done(&mut out),
					_ => {}
				}
			}
			Event::Received(message) => self.received(message, &mut out),
			// NOT BELIEVED, AND NOTHING INFERRED.
			Event::Malformed => {}
			Event::HardResetReceived => {
				if self.runs_pd && matches!(self.attach, Attach::Attached | Attach::HardReset { .. }) {
					self.hard_reset(false, &mut out);
				}
			}
			Event::Transmitted(result) => match result {
				Transmitted::Success => {
					self.tx_id = (self.tx_id + 1) % 8;
					if self.awaiting_answer {
						self.awaiting_answer = false;
						out.push(Action::Arm(Timer::SenderResponse));
					}
				}
				Transmitted::Failed if matches!(self.attach, Attach::Attached) => {
					if self.awaiting_answer {
						self.awaiting_answer = false;
						self.hard_reset(true, &mut out);
					} else {
						// A MESSAGE THAT EXPECTED NO ANSWER WENT UNACKNOWLEDGED: the protocol is reset softly.
						self.policy = Policy::SoftReset;
						self.tx_id = 0;
						self.transmit(Kind::Control(Control::SoftReset), &[], true, &mut out);
					}
				}
				Transmitted::Failed => {}
				// The message that arrived first drives what happens next.
				Transmitted::Discarded => self.awaiting_answer = false,
			},
			// VBUS LEAVING ITS WINDOW WITH CC ALREADY OPEN is the partner going - its VBUS decays through the low
			// threshold within tPDDebounce - and is the detach, not a fault: the sink path off first, no hard reset.
			Event::Alarm if matches!(self.attach, Attach::Attached) && self.cc == Cc::Open => self.detach(&mut out),
			Event::Alarm => {
				if matches!(self.attach, Attach::Attached) {
					self.fault = true;
					if self.runs_pd && self.hard_resets < HARD_RESET_COUNT {
						self.hard_reset(true, &mut out);
					} else {
						self.error_recovery(&mut out);
					}
				}
			}
			Event::Timer(timer) => self.timer(timer, &mut out),
		}
		out
	}

	fn timer(&mut self, timer: Timer, out: &mut Vec<Action>) {
		match (timer, self.attach, self.policy) {
			(Timer::CcDebounce, Attach::AttachWait { .. }, _) if matches!(self.cc, Cc::Rp(_)) => {
				self.attach = Attach::AttachWait { debounced: true };
				if self.vbus {
					self.attach(out);
				}
			}
			(Timer::PdDebounce, Attach::Attached | Attach::HardReset { .. }, _) if self.cc == Cc::Open => self.detach(out),
			(Timer::SinkWaitCap, Attach::Attached, Policy::WaitCaps) => {
				if self.hard_resets < HARD_RESET_COUNT {
					self.hard_reset(true, out);
				} else {
					// NO POWER DELIVERY AFTER THE HARD-RESET COUNT: the Type-C current stands.
					self.policy = Policy::Idle;
					out.push(Action::Publish);
				}
			}
			(Timer::SenderResponse, Attach::Attached, Policy::Select(_) | Policy::SoftReset) => self.hard_reset(true, out),
			(Timer::PsTransition, Attach::Attached, Policy::Transition(_)) => self.hard_reset(true, out),
			(Timer::SinkRequest, Attach::Attached, Policy::Ready) if !self.offers.is_empty() => self.request(out),
			// VBUS KEPT UP THROUGH THE HARD RESET: some sources do.
			(Timer::Safe0V, Attach::HardReset { fell: false }, _) => self.hard_reset_done(out),
			// VBUS DID NOT COME BACK: the partner is gone.
			(Timer::SrcRecover, Attach::HardReset { fell: true }, _) => self.detach(out),
			(Timer::ErrorRecovery, Attach::ErrorRecovery, _) => {
				out.push(Action::CcOpen(false));
				self.attach = Attach::Unattached;
				self.hard_resets = 0;
				self.fault = false;
				if matches!(self.cc, Cc::Rp(_)) {
					self.attach = Attach::AttachWait { debounced: false };
					out.push(Action::Arm(Timer::CcDebounce));
				}
				out.push(Action::Publish);
			}
			_ => {}
		}
	}

	/// Whether an offer is a fixed one: the only kind requested.
	pub fn fixed(object: u32) -> bool {
		matches!(Offer::decode(object), Offer::Fixed { .. })
	}
}
