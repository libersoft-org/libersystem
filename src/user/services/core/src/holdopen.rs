#![no_std]
#![no_main]

use rt::*;

// A program that CANNOT EXIT ON ITS OWN, for a test that needs a live process to stay live.
//
// `process_service_lists_every_started_program` starts `log_service` and `device_manager` and
// asserts both are still listed. That is a race the test does not own: a program whose bootstrap
// channel is finished with is entitled to exit, and one of the two has twice been reaped before the
// list request was drained - once on riscv64, once on x86_64, in runs of two hundred - which reads
// exactly like a ProcessService bookkeeping defect and is not one.
//
// So the fixture is the answer rather than another retry. This blocks on its bootstrap channel
// forever: the test holds the other end, nothing is ever sent, and the process is alive for exactly
// as long as the test keeps it. "Both are listed" becomes a fact about the service rather than a
// hope about scheduling.
//
// It does not read its bootstrap capabilities on purpose - it needs none, and taking them would give
// it a way to fail. A blocking wait on a channel whose peer the test holds open returns only when
// the test drops it, which is the shutdown path: peer closed, exit.
//
// **IT DID NOT BLOCK, AND HAD NEVER BLOCKED (2026-08-19).** The loop below was `recv_into`, which is
// NOT a blocking receive: it issues `SYS_CHANNEL_RECV` and answers `ERR_WOULD_BLOCK` as `Empty` -
// and `Empty` broke out of the loop. So the program that "cannot exit on its own" exited on its
// first instruction after entry, every time, and `process_service_lists_every_started_program`
// passed exactly when the scheduler happened to drain the list request before the children ran.
// That is the intermittency recorded against that test on riscv64 2026-08-10, x86_64 2026-08-13 and
// x86_64 2026-08-19; the fixture introduced to remove the race had the race inside it.
//
// `wait` is the one that sleeps: it returns 0 when the channel is readable and a negative error when
// it is not going to be - including the peer-closed case, which is the shutdown path. The receive
// stays, so a message still ends the program, and `Empty` now means "somebody else took it" and goes
// back to waiting instead of exiting.
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 32] = [0u8; 32];
	unsafe {
		loop {
			// SLEEP UNTIL THERE IS SOMETHING, at ~0% CPU. A spin would be worse than the exit it
			// replaces: the kernel harness drains with `run_until_idle`, and a process that never
			// stops running never lets the scheduler go idle.
			if wait(bootstrap, 0) < 0 {
				break;
			}
			// STATICALLY LINKED and PINNED into the init package, which is what makes it reachable
			// at all: the test that needs it drives ProcessService with no storage client, so
			// programs come from the package rather than from the volume, and a volume-staged tool
			// cannot be launched there. It is a probe rather than a tool for the same reason -
			// `resource_probe` beside it is pinned too - and the manifest enforces that a tool is
			// volume-staged.
			match recv_into(bootstrap, &mut buf) {
				// Anything at all is a request to stop - the test does not send, so this is only
				// reached if somebody decides to. Answering it beats ignoring it.
				RecvInto::Received(_) => break,
				// The test let go of its end: there is nothing left to be held open for.
				RecvInto::PeerClosed => break,
				// `wait` said readable and the message was gone: another reader took it. Back to
				// waiting - this is the one case that is not a reason to stop.
				RecvInto::Empty => continue,
				// The receive itself failed - a handle without the right, or one that is not a
				// channel. Not reachable in the test that uses this, and breaking beats spinning.
				RecvInto::Failed => break,
			}
		}
	}
	exit();
}
