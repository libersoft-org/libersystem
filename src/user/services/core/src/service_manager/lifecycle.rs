use super::*;
use wire::Sink;

// Whether component `i` depends on any component currently in the teardown scope.
pub(super) fn depends_on_scoped(i: usize, scope: &[bool; N]) -> bool {
	services::service_lifecycle::depends_on_any(i, N, |node| scope[node], index_of_dep)
}

// Whether any in-scope Running component still depends on component `i` - i.e. `i` is
// not yet a leaf of the scoped subgraph and must not be stopped this round.
pub(super) fn has_running_dependent(i: usize, scope: &[bool; N], state: &[State; N]) -> bool {
	services::service_lifecycle::has_active_dependent(i, N, |node| scope[node] && state[node] == State::Ready, index_of_dep)
}

// Whether component `j` declares component `i` among its dependencies.
fn index_of_dep(j: usize, i: usize) -> bool {
	for &dep in MANIFEST[j].deps {
		if index_of(dep) == Some(i) {
			return true;
		}
	}
	false
}

// WHAT DIES WITH THE MACHINE rather than before it: the shell - the issuing terminal, which holds no supervised
// Process here - and the ConsoleService that hosts it. Killing the console host ends the shell's attachment to the
// kernel console, and the kernel's console loop takes a shell that attached and went away for the operator typing
// `exit`: it halts. So a teardown that killed ConsoleService stopped the machine THERE - the rest of the services
// never stopped and the power request was never made; the orderly `reboot` left a halted guest.
fn dies_with_the_machine(node: usize) -> bool {
	Some(node) == index_of(b"shell") || Some(node) == index_of(b"console_service")
}

// The reverse-dependency teardown order for a graceful shutdown: every currently
// Running service but the two that die with the machine, ordered so a dependent always
// precedes every dependency it declares. Computed by repeatedly taking the current
// leaves of the scoped subgraph.
pub(super) fn shutdown_order(state: &[State; N]) -> Vec<usize> {
	services::service_lifecycle::reverse_dependency_order(N, |node| state[node] == State::Ready && !dies_with_the_machine(node), index_of_dep).unwrap_or_default()
}

// Tear the whole service tree down for a graceful power-off. LogService flushes first;
// every service whose row declares the shutdown notice is told next, under one bound; every
// other service then stops in reverse-dependency order. The issuing shell and the console
// that hosts it are excluded from the order and die with the machine. `notice_ticks` is the
// notice's bound; `None` leaves the notice out, which only a development hook asks for.
pub(super) fn shutdown_all(state: &mut [State; N], channels: &mut [u64; N], sup: &mut [Supervised; N], procs: &[u64; N], log_client: u64, action: proto::system::ShutdownAction, notice_ticks: Option<u64>, buf: &mut [u8]) {
	if let Some(log) = index_of(b"log_service") {
		if state[log] == State::Ready && channels[log] != 0 {
			send_blocking(channels[log], b"FLUSH", 0);
		}
	}
	if let Some(notice_ticks) = notice_ticks {
		notify_shutdown(state, channels, action, notice_ticks, buf);
	}
	let order: Vec<usize> = shutdown_order(state);
	for &idx in &order {
		if state[idx] != State::Ready {
			continue;
		}
		if procs[idx] != 0 {
			signal(procs[idx], SIG_KILL);
		}
		if !drain_closed_within(channels[idx], KILL_DRAIN_TICKS, buf) {
			console_report(MANIFEST[idx].name, b"did not end within its bound after SIG_KILL - it is left behind");
		}
		if channels[idx] != 0 {
			close(channels[idx]);
			channels[idx] = 0;
		}
		state[idx] = State::Stopped;
		sup[idx].failure = Failure::Stopped;
		emit_event(log_client, MANIFEST[idx].name, b"stopped");
		console_report(MANIFEST[idx].name, b"stopped");
	}
}

// Verify the selftest shutdown ordering: every Running non-shell service is present,
// and each dependent appears before every dependency that is also in the order.
pub(super) fn verify_shutdown_order(order: &[usize], state: &[State; N]) -> bool {
	services::service_lifecycle::verify_reverse_dependency_order(order, N, |node| state[node] == State::Ready && !dies_with_the_machine(node), index_of_dep)
}

// The status answer's bound: every row with room to spare, and far inside a message's.
const STATUS_REPLY_BYTES: usize = 32 * 1024;

// Answer one request on a supervisor stats channel. Returns false once the peer is
// gone, so the standing supervisor drops that channel from its wait set.
pub(super) fn serve_stats_once(stats: u64, state: &[State; N], desired: &[Desired; N], procs: &[u64; N], lifecycle: &LifecycleLog, sup: &[Supervised; N], reason: &[String; N], canary_sup: &Supervised, drivers: &[(&'static [u8], bool)], buf: &mut [u8]) -> bool {
	let (len, mut handle) = match recv_caps_blocking(stats, buf) {
		ReceivedCaps::Message { len, handles } => (len, handles),
		ReceivedCaps::Closed => return false,
	};
	let mut api = StatsApi { state, desired, procs, lifecycle, sup, reason, canary_sup, drivers };
	// ONE ROW PER SERVICE, THE CANARY AND ONE PER MANIFEST DRIVER - which outgrew 4 KiB as the drivers grew, when the
	// answer became `again` and `lssvc` and the BMC service's settled test both read nothing. On the heap, since the
	// standing loop's stack holds the rest.
	let mut reply: Vec<u8> = alloc::vec![0u8; STATUS_REPLY_BYTES];
	let mut reply_handle = proto::codec::Handles::new();
	if let Some(n) = supervisor::dispatch(&mut api, &buf[..len], &mut handle, &mut reply, &mut reply_handle) {
		if !send_caps_blocking(stats, &reply[..n], reply_handle.as_slice()) {
			for &leftover in reply_handle.as_slice() {
				close(leftover);
			}
		}
	}
	for &unclaimed in handle.as_slice() {
		close(unclaimed);
	}
	true
}

// The name a reader sees. `starting` and `stopping` are new words for states that always existed
// and had no name: a process that had been launched and had not reported in was called `running`,
// which is the one thing it certainly was not.
fn state_name(state: State) -> &'static str {
	match state {
		State::Absent => "pending",
		State::Starting => "starting",
		State::Ready => "running",
		State::Stopping => "stopping",
		State::Stopped => "stopped",
		State::Failed => "failed",
	}
}

struct StatsApi<'a> {
	state: &'a [State; N],
	desired: &'a [Desired; N],
	procs: &'a [u64; N],
	lifecycle: &'a LifecycleLog,
	sup: &'a [Supervised; N],
	reason: &'a [String; N],
	canary_sup: &'a Supervised,
	drivers: &'a [(&'static [u8], bool)],
}

// What the supervisor WANTS, as one word.
fn desired_name(desired: Desired) -> &'static str {
	match desired {
		Desired::Running => "running",
		Desired::Stopped => "stopped",
	}
}

// Why the last transition happened. A restart count says how often; this says what for.
fn reason_name(reason: Reason) -> &'static str {
	match reason {
		Reason::Started => "started",
		Reason::ReportedReady => "reported ready",
		Reason::BootstrapRefused => "bootstrap refused",
		Reason::NoReport => "no report",
		Reason::Faulted => "faulted",
		Reason::StopRequested => "stop requested",
		Reason::Replaced => "replaced",
		Reason::BudgetSpent => "restart budget spent",
	}
}

impl supervisor::Service for StatsApi<'_> {
	fn status(&mut self) -> Result<Vec<SupervisorStat>, Error> {
		let mut out: Vec<SupervisorStat> = Vec::new();
		let mut i: usize = 0;
		while i < N {
			let last_failure: String = if self.reason[i].is_empty() { String::from_utf8_lossy(self.sup[i].failure.as_bytes()).into_owned() } else { self.reason[i].clone() };
			let latest: Option<Transition> = self.lifecycle.latest(i);
			let last_reason: String = latest.map_or_else(String::new, |t| String::from(reason_name(t.reason)));
			// THE INSTANCE THIS ROW IS ABOUT. A running service answers with its own process. A
			// failed or stopped one has no process left, and answering 0 there threw away the
			// identity of the instance the last transition was about - which is the instance
			// somebody reading a failure is asking after.
			let epoch: u64 = match unsafe { epoch_of(self.procs[i]) } {
				0 => latest.map_or(0, |t| t.epoch),
				live => live,
			};
			out.push(SupervisorStat { name: String::from_utf8_lossy(MANIFEST[i].name).into_owned(), state: String::from(state_name(self.state[i])), desired: String::from(desired_name(self.desired[i])), epoch, last_reason, restarts: self.sup[i].restarts, watchdog_trips: self.sup[i].watchdog_trips, last_failure });
			i += 1;
		}
		// The canary and the drivers are not managed deployments: they have no declared desired
		// state and no instance epoch this supervisor assigns. Reported as blank rather than
		// invented, because a made-up epoch is worse than none.
		out.push(SupervisorStat { name: String::from("watchdog_probe"), state: String::from("running"), desired: String::new(), epoch: 0, last_reason: String::new(), restarts: self.canary_sup.restarts, watchdog_trips: self.canary_sup.watchdog_trips, last_failure: String::from_utf8_lossy(self.canary_sup.failure.as_bytes()).into_owned() });
		for &(name, online) in self.drivers {
			out.push(SupervisorStat { name: String::from_utf8_lossy(name).into_owned(), state: String::from(if online { "running" } else { "pending" }), desired: String::new(), epoch: 0, last_reason: String::new(), restarts: 0, watchdog_trips: 0, last_failure: String::new() });
		}
		Ok(out)
	}
}

// THE ORDERLY SHUTDOWN NOTICE: `prepare(action)` of `shutdown-notice`, sent at once to every running service whose row
// declares the notice - on the control channel held for it, the one LogService's `FLUSH` travels on - and the answers
// waited for under ONE bound, `notice_ticks`. A service that has not answered by then is killed with the rest; a
// message on a control channel that is not the answer is not served while the sequence runs.
//
// Only this orderly sequence tells anybody: the immediate power paths - the Power key, Ctrl+Alt+Delete, the power
// button DeviceManager handles - reach SystemManager's `system-power` directly and stay notice-free by design.
pub(super) fn notify_shutdown(state: &[State; N], channels: &[u64; N], action: proto::system::ShutdownAction, notice_ticks: u64, buf: &mut [u8]) {
	// One correlation for the whole sequence: each service is asked exactly once.
	const CORRELATION: u32 = 1;
	let mut writer = wire::VecWriter::new();
	let encoded: Option<()> = (|| {
		writer.u16(proto::system::shutdown_notice::OP_PREPARE)?;
		writer.u32(CORRELATION)?;
		action.write(&mut writer)
	})();
	let Some(request) = encoded.and_then(|()| writer.into_inner()) else { return };
	let mut waiting: Vec<usize> = Vec::new();
	for idx in 0..N {
		if MANIFEST[idx].notices & NOTICE_SHUTDOWN != 0 && state[idx] == State::Ready && channels[idx] != 0 && try_send(channels[idx], &request, 0) {
			waiting.push(idx);
		}
	}
	let deadline: u64 = clock() + notice_ticks;
	while !waiting.is_empty() {
		let handles: Vec<u64> = waiting.iter().map(|&idx| channels[idx]).collect();
		let ready: i64 = wait_any(&handles, deadline);
		if ready < 0 {
			break;
		}
		let at: usize = ready as usize;
		let idx: usize = waiting[at];
		match try_recv(channels[idx], buf) {
			Polled::Message { len, handle } => {
				if handle != 0 {
					close(handle);
				}
				if len >= 5 && buf[..4] == CORRELATION.to_le_bytes() {
					console_report(MANIFEST[idx].name, if buf[4] == 1 { b"answered the shutdown notice" } else { b"refused the shutdown notice" });
					waiting.remove(at);
				}
			}
			Polled::Closed => {
				waiting.remove(at);
			}
			Polled::Empty => {}
		}
	}
	for &idx in &waiting {
		console_report(MANIFEST[idx].name, b"did not answer the shutdown notice within its bound");
	}
}
