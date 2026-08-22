use super::*;

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

// The reverse-dependency teardown order for a graceful shutdown: every currently
// Running service (the shell exempted - it is the issuing terminal and holds no
// supervised Process here), ordered so a dependent always precedes every dependency it
// declares. Computed by repeatedly taking the current leaves of the scoped subgraph.
pub(super) fn shutdown_order(state: &[State; N]) -> Vec<usize> {
	let shell = index_of(b"shell");
	services::service_lifecycle::reverse_dependency_order(N, |node| state[node] == State::Ready && Some(node) != shell, index_of_dep).unwrap_or_default()
}

// Tear the whole service tree down for a graceful power-off. LogService flushes first;
// every other service then stops in reverse-dependency order. The issuing shell is
// excluded from the order and dies with the machine.
pub(super) unsafe fn shutdown_all(state: &mut [State; N], channels: &mut [u64; N], sup: &mut [Supervised; N], procs: &[u64; N], log_client: u64, buf: &mut [u8]) {
	unsafe {
		if let Some(log) = index_of(b"log_service") {
			if state[log] == State::Ready && channels[log] != 0 {
				send_blocking(channels[log], b"FLUSH", 0);
			}
		}
		let order: Vec<usize> = shutdown_order(state);
		for &idx in &order {
			if state[idx] != State::Ready {
				continue;
			}
			if procs[idx] != 0 {
				signal(procs[idx], SIG_KILL);
			}
			drain_closed(channels[idx], buf);
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
}

// Verify the selftest shutdown ordering: every Running non-shell service is present,
// and each dependent appears before every dependency that is also in the order.
pub(super) fn verify_shutdown_order(order: &[usize], state: &[State; N]) -> bool {
	let shell: Option<usize> = index_of(b"shell");
	services::service_lifecycle::verify_reverse_dependency_order(order, N, |node| state[node] == State::Ready && Some(node) != shell, index_of_dep)
}

// Answer one request on a supervisor stats channel. Returns false once the peer is
// gone, so the standing supervisor drops that channel from its wait set.
pub(super) unsafe fn serve_stats_once(stats: u64, state: &[State; N], desired: &[Desired; N], procs: &[u64; N], lifecycle: &LifecycleLog, sup: &[Supervised; N], reason: &[String; N], canary_sup: &Supervised, drivers: &[(&'static [u8], bool)], buf: &mut [u8]) -> bool {
	unsafe {
		let (len, mut handle) = match recv_caps_blocking(stats, buf) {
			ReceivedCaps::Message { len, handles } => (len, handles),
			ReceivedCaps::Closed => return false,
		};
		let mut api = StatsApi { state, desired, procs, lifecycle, sup, reason, canary_sup, drivers };
		let mut reply: [u8; 4096] = [0u8; 4096];
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
