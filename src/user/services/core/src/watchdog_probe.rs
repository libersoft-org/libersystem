// WatchdogProbe - the supervisor's managed canary service.
//
// ServiceManager owns this program outright (it is not in the service manifest; the
// supervisor spawns it directly). It exists to exercise the supervisor's restart and
// watchdog machinery end to end against a service the supervisor fully controls, and
// - as a standing client with a CONFIG grant - to prove the transparent-restart
// broker: a "CHECK" command makes it re-resolve ConfigService through the broker
// (its bootstrap channel) and run a typed request against the freshly restarted
// instance, the client-survives-a-service-restart proof.
//
// It reports "WatchdogProbe: online" over its bootstrap channel (the supervisor keeps
// the other end) and then serves that same channel:
//   - a heartbeat probe is answered uniformly by the rt serve loop (a "PONG"), which
//     is how the watchdog proves the canary is responsive;
//   - a "CRASH" request faults the process (a real crash the supervisor restarts);
//   - a "HANG" request parks the thread forever - alive but unresponsive, so the next
//     heartbeat goes unanswered and the watchdog kills and restarts the canary;
//   - a "CHECK" request re-resolves CONFIG over the broker, round-trips a typed
//     config get, and asserts an un-granted name (STORAGE) is denied, replying with
//     its verdict.

#![no_std]
#![no_main]

use ipc_client::ChannelTransport;
use proto::system::config;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// report in over the channel the supervisor probes and commands us on.
		send_blocking(bootstrap, b"WatchdogProbe: online", 0);

		// serve. The heartbeat is answered by the serve loop itself; CRASH and HANG drive
		// the two failure modes the supervisor is built to handle; CHECK proves the
		// transparent-restart broker from the client side.
		let mut request: [u8; 64] = [0u8; 64];
		let mut reply: [u8; 32] = [0u8; 32];
		serve(bootstrap, &mut request, &mut reply, |req: &[u8], _handle: &mut u64, out: &mut [u8], _reply_handle: &mut u64| -> Option<usize> {
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
			} else if req == b"CHECK" {
				// The transparent-restart proof, from the client side. Our old view of
				// ConfigService (if we had one) died with the crashed instance; the durable
				// reference is the NAME, so resolve CONFIG through the broker - the reply
				// is a fresh channel to the restarted instance - and prove it WORKS with a
				// typed round-trip. Then prove the grant discipline: STORAGE is not in our
				// grant set, so its resolve must be denied. The broker serves our resolves
				// on this same channel while it waits for this verdict (both sides are
				// single-threaded, so the interleaving is deterministic).
				let verdict: &[u8] = match resolve(bootstrap, CAP_CONFIG) {
					Some(chan) => {
						let got: bool = matches!(config::Client::new(ChannelTransport { chan }).get("system.name"), Some(Ok(v)) if v == "LiberSystem");
						let denied: bool = resolve(bootstrap, CAP_STORAGE).is_none();
						close(chan);
						match (got, denied) {
							(true, true) => b"CONFIG-OK DENIED-OK",
							(true, false) => b"GRANT-LEAK",
							_ => b"CONFIG-FAILED",
						}
					}
					None => b"RESOLVE-FAILED",
				};
				out[..verdict.len()].copy_from_slice(verdict);
				return Some(verdict.len());
			}
			None
		});
	}
	exit();
}
