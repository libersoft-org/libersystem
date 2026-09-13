// THE DECISIONS OF A CONSOLE BYTE STREAM, ON THEIR OWN.
//
// `liber:device@1`'s `console-stream` is served by a driver and consumed by a program in another
// address space, and the two ends have to agree about four things that no amount of care in either
// one can settle alone: which version is being spoken, which published provider is the one being
// used, how a frame longer than the wire's bound is cut up, and what a refused write means. Each of
// those was a convention before - an empty message, a magic tag, a driver's name - and a convention
// is what both ends remember separately until one of them stops.
//
// They live HERE, in the crate both ends already share, because that is what makes them one rule
// rather than two implementations of one rule; and they are pure, because a rule a host test can
// watch fail is a rule, and a rule that can only be exercised by booting a guest is a hope.
//
// This module decides. It never sends: the driver and the agent own their channels, and a layer
// that both decided and acted would be a second place for the two to disagree.

// THE VERSION THIS TREE SPEAKS. One number, in one place, for both ends: a constant each end
// defines for itself is two constants, and the day they differ is the day a byte stream starts
// arriving somewhere else in the reader with nothing refused anywhere.
pub const WIRE_VERSION: u32 = 1;

// THE LARGEST WRITE THE WIRE CAN EXPRESS, and it is not a choice: a `list<u8>` is length-prefixed
// with a `u16` on this transport, so 65535 is the ceiling the schema itself enforces. A provider may
// answer with less; it may not answer with more.
pub const MAX_WRITE: u32 = 65535;

// What a provider answers an `attach` with, or why it will not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attach {
	// Agreed: this is the version being spoken and the largest single write that will be taken.
	Speak { version: u32, max_frame: u32 },
	// The version asked for is not the version served. REFUSED RATHER THAN DOWNGRADED: a byte
	// stream cannot report a framing disagreement later, so the only safe moment to refuse is
	// before the first byte.
	Unsupported,
	// The provider serves a version it cannot carry a frame of, which is a provider that would
	// accept an attachment and refuse every write made under it.
	Unusable,
}

// Decide one attachment. `served` and `max_frame` are the provider's own; `asked` is the consumer's.
pub fn attach(asked: u32, served: u32, max_frame: u32) -> Attach {
	if asked != served {
		return Attach::Unsupported;
	}
	if max_frame == 0 || max_frame > MAX_WRITE {
		return Attach::Unusable;
	}
	Attach::Speak { version: served, max_frame }
}

// WHICH PUBLICATION A CONSUMER IS HOLDING, by the three numbers the manager minted rather than by
// anything a driver chose. A withdrawal is described after its handle is gone, so identity is all
// that is left to recognise it by - and a slot that has been reused must not be mistaken for the
// provider that left it, which is what the generations are for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Identity {
	pub slot: u32,
	pub provider_generation: u32,
	pub binding_generation: u64,
}

// What one publication frame means to a consumer holding `held`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Follow {
	// Nothing is held and this one is live: open it.
	Attach,
	// The one being held has gone: let it go, and with it the session it carried.
	Detach,
	// Somebody else's publication, or a repeat of what is already held.
	Ignore,
}

pub fn follow(held: Option<Identity>, seen: Identity, live: bool) -> Follow {
	match held {
		None if live => Follow::Attach,
		None => Follow::Ignore,
		Some(current) if current == seen && !live => Follow::Detach,
		Some(_) => Follow::Ignore,
	}
}

// Whether a provider may take this write at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admit {
	Write,
	// A consumer that has not settled a version does not know how what comes back is framed, so
	// letting its bytes reach the device would put a stream on the wire that neither end can parse.
	NotAttached,
	// Empty, or longer than what was agreed. Both are refusals rather than adjustments: a write
	// silently cut to fit is a frame the reader will never reassemble.
	BadLength,
}

pub fn admit_write(attached: bool, len: usize, max_frame: u32) -> Admit {
	if !attached {
		return Admit::NotAttached;
	}
	if len == 0 || len > max_frame as usize {
		return Admit::BadLength;
	}
	Admit::Write
}

// Whether a provider may grant a receive stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grant {
	Mint,
	NotAttached,
	// ONE SUBSCRIPTION PER CONNECTION. A second endpoint would be a second reader of one port, and
	// the bytes would be divided between them by whichever happened to be drained first.
	AlreadyGranted,
}

pub fn grant_stream(attached: bool, has_stream: bool) -> Grant {
	if !attached {
		return Grant::NotAttached;
	}
	if has_stream {
		return Grant::AlreadyGranted;
	}
	Grant::Mint
}

// THE NEXT PIECE OF A FRAME, or none when all of it has gone.
//
// A byte stream has no frame boundaries, which is its one freedom: a protocol frame longer than
// what a provider takes in one call is written in several, and the reader reassembles it the same
// way it reassembles everything else. The span is returned rather than the bytes so the caller
// keeps its own buffer and this stays a decision.
pub fn write_span(sent: usize, len: usize, max_frame: u32) -> Option<(usize, usize)> {
	if max_frame == 0 || sent >= len {
		return None;
	}
	let end: usize = sent.saturating_add(max_frame as usize).min(len);
	Some((sent, end))
}

// What a provider answered a `write` with, in the words the contract uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteAnswer {
	// It took this many bytes.
	Took(u32),
	// `again`: the device still holds the buffer the last write went into, which is what a host
	// that stopped reading looks like from inside the guest. Nothing was written.
	Again,
	// Any other refusal the contract can carry.
	Refused,
	// No answer at all: the connection is gone.
	NoAnswer,
}

// What that answer means for the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrote {
	// All of the span went; continue with the next one.
	All,
	// THE SESSION ENDS AND THE ATTACHMENT STAYS. The port is fine; the tool at the far end of it
	// has stopped reading. Tearing down the provider for that would lose a working device because
	// somebody closed a terminal.
	SessionOver,
	// The provider itself is not usable: let it go and let the subscription find the next one.
	ProviderLost,
}

pub fn classify_write(answer: WriteAnswer, asked: usize) -> Wrote {
	match answer {
		WriteAnswer::Took(taken) if taken as usize == asked => Wrote::All,
		// A PARTIAL WRITE IS NOT A WRITE. The contract answers with what the port took, and a
		// provider that took part of a span has left the stream in a state no reader can recover:
		// the remainder would be read as the beginning of the next frame.
		WriteAnswer::Took(_) => Wrote::ProviderLost,
		WriteAnswer::Again => Wrote::SessionOver,
		WriteAnswer::Refused | WriteAnswer::NoAnswer => Wrote::ProviderLost,
	}
}

#[cfg(test)]
mod tests;
