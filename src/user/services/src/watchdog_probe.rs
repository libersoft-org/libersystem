// WatchdogProbe - the supervisor's managed canary service.
//
// ServiceManager owns this program outright (it is not in the service manifest; the
// supervisor spawns it directly). It exists to exercise the supervisor's restart and
// watchdog machinery end to end against a service the supervisor fully controls - the
// channel re-wiring that makes transparent restart of a real service hard does not
// apply here, because no other component holds a channel to the canary.
//
// It reports "WatchdogProbe: online" over its bootstrap channel (the supervisor keeps
// the other end) and then serves that same channel:
//   - a heartbeat probe is answered uniformly by the rt serve loop (a "PONG"), which
//     is how the watchdog proves the canary is responsive;
//   - a "CRASH" request faults the process (a real crash the supervisor restarts);
//   - a "HANG" request parks the thread forever - alive but unresponsive, so the next
//     heartbeat goes unanswered and the watchdog kills and restarts the canary.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// report in over the channel the supervisor probes and commands us on.
		send_blocking(bootstrap, b"WatchdogProbe: online", 0);

		// serve. The heartbeat is answered by the serve loop itself; CRASH and HANG drive
		// the two failure modes the supervisor is built to handle.
		let mut request: [u8; 64] = [0u8; 64];
		let mut reply: [u8; 16] = [0u8; 16];
		serve(bootstrap, &mut request, &mut reply, |req: &[u8], _handle: u64, _reply: &mut [u8], _reply_handle: &mut u64| -> Option<usize> {
			if req == b"CRASH" {
				// a deliberate fault: write to the unmapped null page. The kernel records
				// the fault, tears us down, and our channel peer-closes - the crash the
				// supervisor's restart policy is built to recover from.
				(0 as *mut u8).write_volatile(0);
			} else if req == b"HANG" {
				// park forever on a channel that never becomes readable (we hold both ends
				// and never send): the thread sleeps at ~0% CPU but never returns to answer
				// the next heartbeat, so the watchdog detects us as hung.
				if let Some((idle, _peer)) = channel() {
					loop {
						wait(idle, 0);
					}
				}
			}
			None
		});
	}
	exit();
}
