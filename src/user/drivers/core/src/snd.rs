// The sound driver's completion contract, as pure decisions: what a period actually did, tested on
// the host through the crate's seam.
//
// THE DEBT THAT LIVES HERE (DRV-009). The playback path submitted a period, blocked on the device's
// MSI-X interrupt, reaped whatever the used ring happened to hold, and replied "OK" - four separate
// pieces of trust, none of them checked:
//
//   ANY INTERRUPT WAS A COMPLETION. An MSI-X vector is shared with the queues the driver does not
//     drive, and an interrupt is not evidence that THIS submission finished. The used ring is.
//   THE STATUS WORD WAS NEVER READ. The device writes `virtio_snd_pcm_status` into the last segment
//     of the chain, and it is where a device says it could not play what it was given.
//   A FAILED PERIOD WAS PLAYED AGAIN. The same DMA page holds the next period, so a period that was
//     never consumed is overwritten by the next one and silently lost - or, worse, a period the
//     device is still reading is overwritten while it reads it.
//   AND THE ANSWER WAS ALWAYS SUCCESS, so none of the three could be noticed from outside.
//
// Each is a decision rather than a device operation, which is why they are here: a host test can
// hold them against a ring it writes by hand.

// The virtio-sound status codes. `S_OK` is the only one that means the device did what was asked.
pub const S_OK: u32 = 0x8000;
pub const S_BAD_MSG: u32 = 0x8001;
pub const S_NOT_SUPP: u32 = 0x8002;
pub const S_IO_ERR: u32 = 0x8003;

// `virtio_snd_pcm_status` is two little-endian words: the status, then the latency in bytes.
pub const STATUS_BYTES: u32 = 8;

// Why a submitted period is not a played period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeriodFault {
	// The interrupt arrived and the used ring had nothing for us. Not an error by itself - the
	// caller waits again - but it is NOT a completion, which is the whole of the first defect.
	NoCompletion,
	// A completion arrived for a descriptor this driver did not submit.
	WrongDescriptor,
	// The device wrote fewer bytes than the status structure it owes.
	ShortStatus,
	// The device refused the period, with the code it gave.
	Device(u32),
}

// What one submitted period did, from the used element the ring produced and the status word the
// device wrote.
//
// `completion` is `take_used`'s answer - `None` when nothing completed, which is what a spurious or
// shared interrupt looks like from here. `expected` is the descriptor head this driver submitted.
// `written` is the used element's length, and `status` the first word of the status structure.
pub fn period_outcome(completion: Option<(u16, u32)>, expected: u16, status: u32) -> Result<u32, PeriodFault> {
	let Some((id, written)) = completion else {
		return Err(PeriodFault::NoCompletion);
	};
	if id != expected {
		return Err(PeriodFault::WrongDescriptor);
	}
	// THE LENGTH IS CHECKED BEFORE THE STATUS IS BELIEVED: a device that wrote four bytes did not
	// write a status structure, and the second word of what it did write is whatever was in the page.
	if written < STATUS_BYTES {
		return Err(PeriodFault::ShortStatus);
	}
	if status != S_OK {
		return Err(PeriodFault::Device(status));
	}
	Ok(written)
}

// The same, for a capture period: the device also owes the PCM it was asked to fill.
//
// A SHORT CAPTURE IS NOT A SHORT SUCCESS. The bytes beyond what the device wrote are the previous
// period's, and handing them to a recorder is handing it audio from a moment that has passed.
pub fn capture_outcome(completion: Option<(u16, u32)>, expected: u16, status: u32, period_bytes: u32) -> Result<u32, PeriodFault> {
	let written = period_outcome(completion, expected, status)?;
	if written < period_bytes.saturating_add(STATUS_BYTES) {
		return Err(PeriodFault::ShortStatus);
	}
	Ok(written)
}

// What the driver answers its service with. A REFUSAL IS AN EMPTY REPLY and a period is never empty,
// which is the convention this driver already uses for capture - so a playback failure reaches
// AudioService without a second message shape.
pub fn playback_reply(outcome: Result<u32, PeriodFault>) -> &'static [u8] {
	match outcome {
		Ok(_) => b"OK",
		Err(_) => &[],
	}
}

#[cfg(test)]
mod tests;
