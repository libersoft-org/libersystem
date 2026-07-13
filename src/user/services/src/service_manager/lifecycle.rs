use super::*;

// Whether component `i` depends on any component currently in the teardown scope.
pub(super) fn depends_on_scoped(i: usize, scope: &[bool; N]) -> bool {
	for &dep in MANIFEST[i].deps {
		if let Some(d) = index_of(dep) {
			if scope[d] {
				return true;
			}
		}
	}
	false
}

// Whether any in-scope Running component still depends on component `i` - i.e. `i` is
// not yet a leaf of the scoped subgraph and must not be stopped this round.
pub(super) fn has_running_dependent(i: usize, scope: &[bool; N], state: &[State; N]) -> bool {
	has_active_dependent(i, scope, |j| state[j] == State::Running)
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
	let mut scope: [bool; N] = [false; N];
	let mut i: usize = 0;
	while i < N {
		if state[i] == State::Running {
			scope[i] = true;
		}
		i += 1;
	}
	if let Some(sh) = index_of(b"shell") {
		scope[sh] = false;
	}
	let mut dropped: [bool; N] = [false; N];
	let mut order: Vec<usize> = Vec::new();
	loop {
		let mut progress: bool = false;
		let mut i: usize = 0;
		while i < N {
			if scope[i] && !dropped[i] && !has_scoped_undropped_dependent(i, &scope, &dropped) {
				dropped[i] = true;
				order.push(i);
				progress = true;
			}
			i += 1;
		}
		if !progress {
			break;
		}
	}
	order
}

fn has_scoped_undropped_dependent(i: usize, scope: &[bool; N], dropped: &[bool; N]) -> bool {
	has_active_dependent(i, scope, |j| !dropped[j])
}

fn has_active_dependent(i: usize, scope: &[bool; N], mut active: impl FnMut(usize) -> bool) -> bool {
	let mut j: usize = 0;
	while j < N {
		if j != i && scope[j] && active(j) && index_of_dep(j, i) {
			return true;
		}
		j += 1;
	}
	false
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
	let mut i: usize = 0;
	while i < N {
		if state[i] == State::Running && Some(i) != shell && !order.contains(&i) {
			return false;
		}
		i += 1;
	}
	let mut pos: usize = 0;
	while pos < order.len() {
		let x: usize = order[pos];
		for &dep in MANIFEST[x].deps {
			if let Some(d) = index_of(dep) {
				if let Some(dpos) = order.iter().position(|&s| s == d) {
					if dpos < pos {
						return false;
					}
				}
			}
		}
		pos += 1;
	}
	true
}

// Answer one request on a supervisor stats channel. Returns false once the peer is
// gone, so the standing supervisor drops that channel from its wait set.
pub(super) unsafe fn serve_stats_once(stats: u64, state: &[State; N], sup: &[Supervised; N], reason: &[String; N], canary_sup: &Supervised, drivers: &[(&'static [u8], bool)], buf: &mut [u8]) -> bool {
	unsafe {
		let (len, handle): (usize, u64) = match recv_blocking(stats, buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => return false,
		};
		let mut api = StatsApi { state, sup, reason, canary_sup, drivers };
		let mut reply: [u8; 4096] = [0u8; 4096];
		let mut reply_handle: u64 = 0;
		if let Some(n) = supervisor::dispatch(&mut api, &buf[..len], handle, &mut reply, &mut reply_handle) {
			send_blocking(stats, &reply[..n], reply_handle);
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
