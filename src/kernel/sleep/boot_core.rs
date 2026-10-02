// THE BOOT CORE'S IDLE CONTEXT, WHERE A SLEEP THAT TAKES EVERY CORE IS ENTERED: suspend to RAM, hibernation's snapshot
// and a restore's replacement. Each is asked from any thread and RUN on the boot core once it idles
// (`arch::sleep::run`) - the one context a firmware's resume or an image's comes back to, which no thread owns - while
// the asking thread blocks until the boot core answers: after the resume, or at a refusal.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use abi::{ERR_ACCESS_DENIED, SleepReport};

use crate::sync::SpinLock;

// WHAT THE BOOT CORE IS ASKED TO RUN: S3; hibernation's snapshot; or a restore's whole-memory replacement, which
// answers only when it cannot happen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	Ram,
	Snapshot,
	Replace,
}

struct Request {
	kind: Kind,
	pair: (u8, u8),
	after: Option<u64>,
	answer: Option<Result<SleepReport, i64>>,
}

static REQUEST: SpinLock<Option<Request>> = SpinLock::new(None);
// The koid the asking thread blocks on, and whether a request waits for the boot core.
static REQUEST_KOID: AtomicU64 = AtomicU64::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);

// A REQUEST, asked from any core: handed to the boot core's idle context, and this thread blocked until it answers.
// One at a time: a second while one is held is refused.
pub fn ask(kind: Kind, pair: (u8, u8), after: Option<u64>) -> Result<SleepReport, i64> {
	let koid = crate::object::ObjectHeader::new().koid();
	{
		let mut request = REQUEST.lock();
		if request.is_some() {
			return Err(ERR_ACCESS_DENIED);
		}
		*request = Some(Request { kind, pair, after, answer: None });
	}
	REQUEST_KOID.store(koid, Ordering::Release);
	PENDING.store(true, Ordering::Release);
	crate::idle::wake_core(0);
	loop {
		if let Some(answer) = {
			let mut request = REQUEST.lock();
			match request.as_mut().and_then(|held| held.answer.take()) {
				Some(answer) => {
					*request = None;
					Some(answer)
				}
				None => None,
			}
		} {
			return answer;
		}
		crate::sched::block_on_flagged(koid, crate::sched::NO_DEADLINE, false, || REQUEST.lock().as_ref().is_some_and(|held| held.answer.is_some()));
	}
}

// Whether a request waits for the boot core's idle context.
pub fn pending() -> bool {
	PENDING.load(Ordering::Acquire)
}

// THE BOOT CORE, IDLE: the pending request run, and the asker woken with the answer.
pub fn run_pending() {
	if !PENDING.swap(false, Ordering::AcqRel) {
		return;
	}
	let Some((kind, pair, after)) = REQUEST.lock().as_ref().map(|request| (request.kind, request.pair, request.after)) else { return };
	let answer = crate::arch::sleep::run(kind, pair, after);
	if let Some(request) = REQUEST.lock().as_mut() {
		request.answer = Some(answer);
	}
	crate::sched::wake_object(REQUEST_KOID.load(Ordering::Acquire));
}
