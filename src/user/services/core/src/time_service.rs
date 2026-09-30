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
//
// THE WALL CLOCK COUNTS ON THE BOOT-TIME CLOCK, which takes every sleep in, and not on the monotonic ticks, which leave
// it out: a machine asleep all night wakes with its wall clock a night later without anyone telling it. The resume
// notice - this service declares the sleep notice - reads the RTC again to correct the drift, and a service that read no
// time at its start reads again at each request until the kernel answers one.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::time::{self, Service};
use proto::system::{Error, SleepState, Timestamp, network, sleep_notice};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/roles_time_service.rs"));

const NANOS_PER_SEC: u64 = 1_000_000_000;
// The NTP server TimeService disciplines against (resolved via DNS, queried over UDP).
const NTP_SERVER: &str = "time.cloudflare.com";

// TimeService state: the Unix epoch (seconds, UTC) at boot, so the current wall time is this plus the boot-time clock.
// Seeded at boot from the RTC, then refined by an SNTP query.
struct Time {
	epoch_at_boot: u64,
	// Whether a time has been read at all: the kernel answers 0 for an RTC it cannot read yet.
	seeded: bool,
}

impl Time {
	// The wall clock now: the epoch at boot plus the seconds since, sleeps included.
	fn now_unix(&self) -> u64 {
		self.epoch_at_boot + clock_boot_ns() / NANOS_PER_SEC
	}

	// Reset the offset so the wall clock reads `unix` at this instant.
	fn set_now(&mut self, unix: u64) {
		self.epoch_at_boot = unix.saturating_sub(clock_boot_ns() / NANOS_PER_SEC);
		self.seeded = true;
	}

	// THE RTC, READ AGAIN: at the resume, and at each request while nothing has been read.
	fn read_rtc(&mut self) {
		let unix = clock_rtc();
		if unix != 0 {
			self.set_now(unix);
		}
	}
}

impl Service for Time {
	// The current wall-clock instant.
	fn now(&mut self) -> Result<Timestamp, Error> {
		if !self.seeded {
			self.read_rtc();
		}
		Ok(Timestamp { unix_secs: self.now_unix() })
	}
}

// THE SLEEP NOTICE: the resume reads the RTC again, and nothing else asks anything of this service.
impl sleep_notice::Service for Time {
	fn announce(&mut self, _state: SleepState) -> Result<(), Error> {
		Ok(())
	}

	fn hold_writes(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn release_writes(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn resumed(&mut self, _state: SleepState) -> Result<(), Error> {
		self.read_rtc();
		Ok(())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
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
	let mut time = Time { epoch_at_boot: 0, seeded: false };
	time.read_rtc();

	// 3. report in - boot does not wait on the network - then discipline against
	//    SNTP best-effort; on failure the RTC seeding stands.
	send_blocking(bootstrap, b"TimeService: online", 0);
	if netsvc != 0 {
		discipline_sntp(netsvc, &mut time);
		close(netsvc);
	}

	// 4. serve now() until the client side closes, and ServiceManager's notices on the control channel beside it.
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 256] = [0u8; 256];
	serve_multi_rooted(&[service, bootstrap], &mut request, &mut reply, |origin, _chan, req, handle, out, reply_handle| -> Option<usize> {
		if origin == 1 {
			return sleep_notice::dispatch(&mut time, req, handle, out, reply_handle);
		}
		time::dispatch(&mut time, req, handle, out, reply_handle)
	});
	exit();
}

// Refine the offset against an NTP server: resolve its name, query it over SNTP, and
// on a reply reset the wall clock to the returned Unix time. Best-effort - any
// failure (no DNS, no route, no reply) leaves the RTC-seeded offset in place.
fn discipline_sntp(netsvc: u64, time: &mut Time) {
	let mut net = network::Client::new(ChannelTransport { chan: netsvc });
	// THE FIRST CANDIDATE, and only the first: this is one best-effort query against one server, and
	// walking a name's whole address list to set a clock would turn a background refinement into a
	// sequence of timeouts.
	let Some(Ok(addresses)) = net.resolve(NTP_SERVER) else {
		return;
	};
	let Some(server) = addresses.into_iter().next() else {
		return;
	};
	if let Some(Ok(unix)) = net.sntp(&proto::system::ScopedAddress::global(server)) {
		time.set_now(unix);
	}
}
