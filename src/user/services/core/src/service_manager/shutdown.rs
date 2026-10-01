// `liber:process@1/system-shutdown` - THE ORDERLY POWER-OFF, AND NOTHING ELSE, served here on connections this supervisor
// mints itself for the roles that name it: the power-state service's critical battery and a thermal policy's `_CRT`
// reach the one sequence the admin channel's `!poweroff` runs - every service stopped in reverse dependency order,
// LogService's last batch flushed, then `system-power` - without the admin channel, which can stop any service.
//
// ANSWERED AT ACCEPTANCE, before the sequence runs: the sequence ends with the machine off, and a requester that is one of
// the services it stops would otherwise wait on its own ending. One asked while a sleep's transaction runs ends the
// transaction at its next step, as the admin door does, and runs once it has ended.

use super::*;
use core::sync::atomic::{AtomicU64, Ordering};
use proto::system::system_shutdown;

// THE NEAR ENDS OF EVERY CONNECTION, read by the standing loop's wait set each round.
pub(super) const MAX_SHUTDOWN_CLIENTS: usize = 4;
pub(super) static SHUTDOWN_CLIENTS: [AtomicU64; MAX_SHUTDOWN_CLIENTS] = [const { AtomicU64::new(0) }; MAX_SHUTDOWN_CLIENTS];

// A FRESH CONNECTION, as `sleep::mint` makes one. None when the table is full.
pub(super) fn mint() -> Option<u64> {
	let slot = SHUTDOWN_CLIENTS.iter().find(|slot| slot.load(Ordering::Relaxed) == 0)?;
	let (near, far): (u64, u64) = channel()?;
	slot.store(near, Ordering::Relaxed);
	Some(far)
}

fn retire(near: u64) {
	for slot in SHUTDOWN_CLIENTS.iter() {
		if slot.load(Ordering::Relaxed) == near {
			slot.store(0, Ordering::Relaxed);
		}
	}
	close(near);
}

struct Api {
	asked: bool,
}

impl system_shutdown::Service for Api {
	fn power_off(&mut self) -> Result<(), Error> {
		self.asked = true;
		Ok(())
	}
}

// ONE REQUEST: answered, and whether the orderly power-off was asked for. A closed connection is retired.
pub(super) fn serve(near: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(near, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return false,
		PolledCaps::Closed => {
			retire(near);
			return false;
		}
	};
	let op: u16 = if len >= 2 { u16::from_le_bytes([buf[0], buf[1]]) } else { 0 };
	if op == CONNECT_OP || op == HEARTBEAT_OP {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if op == HEARTBEAT_OP {
			let _ = try_send(near, b"PONG", 0);
		} else {
			let minted: u64 = mint().unwrap_or(0);
			if !try_send(near, &[], minted) && minted != 0 {
				close(minted);
			}
		}
		return false;
	}
	let mut api = Api { asked: false };
	let mut reply = [0u8; 64];
	let mut reply_handles = wire::Handles::new();
	let written = system_shutdown::dispatch(&mut api, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
		close(leftover);
	}
	if let Some(written) = written {
		let _ = try_send(near, &reply[..written], 0);
	}
	api.asked
}
