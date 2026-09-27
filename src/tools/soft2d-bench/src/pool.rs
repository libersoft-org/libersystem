//! A HOST THREAD POOL FOR THE BENCHMARK, in the shape of the guest's `rt::pool`: threads made once and
//! kept, the caller taking part with lane zero, units handed out by one atomic counter.
//!
//! PERSISTENT, BECAUSE THE BENCHMARK MEASURES THE FRAME AND NOT THE POOL. Spawning sixty-four threads a
//! frame costs milliseconds on its own, which is the size of the frames being measured; the guest's
//! pool keeps its workers for the same reason.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use soft2d::{Lane, Unit, Workers};

/// What a helper runs: the dispatch's address and the function that knows its type.
#[derive(Clone, Copy)]
struct Job {
	enter: unsafe fn(*const (), usize),
	data: *const (),
	helpers: usize,
}

// SAFETY: the pointer is to a `Dispatch` on the stack of a `run` that does not return until every
// helper has left it, and a helper touches it only between being handed the job and reporting done.
unsafe impl Send for Job {}

struct State {
	generation: u64,
	job: Option<Job>,
	busy: usize,
	exit: bool,
}

struct Shared {
	state: Mutex<State>,
	start: Condvar,
	finished: Condvar,
}

pub struct HostPool {
	shared: Arc<Shared>,
	threads: Vec<std::thread::JoinHandle<()>>,
}

/// One frame's units and the lanes that run them.
struct Dispatch<'d, 'u> {
	lanes: *mut Lane,
	units: *mut Unit<'u>,
	count: usize,
	next: AtomicUsize,
	work: &'d (dyn Fn(&mut Lane, &mut Unit<'u>) + Sync),
}

/// Take units until there are none, on lane `lane`.
///
/// SAFETY: `data` is a live `Dispatch`, `lane` is a lane index no other participant holds, and each unit
/// index is claimed by exactly one `fetch_add`, so no two participants hold one unit or one lane.
unsafe fn participate(data: *const (), lane: usize) {
	let dispatch = unsafe { &*(data as *const Dispatch<'_, '_>) };
	loop {
		let index = dispatch.next.fetch_add(1, Ordering::Relaxed);
		if index >= dispatch.count {
			break;
		}
		unsafe { (dispatch.work)(&mut *dispatch.lanes.add(lane), &mut *dispatch.units.add(index)) };
	}
}

impl HostPool {
	/// A pool offering `lanes` lanes: this thread and `lanes - 1` helpers.
	pub fn new(lanes: usize) -> HostPool {
		let shared = Arc::new(Shared { state: Mutex::new(State { generation: 0, job: None, busy: 0, exit: false }), start: Condvar::new(), finished: Condvar::new() });
		let threads = (1..lanes.max(1))
			.map(|helper| {
				let shared = shared.clone();
				std::thread::Builder::new()
					// The guest's workers run on 256 KiB stacks; a unit that needed more would find out
					// here first.
					.stack_size(256 * 1024)
					.spawn(move || {
						let mut seen = 0u64;
						loop {
							let job = {
								let mut state = shared.state.lock().unwrap();
								while !state.exit && state.generation == seen {
									state = shared.start.wait(state).unwrap();
								}
								if state.exit {
									return;
								}
								seen = state.generation;
								state.job
							};
							if let Some(job) = job
								&& helper <= job.helpers
							{
								// SAFETY: see `Job`; helper `n` runs lane `n`, which no one else holds.
								unsafe { (job.enter)(job.data, helper) };
								let mut state = shared.state.lock().unwrap();
								state.busy -= 1;
								if state.busy == 0 {
									shared.finished.notify_all();
								}
							}
						}
					})
					.expect("a helper thread")
			})
			.collect();
		HostPool { shared, threads }
	}
}

impl Workers for HostPool {
	fn lanes(&self) -> usize {
		self.threads.len() + 1
	}

	fn run<'u>(&self, lanes: &mut [Lane], units: &mut [Unit<'u>], work: &(dyn Fn(&mut Lane, &mut Unit<'u>) + Sync)) {
		if lanes.is_empty() {
			return;
		}
		// AT MOST ONE HELPER FEWER THAN THERE ARE UNITS, as the guest's pool wakes: a unit is not split,
		// so a helper beyond that would find nothing to take.
		let helpers = self.threads.len().min(lanes.len() - 1).min(units.len().saturating_sub(1));
		let dispatch = Dispatch { lanes: lanes.as_mut_ptr(), units: units.as_mut_ptr(), count: units.len(), next: AtomicUsize::new(0), work };
		let data = &dispatch as *const Dispatch<'_, 'u> as *const ();
		if helpers > 0 {
			let mut state = self.shared.state.lock().unwrap();
			state.job = Some(Job { enter: participate, data, helpers });
			state.busy = helpers;
			state.generation += 1;
			self.shared.start.notify_all();
		}
		// SAFETY: lane zero is this thread's alone; the helpers were given lanes one to `helpers`.
		unsafe { participate(data, 0) };
		if helpers > 0 {
			let mut state = self.shared.state.lock().unwrap();
			while state.busy > 0 {
				state = self.shared.finished.wait(state).unwrap();
			}
			state.job = None;
		}
	}
}

impl Drop for HostPool {
	fn drop(&mut self) {
		{
			let mut state = self.shared.state.lock().unwrap();
			state.exit = true;
			self.shared.start.notify_all();
		}
		for thread in self.threads.drain(..) {
			let _ = thread.join();
		}
	}
}
