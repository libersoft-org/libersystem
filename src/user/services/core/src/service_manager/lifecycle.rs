use super::*;

// Whether component `i` depends on any component currently in the teardown scope.
pub(super) fn depends_on_scoped(i: usize, scope: &[bool; N]) -> bool {
	services::service_lifecycle::depends_on_any(i, N, |node| scope[node], index_of_dep)
}

// Whether any in-scope Running component still depends on component `i` - i.e. `i` is
// not yet a leaf of the scoped subgraph and must not be stopped this round.
pub(super) fn has_running_dependent(i: usize, scope: &[bool; N], state: &[State; N]) -> bool {
	services::service_lifecycle::has_active_dependent(i, N, |node| scope[node] && state[node] == State::Running, index_of_dep)
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
	services::service_lifecycle::reverse_dependency_order(N, |node| state[node] == State::Running && Some(node) != shell, index_of_dep).unwrap_or_default()
}

// Tear the whole service tree down for a graceful power-off. LogService flushes first;
// every other service then stops in reverse-dependency order. The issuing shell is
// excluded from the order and dies with the machine.
pub(super) unsafe fn shutdown_all(state: &mut [State; N], channels: &mut [u64; N], sup: &mut [Supervised; N], procs: &[u64; N], log_client: u64, buf: &mut [u8]) {
	unsafe {
		if let Some(log) = index_of(b"log_service") {
			if state[log] == State::Running && channels[log] != 0 {
				send_blocking(channels[log], b"FLUSH", 0);
			}
		}
		let order: Vec<usize> = shutdown_order(state);
		for &idx in &order {
			if state[idx] != State::Running {
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
	services::service_lifecycle::verify_reverse_dependency_order(order, N, |node| state[node] == State::Running && Some(node) != shell, index_of_dep)
}

// Answer one request on a supervisor stats channel. Returns false once the peer is
// gone, so the standing supervisor drops that channel from its wait set.
pub(super) unsafe fn serve_stats_once(stats: u64, state: &[State; N], sup: &[Supervised; N], reason: &[String; N], canary_sup: &Supervised, drivers: &[(&'static [u8], bool)], buf: &mut [u8]) -> bool {
	unsafe {
		let (len, mut handle) = match recv_caps_blocking(stats, buf) {
			ReceivedCaps::Message { len, handles } => (len, handles),
			ReceivedCaps::Closed => return false,
		};
		let mut api = StatsApi { state, sup, reason, canary_sup, drivers };
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

fn state_name(state: State) -> &'static str {
	match state {
		State::Pending => "pending",
		State::Running => "running",
		State::Stopped => "stopped",
		State::Failed => "failed",
	}
}

struct StatsApi<'a> {
	state: &'a [State; N],
	sup: &'a [Supervised; N],
	reason: &'a [String; N],
	canary_sup: &'a Supervised,
	drivers: &'a [(&'static [u8], bool)],
}

impl supervisor::Service for StatsApi<'_> {
	fn status(&mut self) -> Result<Vec<SupervisorStat>, Error> {
		let mut out: Vec<SupervisorStat> = Vec::new();
		let mut i: usize = 0;
		while i < N {
			let last_failure: String = if self.reason[i].is_empty() { String::from_utf8_lossy(self.sup[i].failure.as_bytes()).into_owned() } else { self.reason[i].clone() };
			out.push(SupervisorStat { name: String::from_utf8_lossy(MANIFEST[i].name).into_owned(), state: String::from(state_name(self.state[i])), restarts: self.sup[i].restarts, watchdog_trips: self.sup[i].watchdog_trips, last_failure });
			i += 1;
		}
		out.push(SupervisorStat { name: String::from("watchdog_probe"), state: String::from("running"), restarts: self.canary_sup.restarts, watchdog_trips: self.canary_sup.watchdog_trips, last_failure: String::from_utf8_lossy(self.canary_sup.failure.as_bytes()).into_owned() });
		for &(name, online) in self.drivers {
			out.push(SupervisorStat { name: String::from_utf8_lossy(name).into_owned(), state: String::from(if online { "running" } else { "pending" }), restarts: 0, watchdog_trips: 0, last_failure: String::new() });
		}
		Ok(out)
	}
}
