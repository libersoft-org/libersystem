// spool_probe - SpoolService's real client, for the kernel's sink scenario.
//
// It holds `spool` and nothing else: no catalogue, no backend, no device authority. The harness that runs it
// plays the printers and decides how each one answers; this program stages, submits, cancels and reads status,
// and says what it saw. Each mode is one step of the scenario run as its own process on a fresh connection, so
// a mode that ends by exiting is exactly an owner going away.
//
// THE DOCUMENTS ARE PATTERNS THE HARNESS KNOWS: `pattern(seed, n)`, with the seeds and lengths below. The
// harness compares every byte its sinks received against them, so a byte sent twice, out of order, or from a
// job that was never submitted is a failure there even when everything here passed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{DocumentLanguage, Error, JobCause, JobState, JobStatus, LaunchContext, Observation, PrinterId, PrinterInfo, job, spool};
use rt::*;
use services::capability_names::*;
use wire::Reader;

// The same formula as the harness's.
fn pattern(seed: u8, n: usize) -> u8 {
	((n as u32).wrapping_mul(7).wrapping_add(u32::from(seed) * 13).wrapping_add(n as u32 >> 8)) as u8
}

fn document(seed: u8, length: usize) -> Vec<u8> {
	(0..length).map(|n| pattern(seed, n)).collect()
}

fn fail(mode: &str, why: &str) -> ! {
	print(format!("spool-probe: FAIL {mode}: {why}\n").as_bytes());
	exit();
}

fn pass(mode: &str, detail: &str) -> ! {
	print(format!("spool-probe: PASS {mode}{detail}\n").as_bytes());
	exit();
}

struct Probe {
	mode: &'static str,
	spool: u64,
}

fn job_client(chan: u64) -> job::Client<ChannelTransport> {
	job::Client::with_deadline(ChannelTransport { chan }, clock() + 500)
}

fn terminal(state: JobState) -> bool {
	matches!(state, JobState::Transferred | JobState::Failed | JobState::Cancelled)
}

impl Probe {
	fn client(&self) -> spool::Client<ChannelTransport> {
		spool::Client::with_deadline(ChannelTransport { chan: self.spool }, clock() + 500)
	}

	fn fail(&self, why: &str) -> ! {
		fail(self.mode, why)
	}

	fn printers(&self) -> Vec<PrinterInfo> {
		match self.client().printers() {
			Some(Ok(printers)) => printers,
			_ => self.fail("the printers could not be listed"),
		}
	}

	// A printer by the model its device ID names, waiting for it to be attached - a replacement or a reset one
	// takes a moment.
	fn printer(&self, model: &str) -> PrinterInfo {
		let deadline = clock() + 1000;
		loop {
			if let Some(printer) = self.printers().into_iter().find(|printer| printer.model == model && printer.available) {
				return printer;
			}
			if clock() >= deadline {
				self.fail(&format!("no available printer named {model}"));
			}
			sleep_until(clock() + 2);
		}
	}

	fn create(&self, printer: &PrinterId, length: u32) -> Result<u64, Error> {
		match self.client().create(printer, &DocumentLanguage::Postscript, &length) {
			Some(result) => result,
			None => self.fail("create was not answered"),
		}
	}

	fn created(&self, printer: &PrinterId, length: u32) -> u64 {
		match self.create(printer, length) {
			Ok(job) => job,
			Err(error) => self.fail(&format!("a job of {length} bytes was refused: {error:?}")),
		}
	}

	// Stage bytes a frame at a time; each frame is taken whole.
	fn stage(&self, job: u64, bytes: &[u8]) {
		for frame in bytes.chunks(4096) {
			match job_client(job).write(frame) {
				Some(Ok(taken)) if taken as usize == frame.len() => {}
				other => self.fail(&format!("a frame of {} bytes was not taken whole: {other:?}", frame.len())),
			}
		}
	}

	fn submit(&self, job: u64) -> JobStatus {
		match job_client(job).submit() {
			Some(Ok(status)) => status,
			other => self.fail(&format!("submit failed: {other:?}")),
		}
	}

	fn status(&self, job: u64) -> JobStatus {
		match job_client(job).status() {
			Some(Ok(status)) => status,
			other => self.fail(&format!("status failed: {other:?}")),
		}
	}

	fn finished(&self, job: u64) -> JobStatus {
		let deadline = clock() + 2000;
		loop {
			let status = self.status(job);
			if terminal(status.state) {
				return status;
			}
			if clock() >= deadline {
				self.fail(&format!("a job did not finish: {status:?}"));
			}
			sleep_until(clock() + 2);
		}
	}

	// A job that was sent whole: every byte acknowledged, no cause, nothing uncertain.
	fn transferred(&self, job: u64, declared: u32) {
		let status = self.finished(job);
		if status.state != JobState::Transferred || status.acknowledged != declared || status.cause.is_some() || status.delivery_uncertain {
			self.fail(&format!("a job did not end transferred with every byte acknowledged: {status:?}"));
		}
	}

	// One whole document, created, staged and submitted.
	fn send(&self, printer: &PrinterId, seed: u8, length: usize) -> u64 {
		let job = self.created(printer, length as u32);
		self.stage(job, &document(seed, length));
		let submitted = self.submit(job);
		if submitted.state == JobState::Writing || submitted.staged != length as u32 {
			self.fail(&format!("a submitted job did not leave writing: {submitted:?}"));
		}
		job
	}
}

// The job interface's `write`, by hand, with more bytes than any frame may carry.
fn oversized_write(job: u64) -> Option<Error> {
	let mut frame = Vec::new();
	frame.extend_from_slice(&1u16.to_le_bytes());
	frame.extend_from_slice(&0x5eed_u32.to_le_bytes());
	frame.extend_from_slice(&5000u16.to_le_bytes());
	frame.resize(frame.len() + 5000, 0x41);
	if !send_blocking(job, &frame, 0) {
		return None;
	}
	let mut buf = [0u8; 64];
	let Received::Message { len, .. } = recv_blocking(job, &mut buf) else { return None };
	let mut reader = Reader::new(&buf[..len]);
	if reader.u32()? != 0x5eed || reader.tag()? {
		return None;
	}
	Error::read(&mut reader)
}

// What `printers` says before any job: the three sinks, the languages their device IDs support, the port as
// read and the details only one of them has evidence for; then admission's refusals and one small job.
fn inventory(probe: &Probe) -> ! {
	let printers = probe.printers();
	if printers.len() != 3 {
		probe.fail(&format!("{} printers were listed, not the harness's three", printers.len()));
	}
	let alpha = probe.printer("Sink Alpha");
	let beta = probe.printer("Sink Beta");
	let gamma = probe.printer("Sink Gamma");
	if alpha.languages != [DocumentLanguage::Postscript] || beta.languages != [DocumentLanguage::Postscript] || !gamma.languages.is_empty() {
		probe.fail("the languages did not follow the device IDs");
	}
	if alpha.max_job_bytes != 4 * 1024 * 1024 || alpha.queued != 0 || alpha.active {
		probe.fail("the bounds or the queue were not as admitted");
	}
	let benign = |printer: &PrinterInfo| printer.status.paper_empty == Observation::No && printer.status.selected == Observation::Yes && printer.status.error == Observation::No;
	if !benign(&alpha) || !benign(&beta) {
		probe.fail("the benign port reading was not reported as read");
	}
	if alpha.status.cover_open != Observation::Unsupported || alpha.status.jam != Observation::Unsupported {
		probe.fail("a detail without evidence was reported as anything but unsupported");
	}
	if beta.status.cover_open != Observation::No || beta.status.jam != Observation::No {
		probe.fail("the details a backend had evidence for were not reported");
	}
	// ADMISSION'S REFUSALS, each before anything is reserved.
	let refused = |printer: &PrinterId, language: DocumentLanguage, length: u32| probe.client().create(printer, &language, &length);
	if !matches!(refused(&gamma.id, DocumentLanguage::Postscript, 10), Some(Err(Error::Unsupported))) {
		probe.fail("a printer without PostScript evidence admitted PostScript");
	}
	if !matches!(refused(&alpha.id, DocumentLanguage::Pcl, 10), Some(Err(Error::Unsupported))) {
		probe.fail("a language this system does not send was admitted");
	}
	if !matches!(refused(&alpha.id, DocumentLanguage::Postscript, 0), Some(Err(Error::Invalid))) || !matches!(refused(&alpha.id, DocumentLanguage::Postscript, 4 * 1024 * 1024 + 1), Some(Err(Error::Invalid))) {
		probe.fail("a zero or oversized length was admitted");
	}
	let mut stale = alpha.id.clone();
	stale.attachment += 1;
	if !matches!(refused(&stale, DocumentLanguage::Postscript, 10), Some(Err(Error::Stale))) {
		probe.fail("another attachment generation was admitted");
	}
	let mut unknown = alpha.id.clone();
	unknown.slot = 99;
	if !matches!(refused(&unknown, DocumentLanguage::Postscript, 10), Some(Err(Error::NotFound))) {
		probe.fail("an unknown printer was admitted");
	}
	// TWO JOBS A GRANT CONTEXT, and a terminal job's record still counts until its channel closes.
	let overrun = probe.created(&alpha.id, 10);
	let job = probe.created(&alpha.id, 10);
	if !matches!(probe.create(&alpha.id, 10), Err(Error::Exhausted)) {
		probe.fail("a third job was admitted to one grant context");
	}
	if !matches!(job_client(overrun).write(&[0u8; 11]), Some(Err(Error::Invalid))) {
		probe.fail("a write past the declared length was taken");
	}
	let status = probe.status(overrun);
	if status.state != JobState::Failed || status.cause != Some(JobCause::SizeLimit) || status.staged != 0 {
		probe.fail(&format!("the overrun did not fail with size-limit, taking none of the frame: {status:?}"));
	}
	if !matches!(probe.create(&alpha.id, 10), Err(Error::Exhausted)) {
		probe.fail("a failed job's record stopped counting while its channel was open");
	}
	close(overrun);
	// Its channel closed: the record is gone, and a job can be made again - and abandoned by closing.
	let deadline = clock() + 200;
	let spare = loop {
		match probe.create(&alpha.id, 1) {
			Ok(spare) => break spare,
			Err(Error::Exhausted) if clock() < deadline => sleep_until(clock() + 2),
			Err(error) => probe.fail(&format!("the closed record was never released: {error:?}")),
		}
	};
	close(spare);
	// STAGING: whole frames, an oversized one refused before it reaches the job, and submit only when exact.
	let body = document(1, 10);
	probe.stage(job, &body[..5]);
	if !matches!(job_client(job).submit(), Some(Err(Error::Invalid))) {
		probe.fail("a short job was submitted");
	}
	if oversized_write(job) != Some(Error::Invalid) {
		probe.fail("an oversized frame was not refused as invalid");
	}
	let status = probe.status(job);
	if status.state != JobState::Writing || status.staged != 5 {
		probe.fail(&format!("the oversized frame touched the job: {status:?}"));
	}
	probe.stage(job, &body[5..]);
	let first = probe.submit(job);
	let again = probe.submit(job);
	if first.state == JobState::Writing || again.state == JobState::Writing || again.declared != 10 {
		probe.fail("a repeated submit did not answer the job's state");
	}
	if !matches!(job_client(job).write(&[0u8]), Some(Err(Error::Invalid))) {
		probe.fail("a write after submission was taken");
	}
	probe.transferred(job, 10);
	// STATUS REMAINS after the job is over.
	let status = probe.status(job);
	if status.state != JobState::Transferred {
		probe.fail("the final status was not kept");
	}
	pass("inventory", "")
}

// Two jobs to one printer that takes a prefix of every write and answers `again` first: each arrives whole,
// once, in submission order.
fn two(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let first = probe.send(&alpha.id, 2, 10_000);
	let second = probe.send(&alpha.id, 3, 6_000);
	probe.transferred(first, 10_000);
	probe.transferred(second, 6_000);
	pass("two", "")
}

// One printer stalled on `again`, reporting paper empty; another printer, the supervisor and this client go on.
// When the harness lets the stalled one go, its job finishes whole.
fn stall(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let beta = probe.printer("Sink Beta");
	let stalled = probe.send(&alpha.id, 4, 100);
	let other = probe.send(&beta.id, 5, 5_000);
	probe.transferred(other, 5_000);
	if terminal(probe.status(stalled).state) {
		probe.fail("the stalled job ended before the stall was over");
	}
	// THE PORT, AS READ WHILE STALLED: paper empty, and nothing about readiness claimed.
	let deadline = clock() + 1000;
	loop {
		let now = probe.printers().into_iter().find(|printer| printer.id == alpha.id).unwrap_or_else(|| probe.fail("the stalled printer disappeared"));
		if now.status.paper_empty == Observation::Yes && now.active {
			break;
		}
		if clock() >= deadline {
			probe.fail("the stalled printer's port reading was never reported");
		}
		sleep_until(clock() + 2);
	}
	print(b"spool-probe: stalled\n");
	probe.transferred(stalled, 100);
	pass("stall", "")
}

// Nothing unsubmitted is ever sent: one job closed half-written, another written whole and left when the
// owner exits.
fn abandon(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let closed = probe.created(&alpha.id, 8_000);
	probe.stage(closed, &document(6, 8_000)[..4096]);
	close(closed);
	let whole = probe.created(&alpha.id, 3_000);
	probe.stage(whole, &document(7, 3_000));
	print(b"spool-probe: PASS abandon, exiting without submitting\n");
	exit();
}

// An owner that dies mid-write.
fn crash(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let job = probe.created(&alpha.id, 8_000);
	probe.stage(job, &document(8, 8_000)[..4096]);
	print(b"spool-probe: PASS crash, exiting mid-write\n");
	exit();
}

// An owner that submits and exits at once: the job is committed, and completes unwatched.
fn detach(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	probe.send(&alpha.id, 9, 9_000);
	print(b"spool-probe: PASS detach, exiting after submitting\n");
	exit();
}

// The printer is withdrawn mid-job: the job ends unplugged with what was acknowledged, and says whether more
// may have gone. Its replacement is another printer, and receives only what is sent to it.
fn unplug(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let job = probe.send(&alpha.id, 10, 40_000);
	let status = probe.finished(job);
	if status.state != JobState::Failed || status.cause != Some(JobCause::Unplugged) || status.acknowledged == 0 || status.acknowledged >= 40_000 {
		probe.fail(&format!("the withdrawn printer's job did not end unplugged after a partial delivery: {status:?}"));
	}
	print(format!("spool-probe: unplugged acknowledged={} uncertain={}\n", status.acknowledged, status.delivery_uncertain).as_bytes());
	let replacement = probe.printer("Sink Alpha");
	if replacement.id.generation == alpha.id.generation && replacement.id.binding_generation == alpha.id.binding_generation {
		probe.fail("the replacement was not a new generation");
	}
	if !matches!(probe.create(&alpha.id, 10), Err(Error::NotFound)) {
		probe.fail("the withdrawn printer still admitted jobs");
	}
	let fresh = probe.send(&replacement.id, 11, 2_000);
	probe.transferred(fresh, 2_000);
	pass("unplug", "")
}

// A write fails and the reset succeeds: the active job fails as a backend error with its delivery uncertain,
// the queued one fails as reset, the printer comes back under a new attachment, and the old one is stale. The
// harness holds the printer on `again` until both jobs are in, then fails the next write.
fn reset(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let active = probe.send(&alpha.id, 12, 5_000);
	let queued = probe.send(&alpha.id, 13, 3_000);
	// Both jobs are in: the harness fails the next write now, and not before.
	print(b"spool-probe: queued\n");
	let failed = probe.finished(active);
	if failed.state != JobState::Failed || failed.cause != Some(JobCause::BackendError) || !failed.delivery_uncertain || failed.acknowledged != 0 {
		probe.fail(&format!("the failed write did not end its job as an uncertain backend error: {failed:?}"));
	}
	let dropped = probe.finished(queued);
	if dropped.state != JobState::Failed || dropped.cause != Some(JobCause::Reset) || dropped.acknowledged != 0 || dropped.delivery_uncertain {
		probe.fail(&format!("the queued job did not end with the reset: {dropped:?}"));
	}
	let back = probe.printer("Sink Alpha");
	if back.id.attachment == alpha.id.attachment {
		probe.fail("the reset printer kept its attachment");
	}
	close(active);
	close(queued);
	if !matches!(probe.create(&alpha.id, 10), Err(Error::Stale)) {
		probe.fail("the old attachment was admitted after the reset");
	}
	let fresh = probe.send(&back.id, 14, 1_000);
	probe.transferred(fresh, 1_000);
	pass("reset", "")
}

// A write fails and so does the reset: the printer is unavailable, and admits nothing.
fn recovery(probe: &Probe) -> ! {
	let alpha = probe.printer("Sink Alpha");
	let job = probe.send(&alpha.id, 15, 1_000);
	let failed = probe.finished(job);
	if failed.state != JobState::Failed || failed.cause != Some(JobCause::BackendError) {
		probe.fail(&format!("the failed write did not end its job: {failed:?}"));
	}
	let deadline = clock() + 1000;
	let gone = loop {
		let now = probe.printers().into_iter().find(|printer| printer.model == "Sink Alpha").unwrap_or_else(|| probe.fail("the printer disappeared"));
		if !now.available {
			break now;
		}
		if clock() >= deadline {
			probe.fail("a failed reset left the printer available");
		}
		sleep_until(clock() + 2);
	};
	if !matches!(probe.create(&gone.id, 10), Err(Error::Io)) {
		probe.fail("an unavailable printer admitted a job");
	}
	pass("recovery", "")
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let Some(context) = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) else { exit() };
	let spool = recv_tagged(bootstrap, &mut buf, CAP_SPOOL).unwrap_or(0);
	let mode: &'static str = match context.arguments.as_str() {
		"inventory" => "inventory",
		"two" => "two",
		"stall" => "stall",
		"abandon" => "abandon",
		"crash" => "crash",
		"detach" => "detach",
		"unplug" => "unplug",
		"reset" => "reset",
		"recovery" => "recovery",
		_ => fail("spool", "no such mode"),
	};
	if spool == 0 {
		fail(mode, "the spool grant was not delivered");
	}
	let probe = Probe { mode, spool };
	match mode {
		"inventory" => inventory(&probe),
		"two" => two(&probe),
		"stall" => stall(&probe),
		"abandon" => abandon(&probe),
		"crash" => crash(&probe),
		"detach" => detach(&probe),
		"unplug" => unplug(&probe),
		"reset" => reset(&probe),
		_ => recovery(&probe),
	}
}
