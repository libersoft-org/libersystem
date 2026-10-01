// THE DRIVERS' STEP OF THE SUSPEND TRANSACTION - `liber:process@1/device-sleep`, asked by ServiceManager on the
// control channel it holds for this program.
//
// ONE BINDING AT A TIME, AND NEVER IN LINE. `suspend` sends `SUSPEND` to the online bindings in reverse bind order -
// a binding that consumes another's provider before that provider, every binding that publishes a `watchdog` last -
// and asks the next only once the last has answered, each under its entry's `suspend-deadline` scaled by the port's
// boot window. The standing loop keeps serving meanwhile: each answer arrives as a driver frame and is read by
// `step` on the loop's next pass, so the catalogue, the heartbeats of the bindings not yet asked and every teardown
// keep their service. The request is answered, correlated, once the step is over.
//
// A STEP THAT FAILS UNWINDS ITSELF BEFORE IT ANSWERS: a binding that refuses or does not answer within its bound ends
// the asking, and every binding the step had suspended - and the one that did not answer, which may be half way -
// is sent `RESUME`, with no power lost, before the answer names the binding and why. `resume` returns the suspended
// bindings in bind order, every `watchdog` publisher FIRST; a device that did not come back is torn down and bound
// again through the route a crash takes, once the step has ended - while any binding is suspended nothing is bound,
// since a bind reads the volume and the volume's own driver may be one of them.
//
// HIBERNATION, once its snapshot is taken: `resume-for-image` resumes the bindings the image is written through - every
// suspended one that publishes a `block` or a `tpm` provider - with no power lost, since nothing slept, and names those
// that came back (one that did not is bound again after the sleep, as any);
// `suspend-image` suspends them again for a hybrid sleep's S3. Either is answered once its bindings have, and the
// resume that ends the sleep resumes whatever is still suspended. `stop-all`, a restore's last step before memory is
// replaced, stops every binding as a shutdown does.

use super::*;
use alloc::string::String;
use proto::system::{DriversSuspended, Error, SleepState, device_sleep};
use wire::Sink;

// The question outstanding on a node, so an answer nobody asked for is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SleepAsked {
	None,
	Suspend,
	Resume,
}

// What a node answered, for the step to read once the state move is applied.
#[derive(Clone, Copy)]
pub(super) enum SleepAnswer {
	Suspended(driver_protocol::Suspended),
	Resumed(bool),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
	// Asking each binding to suspend.
	Suspending,
	// A binding refused or did not answer: resuming the ones already asked, then answering the failure.
	Unwinding,
	// Every binding suspended and the answer given; waiting for `resume`.
	Held,
	// Asking each suspended binding to resume.
	Resuming,
	// Hibernation's: resuming the bindings the image is written through, and suspending them again.
	ImageResuming,
	ImageSuspending,
}

#[derive(Clone, Copy)]
struct Asking {
	node: usize,
	generation: u64,
	deadline: u64,
}

pub(super) struct SleepRun {
	correlation: u32,
	state: driver_protocol::SleepState,
	arm_wake: bool,
	timed_wake_ms: u64,
	phase: Phase,
	// The bindings still to be asked, the front first.
	queue: Vec<usize>,
	asking: Option<Asking>,
	// Every binding this run suspended, and the ones that did not answer `SUSPEND` - both are resumed.
	suspended: Vec<usize>,
	wake_nodes: Vec<String>,
	// The earliest "awake by" a driver answered, as a tick; 0 for none.
	awake_by: u64,
	failed: Option<(String, String)>,
	not_back: Vec<String>,
	// A `resume` that came while the bindings were still being suspended - ServiceManager gave up waiting on the step -
	// by its correlation: taken as soon as the suspend is over.
	resume_owed: Option<u32>,
	// The bindings resumed for the image, and the one that failed it, with why.
	image: Vec<usize>,
	image_labels: Vec<String>,
	image_failed: Option<(String, String)>,
}

impl SleepRun {
	// The tick the loop must come back at for the binding being asked.
	pub(super) fn wake_at(&self) -> u64 {
		self.asking.map_or(0, |asking| asking.deadline)
	}
}

// Whether a control-channel message is one of this interface's requests rather than one of the supervisor's tagged
// hand-offs, every one of which begins with an ASCII tag - two letters, far past these opcodes.
pub(super) fn is_request(message: &[u8]) -> bool {
	message.len() >= 6 && matches!(u16::from_le_bytes([message[0], message[1]]), device_sleep::OP_CHECK | device_sleep::OP_SUSPEND | device_sleep::OP_RESUME | device_sleep::OP_RESUME_FOR_IMAGE | device_sleep::OP_SUSPEND_IMAGE)
}

// Whether a control-channel message is a restore's `stop-all`, which the standing loop answers itself - it is a
// shutdown's teardown.
pub(super) fn is_stop_all(message: &[u8]) -> Option<u32> {
	(message.len() >= 6 && u16::from_le_bytes([message[0], message[1]]) == device_sleep::OP_STOP_ALL).then(|| u32::from_le_bytes([message[2], message[3], message[4], message[5]]))
}

// `stop-all`'s answer: the bindings still bound once the teardown ran.
pub(super) fn answer_stopped(correlation: u32, nodes: &[Node]) -> Option<Vec<u8>> {
	let left: Vec<String> = nodes.iter().filter(|node| node.binding.is_some()).map(label).collect();
	reply(correlation, |w| write_list(w, &left))
}

// Whether the node's binding publishes what the image is written through: a disk, or the TPM its key is sealed by.
fn carries_the_image(node: &Node) -> bool {
	node.entry().is_some_and(|entry| entry.provides.iter().any(|&(kind, _, _)| kind == driver_protocol::provider::BLOCK || kind == driver_protocol::provider::TPM))
}

// THE BINDING, NAMED FOR A PERSON: its driver and its device.
pub(super) fn label(node: &Node) -> String {
	let mut text = String::from_utf8_lossy(&driver_text(node.driver_name())).into_owned();
	if let Some(platform) = node.id.platform {
		let mut number = [0u8; 20];
		let n = decimal(u64::from(platform), &mut number);
		text.push_str(" (platform device ");
		text.push_str(core::str::from_utf8(&number[..n]).unwrap_or("?"));
		text.push(')');
	} else {
		const HEX: &[u8; 16] = b"0123456789abcdef";
		let hex = |byte: u8| [HEX[usize::from(byte >> 4)] as char, HEX[usize::from(byte & 0xF)] as char];
		let [b0, b1] = hex(node.id.bus);
		let [d0, d1] = hex(node.id.dev);
		for c in [' ', '(', b0, b1, ':', d0, d1, '.', (b'0' + (node.id.func & 7)) as char, ')'] {
			text.push(c);
		}
	}
	text
}

// Whether the node's binding publishes a `watchdog`: suspended last, resumed first.
fn publishes_watchdog(node: &Node) -> bool {
	node.entry().is_some_and(|entry| entry.provides.iter().any(|&(kind, _, _)| kind == driver_protocol::provider::WATCHDOG))
}

// THE BINDINGS THE SLEEP CANNOT SUSPEND: every online binding whose entry declares no `suspend-deadline`, and every
// bind or stop in flight.
pub(super) fn unready(nodes: &[Node]) -> Vec<String> {
	let mut out: Vec<String> = Vec::new();
	for node in nodes {
		let why: &str = if node.binding.is_some() && node.record.state == BindingState::Online {
			if node.entry().is_some_and(|entry| entry.suspend_deadline.is_some()) {
				continue;
			}
			"its driver does not carry the suspend exchange"
		} else if node.in_flight() {
			"a bind or a stop is in flight"
		} else {
			continue;
		};
		let mut line = label(node);
		line.push_str(": ");
		line.push_str(why);
		out.push(line);
	}
	out
}

fn ms_of(ticks: u64) -> u64 {
	ticks.saturating_mul(1000) / TICKS_PER_SECOND
}

// A binding's bound for one answer: its entry's `suspend-deadline`, scaled as its bind deadlines are.
fn bound_of(node: &Node) -> u64 {
	let declared = node.entry().and_then(|entry| entry.suspend_deadline).unwrap_or(driver_protocol::MAX_SUSPEND_DEADLINE).min(driver_protocol::MAX_SUSPEND_DEADLINE);
	u64::from(declared).saturating_mul(machine_scale())
}

// ------------------------------------------------------------------ the requests

// One request off the control channel. Answers the reply to send now, if there is one; a `suspend` or a `resume` that
// has bindings to ask is answered by `step` when it is over.
pub(super) fn request(run: &mut Option<SleepRun>, nodes: &mut [Node], message: &[u8]) -> Option<Vec<u8>> {
	let mut reader = wire::Reader::new(message);
	let op = reader.u16()?;
	let correlation = reader.u32()?;
	match op {
		device_sleep::OP_CHECK => {
			reader.finish()?;
			let list = unready(nodes);
			reply(correlation, |w| write_list(w, &list))
		}
		device_sleep::OP_SUSPEND => {
			let state = SleepState::read(&mut reader)?;
			let arm_wake = reader.boolean()?;
			let timed_wake_ms = reader.u64()?;
			reader.finish()?;
			let Some(state) = driver_state(state) else { return refuse(correlation, Error::Unsupported) };
			if run.is_some() {
				return refuse(correlation, Error::Invalid);
			}
			// NO DRIVER IS SKIPPED: asked again here, since a binding may have come online since the check.
			if let Some(first) = unready(nodes).into_iter().next() {
				let (who, why) = first.split_once(": ").map_or((first.clone(), String::new()), |(who, why)| (String::from(who), String::from(why)));
				say_line(&[b"the sleep cannot suspend ", who.as_bytes(), b" - ", why.as_bytes()]);
				return answer_suspended(correlation, &DriversSuspended { failed: who, why, wake_nodes: Vec::new(), awake_by_ms: 0 });
			}
			let online: Vec<usize> = (0..nodes.len()).filter(|&at| nodes[at].binding.is_some() && nodes[at].record.state == BindingState::Online).collect();
			// REVERSE BIND ORDER, a consumer before its provider, every `watchdog` publisher last - `sleep_order`'s.
			let bindings = order_view(nodes, &online);
			let queue: Vec<usize> = service_logic::sleep_order::suspend_order(&bindings).into_iter().map(|at| online[at]).collect();
			let mut count = [0u8; 20];
			let n = decimal(queue.len() as u64, &mut count);
			say_line(&[b"the sleep's driver step: ", &count[..n], b" binding(s) to suspend, one at a time"]);
			*run = Some(SleepRun { correlation, state, arm_wake, timed_wake_ms, phase: Phase::Suspending, queue, asking: None, suspended: Vec::new(), wake_nodes: Vec::new(), awake_by: 0, failed: None, not_back: Vec::new(), resume_owed: None, image: Vec::new(), image_labels: Vec::new(), image_failed: None });
			None
		}
		device_sleep::OP_RESUME => {
			let state = SleepState::read(&mut reader)?;
			reader.finish()?;
			let Some(state) = driver_state(state) else { return refuse(correlation, Error::Unsupported) };
			match run {
				// NOTHING IS SUSPENDED: the step answers at once, so a resume after an unwound suspend is harmless.
				None => answer_resumed(correlation, &[]),
				Some(held) if held.phase == Phase::Held => {
					held.correlation = correlation;
					held.state = state;
					held.phase = Phase::Resuming;
					held.queue = resume_order(nodes, &held.suspended);
					None
				}
				// STILL SUSPENDING: the resume is owed, and taken the moment the suspend is over.
				Some(going) if going.phase == Phase::Suspending => {
					going.resume_owed = Some(correlation);
					None
				}
				Some(_) => refuse(correlation, Error::Invalid),
			}
		}
		// HIBERNATION'S: the image's bindings back, with no power lost - only while every binding is held suspended.
		device_sleep::OP_RESUME_FOR_IMAGE => {
			reader.finish()?;
			let Some(held) = run.as_mut().filter(|held| held.phase == Phase::Held) else { return refuse(correlation, Error::Invalid) };
			let image: Vec<usize> = held.suspended.iter().copied().filter(|&at| carries_the_image(&nodes[at])).collect();
			if image.is_empty() {
				say_line(&[b"the image has no binding to be written through"]);
				return reply(correlation, |w| write_list(w, &[]));
			}
			held.suspended.retain(|at| !image.contains(at));
			held.queue = resume_order(nodes, &image);
			held.image = Vec::new();
			held.image_labels = Vec::new();
			held.image_failed = None;
			held.correlation = correlation;
			held.phase = Phase::ImageResuming;
			None
		}
		device_sleep::OP_SUSPEND_IMAGE => {
			reader.finish()?;
			let Some(held) = run.as_mut().filter(|held| held.phase == Phase::Held) else { return refuse(correlation, Error::Invalid) };
			if held.image.is_empty() {
				return reply(correlation, |_| Some(()));
			}
			let bindings = order_view(nodes, &held.image);
			held.queue = service_logic::sleep_order::suspend_order(&bindings).into_iter().map(|at| held.image[at]).collect();
			held.image = Vec::new();
			held.image_failed = None;
			held.correlation = correlation;
			held.phase = Phase::ImageSuspending;
			None
		}
		_ => None,
	}
}

fn driver_state(state: SleepState) -> Option<driver_protocol::SleepState> {
	match state {
		SleepState::Idle => Some(driver_protocol::SleepState::Idle),
		SleepState::Ram => Some(driver_protocol::SleepState::Ram),
		SleepState::Disk => Some(driver_protocol::SleepState::Disk),
	}
}

// BIND ORDER, every `watchdog` publisher first: providers before their consumers - `sleep_order`'s.
fn resume_order(nodes: &[Node], suspended: &[usize]) -> Vec<usize> {
	let bindings = order_view(nodes, suspended);
	let all: Vec<usize> = (0..suspended.len()).collect();
	service_logic::sleep_order::resume_order(&bindings, &all).into_iter().map(|at| suspended[at]).collect()
}

// THE BINDINGS `at` NAMES, as the order sees them: whether each publishes a `watchdog`, how deep it consumes, when it
// bound.
fn order_view(nodes: &[Node], at: &[usize]) -> Vec<service_logic::sleep_order::Binding> {
	let depth = dependency_depths(nodes);
	at.iter().map(|&index| service_logic::sleep_order::Binding { watchdog: publishes_watchdog(&nodes[index]), depth: depth[index] as u32, bind_at: nodes[index].bind_at }).collect()
}

fn reply(correlation: u32, body: impl FnOnce(&mut wire::VecWriter) -> Option<()>) -> Option<Vec<u8>> {
	let mut w = wire::VecWriter::new();
	w.u32(correlation)?;
	w.u8(1)?;
	body(&mut w)?;
	w.into_inner()
}

fn refuse(correlation: u32, error: Error) -> Option<Vec<u8>> {
	let mut w = wire::VecWriter::new();
	w.u32(correlation)?;
	w.u8(0)?;
	error.write(&mut w)?;
	w.into_inner()
}

fn answer_suspended(correlation: u32, answer: &DriversSuspended) -> Option<Vec<u8>> {
	reply(correlation, |w| answer.write(w))
}

fn answer_resumed(correlation: u32, not_back: &[String]) -> Option<Vec<u8>> {
	reply(correlation, |w| write_list(w, not_back))
}

// A `list<string>` as the wire carries one: a 16-bit count, then each string length-prefixed. A list past the count's
// reach is cut at it rather than refused whole.
fn write_list(w: &mut wire::VecWriter, lines: &[String]) -> Option<()> {
	let count = lines.len().min(usize::from(u16::MAX));
	w.u16(count as u16)?;
	for line in &lines[..count] {
		w.bytes_lp(line.as_bytes())?;
	}
	Some(())
}

// ------------------------------------------------------------------ the step, one pass at a time

// ADVANCE THE RUN as far as the answers already in allow. Answers the reply to send when the step is over; `done`
// says the run has ended and holds nothing any more.
pub(super) fn step(run: &mut SleepRun, nodes: &mut [Node]) -> (Option<Vec<u8>>, bool) {
	loop {
		if let Some(asking) = run.asking {
			let node = &mut nodes[asking.node];
			// THE ANSWER FIRST: a device that did not come back is torn down by the time this reads it, and it is still
			// the answer it gave rather than a driver that ended.
			let answer = node.sleep_answer.take();
			let gone = node.binding.is_none() || node.id.generation != asking.generation;
			if answer.is_none() && !gone {
				if clock() < asking.deadline {
					return (None, false);
				}
				node.sleep_asked = SleepAsked::None;
				unanswered(run, nodes, asking.node);
			} else {
				node.sleep_asked = SleepAsked::None;
				answered(run, nodes, asking.node, answer);
			}
			run.asking = None;
		}
		if run.phase == Phase::Held {
			return (None, false);
		}
		if run.queue.is_empty() {
			// A suspend that ended with a resume owed goes straight on to it, in this same pass.
			let before = run.phase;
			let ended = finish(run);
			if ended.0.is_none() && !ended.1 && run.phase != before {
				continue;
			}
			return ended;
		}
		let at = run.queue.remove(0);
		ask(run, nodes, at);
	}
}

// Send the phase's question to one binding; one whose channel is gone is answered as gone on the next turn.
fn ask(run: &mut SleepRun, nodes: &mut [Node], at: usize) {
	let node = &mut nodes[at];
	let Some(binding) = &node.binding else { return };
	let (channel, generation) = (binding.channel, node.id.generation);
	let mut payload = [0u8; driver_protocol::SUSPEND_PAYLOAD_LEN];
	let (opcode, len, asked) = match run.phase {
		Phase::Suspending | Phase::ImageSuspending => (driver_protocol::Opcode::Suspend, driver_protocol::encode_suspend(&driver_protocol::SuspendRequest { state: run.state, arm_wake: run.arm_wake, timed_wake_ms: run.timed_wake_ms }, &mut payload), SleepAsked::Suspend),
		// AN UNWIND NEVER SLEPT, and neither did a snapshot: nothing lost power.
		Phase::Unwinding | Phase::ImageResuming => (driver_protocol::Opcode::Resume, driver_protocol::encode_resume(false, &mut payload), SleepAsked::Resume),
		_ => (driver_protocol::Opcode::Resume, driver_protocol::encode_resume(run.state.loses_power(), &mut payload), SleepAsked::Resume),
	};
	node.sleep_answer = None;
	node.sleep_asked = asked;
	let deadline = if send_frame(channel, opcode, generation, &payload[..len], 0, 0) { clock().saturating_add(bound_of(node)) } else { 0 };
	run.asking = Some(Asking { node: at, generation, deadline });
}

fn answered(run: &mut SleepRun, nodes: &mut [Node], at: usize, answer: Option<SleepAnswer>) {
	let who = label(&nodes[at]);
	match (run.phase, answer) {
		(Phase::Suspending, Some(SleepAnswer::Suspended(suspended))) => match suspended.outcome {
			driver_protocol::SuspendOutcome::Done | driver_protocol::SuspendOutcome::DoneWakeArmed => {
				run.suspended.push(at);
				if suspended.outcome == driver_protocol::SuspendOutcome::DoneWakeArmed {
					match firmware_identity(&nodes[at]) {
						Some(path) => run.wake_nodes.push(path),
						None => say_line(&[who.as_bytes(), b" armed its wake, and the firmware describes no node to arm it at"]),
					}
				}
				if suspended.awake_by_ms != 0 {
					// Rounded DOWN: an "awake by" is never lengthened.
					let by = clock().saturating_add(suspended.awake_by_ms.saturating_mul(TICKS_PER_SECOND) / 1000);
					if run.awake_by == 0 || by < run.awake_by {
						run.awake_by = by;
					}
				}
				say_line(&[b"suspended ", who.as_bytes()]);
			}
			driver_protocol::SuspendOutcome::Refused(code) => fail(run, who, String::from_utf8_lossy(code.name()).into_owned()),
		},
		(Phase::Suspending, _) => fail(run, who, String::from("its driver ended before it answered the suspend")),
		// HIBERNATION'S: a binding that did not come back for the image is torn down and bound again once the sleep has
		// ended, as after any sleep - a driver that sets a stopped device up again only by a bind answers so by design -
		// and the step goes on with the rest: whether the image's own disk came back is what the write finds out. One that
		// would not suspend again for a hybrid sleep's S3 fails the step that asked it.
		(Phase::ImageResuming, Some(SleepAnswer::Resumed(true))) => {
			say_line(&[b"resumed for the image: ", who.as_bytes()]);
			run.image.push(at);
			run.image_labels.push(who);
		}
		(Phase::ImageResuming, _) => {
			say_line(&[who.as_bytes(), b" did not come back for the image; it is torn down and bound again once the sleep has ended"]);
			run.not_back.push(who);
		}
		(Phase::ImageSuspending, Some(SleepAnswer::Suspended(suspended))) if matches!(suspended.outcome, driver_protocol::SuspendOutcome::Done | driver_protocol::SuspendOutcome::DoneWakeArmed) => {
			say_line(&[b"suspended again after the image: ", who.as_bytes()]);
			run.suspended.push(at);
		}
		(Phase::ImageSuspending, _) => {
			run.image.push(at);
			run.image_failed = Some((who, String::from("it would not suspend again after the image")));
		}
		// THE RESUME'S ANSWERS, the unwind's and the step's alike.
		(_, Some(SleepAnswer::Resumed(true))) => say_line(&[b"resumed ", who.as_bytes()]),
		(_, Some(SleepAnswer::Resumed(false))) => {
			say_line(&[who.as_bytes(), b" did not come back from the sleep; it is torn down and bound again"]);
			run.not_back.push(who);
		}
		(_, _) => {
			say_line(&[who.as_bytes(), b" ended while it was suspended; it is bound again"]);
			let mut line = who;
			line.push_str(" (its driver ended)");
			run.not_back.push(line);
		}
	}
}

// NO ANSWER WITHIN THE BOUND. A suspend that was not answered fails the step, and the binding is resumed with the rest
// - it may be half way. A resume that was not answered is a driver whose control path has stopped: torn down and
// bound again, as the heartbeat would once it runs again.
fn unanswered(run: &mut SleepRun, nodes: &mut [Node], at: usize) {
	let who = label(&nodes[at]);
	match run.phase {
		Phase::Suspending => {
			run.suspended.push(at);
			fail(run, who, String::from("it did not answer the suspend within its bound"));
		}
		// Half way, perhaps: resumed with the rest at the sleep's end.
		Phase::ImageSuspending => {
			run.suspended.push(at);
			run.image_failed = Some((who, String::from("it did not answer the suspend within its bound")));
		}
		_ => {
			say_line(&[who.as_bytes(), b" did not answer the resume within its bound; it is torn down and bound again"]);
			let generation = nodes[at].id.generation;
			nodes[at].push(BindingEvent::Wedged { generation });
			let mut line = who;
			line.push_str(" (no answer to the resume)");
			run.not_back.push(line);
		}
	}
}

// THE STEP FAILS: nothing more is asked, and everything already asked is resumed before the answer names who and why.
fn fail(run: &mut SleepRun, who: String, why: String) {
	say_line(&[b"the sleep's driver step failed at ", who.as_bytes(), b" - ", why.as_bytes(), b"; the bindings it suspended are resumed"]);
	run.failed = Some((who, why));
	run.phase = Phase::Unwinding;
	// The reverse of the order they were suspended in.
	let mut back: Vec<usize> = run.suspended.clone();
	back.reverse();
	run.queue = back;
}

fn finish(run: &mut SleepRun) -> (Option<Vec<u8>>, bool) {
	match run.phase {
		Phase::Suspending if run.resume_owed.is_some() => {
			// NOBODY WAITS FOR THIS ANSWER ANY MORE: the resume that came meanwhile starts at once.
			say_line(&[b"the sleep's driver step is done, and a resume is already owed - the bindings are resumed"]);
			run.correlation = run.resume_owed.take().unwrap_or(0);
			run.phase = Phase::Resuming;
			run.queue = core::mem::take(&mut run.suspended);
			run.queue.reverse();
			(None, false)
		}
		Phase::Unwinding if run.resume_owed.is_some() => {
			// The step unwound itself: the owed resume has nothing to resume.
			let correlation = run.resume_owed.take().unwrap_or(0);
			(answer_resumed(correlation, &run.not_back), true)
		}
		Phase::Suspending => {
			run.phase = Phase::Held;
			let awake_by_ms = if run.awake_by == 0 { 0 } else { ms_of(run.awake_by.saturating_sub(clock())).max(1) };
			let mut count = [0u8; 20];
			let n = decimal(run.suspended.len() as u64, &mut count);
			say_line(&[b"the sleep's driver step is done: ", &count[..n], b" binding(s) suspended"]);
			(answer_suspended(run.correlation, &DriversSuspended { failed: String::new(), why: String::new(), wake_nodes: core::mem::take(&mut run.wake_nodes), awake_by_ms }), false)
		}
		Phase::Unwinding => {
			let (failed, why) = run.failed.take().unwrap_or_default();
			(answer_suspended(run.correlation, &DriversSuspended { failed, why, wake_nodes: Vec::new(), awake_by_ms: 0 }), true)
		}
		Phase::Resuming => {
			say_line(&[b"the sleep's resume step is done"]);
			(answer_resumed(run.correlation, &run.not_back), true)
		}
		Phase::ImageResuming | Phase::ImageSuspending => {
			let resuming = run.phase == Phase::ImageResuming;
			run.phase = Phase::Held;
			match run.image_failed.take() {
				Some((who, why)) => {
					say_line(&[b"the image's bindings failed at ", who.as_bytes(), b" - ", why.as_bytes()]);
					(refuse(run.correlation, Error::Io), false)
				}
				None if resuming => (answer_resumed(run.correlation, &core::mem::take(&mut run.image_labels)), false),
				None => (reply(run.correlation, |_| Some(())), false),
			}
		}
		Phase::Held => (None, false),
	}
}
