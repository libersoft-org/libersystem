// TimeService - the userspace wall clock.
//
// ServiceManager starts this program from the init package and hands it a bootstrap
// channel carrying its own NetworkService client (for the SNTP query) and the
// channel its clients reach it on. TimeService seeds its offset from the hardware
// RTC (an immediate, network-free UTC), reports in, then disciplines that offset
// against an NTP server over SNTP (best-effort). Over its service channel clients
// speak the generated `liber:system` Time bindings: `now` returns the current UTC as
// a typed `timestamp` (seconds since the Unix epoch), which the CLI renders as
// ISO-8601 / epoch / human.
//
// Wall-clock time is policy, not a kernel concern: the kernel offers only the
// monotonic clock and a raw RTC read; TimeService combines them. There is no
// client-facing "set the clock" - the only authority that moves wall time is
// TimeService's own RTC/NTP logic, which holds the network capability it needs - so
// no ambient authority can set the clock.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::time::{self, Service};
use proto::system::{Error, Timestamp, network};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/roles_time_service.rs"));

// The LAPIC monotonic clock runs at 100 Hz (ticks per second).
const TICKS_PER_SEC: u64 = 100;
// The NTP server TimeService disciplines against (resolved via DNS, queried over UDP).
const NTP_SERVER: &str = "time.cloudflare.com";

// TimeService state: the Unix epoch (seconds, UTC) at monotonic tick 0, so the
// current wall time is this plus the monotonic clock. Seeded at boot from the RTC,
// then refined by an SNTP query.
struct Time {
	epoch_at_tick0: u64,
}

impl Time {
	// The wall clock now: the epoch at tick 0 plus the monotonic seconds since.
	fn now_unix(&self) -> u64 {
		self.epoch_at_tick0 + unsafe { clock() } / TICKS_PER_SEC
	}

	// Reset the offset so the wall clock reads `unix` at this instant.
	fn set_now(&mut self, unix: u64) {
		self.epoch_at_tick0 = unix.saturating_sub(unsafe { clock() } / TICKS_PER_SEC);
	}
}

impl Service for Time {
	// The current wall-clock instant.
	fn now(&mut self) -> Result<Timestamp, Error> {
		Ok(Timestamp { unix_secs: self.now_unix() })
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. take the roles the plan says this service is handed: a private NetworkService
		//    connection for the SNTP query, and the channel clients reach us on. Checked against
		//    the GENERATED list rather than read by hand, so the tag, the object type and the
		//    rights are all checked and a wrong bootstrap is refused by the name of the role.
		//
		//    THE NETWORK ROLE IS OPTIONAL AND NOW BEHAVES LIKE IT. The manifest has said `optional`
		//    all along and this refused to start without it, which is a boot with a clock that
		//    would not tick because a NIC was missing. The RTC seeding below needs no network.
		let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
		if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
			fail_bootstrap(bootstrap, error.tag(), error.reason());
		}
		let (netsvc, service): (u64, u64) = (roles[0], roles[1]);

		// 2. seed the offset from the hardware RTC (an immediate, network-free UTC).
		let mut time = Time { epoch_at_tick0: 0 };
		time.set_now(clock_rtc());

		// 3. report in - boot does not wait on the network - then discipline against
		//    SNTP best-effort; on failure the RTC seeding stands.
		send_blocking(bootstrap, b"TimeService: online", 0);
		if netsvc != 0 {
			discipline_sntp(netsvc, &mut time);
			close(netsvc);
		}

		// 4. serve now() until the client side closes.
		let mut request: [u8; 256] = [0u8; 256];
		let mut reply: [u8; 256] = [0u8; 256];
		serve_multi(service, &mut request, &mut reply, |_chan, req, handle, out, reply_handle| -> Option<usize> { time::dispatch(&mut time, req, handle, out, reply_handle) });
	}
	exit();
}

// Refine the offset against an NTP server: resolve its name, query it over SNTP, and
// on a reply reset the wall clock to the returned Unix time. Best-effort - any
// failure (no DNS, no route, no reply) leaves the RTC-seeded offset in place.
fn discipline_sntp(netsvc: u64, time: &mut Time) {
	let mut net = network::Client::new(ChannelTransport { chan: netsvc });
	let server = match net.resolve(NTP_SERVER) {
		Some(Ok(ip)) => ip,
		_ => return,
	};
	if let Some(Ok(unix)) = net.sntp(&server) {
		time.set_now(unix);
	}
}
