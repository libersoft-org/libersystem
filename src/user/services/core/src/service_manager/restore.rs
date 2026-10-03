// `liber:process@1/hibernation-restore` - A RESTORE'S DOOR, served here on the connection this supervisor mints for the
// image component's `RESTORE` role at every start.
//
// `prepare-replacement`: every binding of the fresh boot stopped - DeviceManager's `stop-all`, the shutdown's teardown -
// and then answered, so the component replaces memory with devices quiet. Asked once the image is read, authenticated,
// held by the kernel and its header invalidated; the machine is about to be another, so this waits here for the answer.
//
// `boot-continues`: no image is restored, and the boot goes on. What waited for the verdict is let go: LogService's
// journal on the system volume, which is not handed over while an image found at mount may still replace memory - the
// system volume's writes are held until then, and a journal flush that waited on them would stall LogService and every
// program that logs through it.
//
// THE BRING-UP WAITS FOR THE VERDICT TOO. Until the image component has started, only what it needs starts - its
// dependencies and theirs - and once it has, the bring-up serves this door and nothing else until `boot-continues`:
// nothing the boot would start after it - the sessions, the shell, the display, the network's services - runs in a
// machine whose memory is about to be replaced, and none of it can write to the held volume and stall the bring-up that
// the restore's own door waits behind. Bounded: a component that gives no verdict in time lets the boot go on, said.
//
// UNLESS EVERY BINDING WAS STOPPED FOR A REPLACEMENT THAT DID NOT HAPPEN. Then the boot cannot go on - its disks were among
// them - and the image's header was invalidated before the stop was asked: whatever ends the wait - `boot-continues`
// after a refusal, no verdict in time, the component gone - the machine restarts through SystemManager's service and the
// next boot is a fresh one.

use super::*;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use proto::system::{device_sleep, hibernation_restore, system_power};

pub(super) const MAX_RESTORE_CLIENTS: usize = 2;
pub(super) static RESTORE_CLIENTS: [AtomicU64; MAX_RESTORE_CLIENTS] = [const { AtomicU64::new(0) }; MAX_RESTORE_CLIENTS];

// LOGSERVICE'S JOURNAL, owed until the image's verdict - see the head of this file. Zero when none is owed.
pub(super) static JOURNAL_OWED: AtomicU64 = AtomicU64::new(0);

// HOW LONG DEVICEMANAGER'S TEARDOWN MAY TAKE before the restore goes on without its answer. Ten minutes: the teardown
// forces every driver that does not answer its stop in time, one after another, and on an emulated port that is every
// driver - past the one minute this once was, after which the answer arrived on a door nobody read any more. The image
// component waits this long and one call more (`hibernation_service`'s `STOP_TICKS`).
const STOP_TICKS: u64 = 10 * 60 * TICKS_PER_SECOND;
// HOW LONG THE BRING-UP WAITS FOR THE VERDICT: the TPM's own minute, and an image as large as memory read, authenticated
// and decrypted - which on an emulated aarch64 or riscv64 machine took past the fifteen minutes this once was, with the
// boot then going on while the image component still asked for every binding to be stopped. An hour: the wait ends at
// the verdict, so only a component that never gives one is held this long.
const VERDICT_TICKS: u64 = 60 * 60 * TICKS_PER_SECOND;

// Whether `boot-continues` has been answered at this boot.
static VERDICT: AtomicBool = AtomicBool::new(false);
// Whether DeviceManager was asked to stop every binding for a replacement at this boot.
static STOPPED: AtomicBool = AtomicBool::new(false);

// THE SERVICES STARTED BEFORE THE VERDICT: the image component and every service it depends on, directly or not. All of
// them where no image component is in the manifest.
pub(super) fn before_verdict() -> [bool; N] {
	let Some(root) = index_of(b"hibernation_service") else { return [true; N] };
	let mut closure = [false; N];
	let mut stack: Vec<usize> = alloc::vec![root];
	while let Some(at) = stack.pop() {
		if closure[at] {
			continue;
		}
		closure[at] = true;
		stack.extend(MANIFEST[at].deps.iter().filter_map(|dep| index_of(dep)));
	}
	closure
}

// THE BRING-UP HELD at the image component's start, serving the restore's door until the verdict - see the head of this
// file.
pub(super) fn await_verdict(channels: &[u64; N], state: &[State; N], power: u64, buf: &mut [u8]) {
	let started = clock();
	let until = started.saturating_add(VERDICT_TICKS);
	let mut said = false;
	while !VERDICT.load(Ordering::Acquire) {
		let doors: Vec<u64> = RESTORE_CLIENTS.iter().map(|slot| slot.load(Ordering::Relaxed)).filter(|&door| door != 0).collect();
		if doors.is_empty() {
			print(b"ServiceManager: restore: the image component holds no door, so no verdict is waited for\n");
			break;
		}
		let now = clock();
		if now >= until {
			print(b"ServiceManager: restore: the image component gave no verdict in time - the boot goes on\n");
			break;
		}
		if !said && now >= started.saturating_add(2 * TICKS_PER_SECOND) {
			print(b"ServiceManager: restore: the boot waits for the image component's verdict on the image found at mount\n");
			said = true;
		}
		let _ = wait_any(&doors, until.min(now.saturating_add(TICKS_PER_SECOND)));
		for door in doors {
			serve(door, channels, state, buf);
		}
	}
	if STOPPED.load(Ordering::Acquire) {
		restart(power);
	}
}

// A BOOT WHOSE BINDINGS WERE STOPPED FOR A REPLACEMENT THAT DID NOT HAPPEN - see the head of this file. The call returns
// only when the machine did not restart.
fn restart(power: u64) {
	print(b"ServiceManager: restore: every binding was stopped for a replacement that did not happen - the machine restarts, and boots fresh\n");
	match system_power::Client::new(ChannelTransport { chan: power }).reboot() {
		Some(Ok(())) => print(b"ServiceManager: restore: the machine did not restart, and the power service reported no error\n"),
		Some(Err(_)) => print(b"ServiceManager: restore: the power service refused to restart the machine\n"),
		None => print(b"ServiceManager: restore: the power service did not answer; the machine is still running\n"),
	}
}

// A FRESH CONNECTION, narrowed to a client role's ceiling where it is handed over. None when the table is full.
pub(super) fn mint() -> Option<u64> {
	let slot = RESTORE_CLIENTS.iter().find(|slot| slot.load(Ordering::Relaxed) == 0)?;
	let (near, far): (u64, u64) = channel()?;
	slot.store(near, Ordering::Relaxed);
	Some(far)
}

fn retire(near: u64) {
	for slot in RESTORE_CLIENTS.iter() {
		if slot.load(Ordering::Relaxed) == near {
			slot.store(0, Ordering::Relaxed);
		}
	}
	close(near);
}

// LOGSERVICE'S JOURNAL HANDED OVER, if it is owed.
pub(super) fn release_journal(channels: &[u64; N]) {
	let journal = JOURNAL_OWED.swap(0, Ordering::AcqRel);
	if journal == 0 {
		return;
	}
	match index_of(b"log_service").map(|index| channels[index]).filter(|&channel| channel != 0) {
		Some(channel) => {
			send_blocking(channel, b"STORAGE", journal);
		}
		None => close(journal),
	}
}

struct Api<'a> {
	channels: &'a [u64; N],
	state: &'a [State; N],
}

impl hibernation_restore::Service for Api<'_> {
	fn prepare_replacement(&mut self) -> Result<(), Error> {
		let Some(dm) = index_of(b"device_manager").filter(|&index| self.state[index] == State::Ready && self.channels[index] != 0) else {
			print(b"ServiceManager: restore: DeviceManager does not run - memory is replaced with nothing to stop\n");
			return Ok(());
		};
		print(b"ServiceManager: restore: every binding of this boot is stopped before memory is replaced\n");
		STOPPED.store(true, Ordering::Release);
		match device_sleep::Client::with_deadline(ChannelTransport { chan: self.channels[dm] }, clock().saturating_add(STOP_TICKS)).stop_all() {
			Some(Ok(left)) if left.is_empty() => Ok(()),
			Some(Ok(left)) => {
				let line = alloc::format!("ServiceManager: restore: still bound after the teardown: {}\n", left.join(", "));
				print(line.as_bytes());
				Ok(())
			}
			Some(Err(error)) => Err(error),
			None => Err(Error::TimedOut),
		}
	}

	fn boot_continues(&mut self) -> Result<(), Error> {
		// AFTER THE BINDINGS WERE STOPPED the boot does not go on - the verdict's wait restarts the machine - and the journal
		// is not handed to a volume whose disk has no driver.
		if !STOPPED.load(Ordering::Acquire) {
			print(b"ServiceManager: restore: no image is restored - the boot goes on\n");
			release_journal(self.channels);
		}
		VERDICT.store(true, Ordering::Release);
		Ok(())
	}
}

// ONE REQUEST, answered. A closed connection is retired.
pub(super) fn serve(near: u64, channels: &[u64; N], state: &[State; N], buf: &mut [u8]) {
	let (len, mut handles) = match try_recv_caps(near, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			retire(near);
			return;
		}
	};
	let request = buf[..len].to_vec();
	let mut reply = [0u8; 64];
	let mut reply_handles = wire::Handles::new();
	let written = hibernation_restore::dispatch(&mut Api { channels, state }, &request, &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
		close(leftover);
	}
	if let Some(written) = written {
		let _ = try_send(near, &reply[..written], 0);
	}
}
