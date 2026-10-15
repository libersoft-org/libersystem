//! Which Loader/Driver interface versions this substrate admits, and why each refusal is a refusal.
//!
//! "TWO EXPORTS" IS NOT A VERSION-INDEPENDENT CONTRACT, which is the whole reason this module is a
//! decision rather than a comment. The upstream interface does not have one export shape across its
//! versions, so a substrate that said "two exports" and negotiated whatever the driver offered would
//! be admitting versions whose required lookup surface two exports do not provide.
//!
//! AND THE DECISION IS ENFORCED WHERE THE NEGOTIATION HAPPENS, not only stated here. The pinned
//! loader asks every driver for its own current version - 7 - and accepts any answer down to 0. So
//! the substrate does not hand the loader a driver's negotiation entry point at all: it hands it
//! `library::resolve`'s bounded one, which runs `exchange` below - offering the driver at most 6 and
//! refusing any answer outside 2..=6 - and only then tells the loader what was agreed.

use crate::record::NegotiateFn;

/// `VK_SUCCESS` and `VK_ERROR_INCOMPATIBLE_DRIVER`, the two results a negotiation distinguishes.
///
/// SPELLED HERE RATHER THAN INCLUDED, because the pinned headers are audit-only; both are numbers of
/// the interface, not of any header's implementation. The second is the ONLY failure the pinned
/// loader recognises from a negotiation - every other result it treats as agreement - which is why
/// every refusal below reaches it as exactly this value.
pub const SUCCESS: i32 = 0;
pub const INCOMPATIBLE_DRIVER: i32 = -9;

/// The versions this substrate will negotiate.
///
/// TWO EXPORTS ARE NECESSARY AND SUFFICIENT ACROSS ALL FIVE: negotiation exists from 2, and it is
/// only at 7 that the interface functions MAY be obtained through `vk_icdGetInstanceProcAddr`
/// instead of being exported.
pub const ADMITTED: [u32; 5] = [2, 3, 4, 5, 6];

pub const LOWEST_ADMITTED: u32 = 2;
pub const HIGHEST_ADMITTED: u32 = 6;

/// Why a version was refused. Each is a different fact about the interface, and collapsing them into
/// one "unsupported" would leave whoever hits it guessing which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Version 0's multi-export bootstrap is a different SHAPE. Admitting it would put a third and a
	/// fourth symbol into a surface this milestone defines as closed.
	MultiExportBootstrap,
	/// Version 1 has `vk_icdGetInstanceProcAddr` and NO negotiation function, so an ICD at this
	/// version exports ONE symbol and the two-export rule cannot be satisfied by it.
	NoNegotiationFunction,
	/// At 7 and above the negotiation and physical-device functions MAY be queried rather than
	/// exported, so a conforming driver need not export what this substrate resolves. The substrate
	/// never negotiates above 6, which is what keeps two exports sufficient.
	QueryableRatherThanExported,
	/// A driver requiring `vk_icdGetPhysicalDeviceProcAddr` - the symbol version 4 adds for drivers
	/// exposing physical-device extensions - is outside this profile. Admitting it opens a THIRD
	/// symbol, which is the closed surface this decision exists to define.
	ThirdExportRequired,
	/// The driver answered the offer with an error: it speaks nothing at or below what it was
	/// offered. A driver that speaks only 7 and above says exactly this when it is offered 6.
	DriverDeclined,
	/// The driver wrote back a version it was not offered. A conforming driver answers at or below
	/// the offer; one that answers above it claims an agreement nobody made.
	NotOffered,
	/// The loader asked for less than the lowest admitted version, so there was nothing to offer and
	/// the driver was never called. The pinned loader asks for 7 and never gets here; the case exists
	/// because the bound is the substrate's and has to hold whoever calls it.
	NothingToOffer,
}

impl Refusal {
	/// The refusal in words, for the one diagnostic line a refused negotiation produces.
	pub fn reason(self) -> &'static [u8] {
		match self {
			Refusal::MultiExportBootstrap => b"version 0 is a different bootstrap shape",
			Refusal::NoNegotiationFunction => b"version 1 has no negotiation function",
			Refusal::QueryableRatherThanExported => b"7 and above may query what this substrate resolves by export",
			Refusal::ThirdExportRequired => b"the physical-device export is outside this profile",
			Refusal::DriverDeclined => b"the driver speaks nothing up to the offer",
			Refusal::NotOffered => b"the driver answered a version it was not offered",
			Refusal::NothingToOffer => b"the loader asked below the lowest admitted version",
		}
	}
}

/// What negotiating with a driver settled on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Negotiated {
	/// Both sides agreed on this version, and it is admitted.
	At(u32),
	Refused(Refusal),
}

/// Negotiate with a driver that offers `offered`, and that may or may not require the third export.
///
/// THE RULE IS "THE HIGHEST VERSION BOTH SIDES SUPPORT", which is upstream's, and the ceiling is
/// this substrate's: never above 6. A driver offering more gets 6 and is content; a driver offering
/// less than 2 has nothing to agree on.
pub fn negotiate(offered: u32, requires_third_export: bool) -> Negotiated {
	if requires_third_export {
		// CHECKED FIRST, because a driver that needs the third export is outside this profile
		// whatever version it offers - and reporting the version refusal instead would send the
		// reader looking at the wrong thing.
		return Negotiated::Refused(Refusal::ThirdExportRequired);
	}
	match offered {
		0 => Negotiated::Refused(Refusal::MultiExportBootstrap),
		1 => Negotiated::Refused(Refusal::NoNegotiationFunction),
		version if version >= LOWEST_ADMITTED => Negotiated::At(version.min(HIGHEST_ADMITTED)),
		// UNREACHABLE BY ARITHMETIC and written anyway: 0 and 1 are the only values below the
		// lowest admitted one, and both have their own answer above.
		_ => Negotiated::Refused(Refusal::NoNegotiationFunction),
	}
}

/// Is `version` one this substrate will run against?
pub fn is_admitted(version: u32) -> bool {
	ADMITTED.contains(&version)
}

/// What the substrate offers a driver when the loader asks for `asked`.
///
/// NEVER ABOVE 6. The pinned loader asks for 7, and at 7 a driver may stop exporting what this
/// substrate resolves - so the driver is never told 7 is on the table. And NOTHING BELOW 2: there is
/// no negotiation there to have, so `None` means the driver is not called at all.
pub fn offer(asked: u32) -> Option<u32> {
	let offered = asked.min(HIGHEST_ADMITTED);
	(offered >= LOWEST_ADMITTED).then_some(offered)
}

/// Judge a driver's answer to `offered`: the result it returned and the version it wrote back.
///
/// ONLY `SUCCESS` IS AN AGREEMENT. The pinned loader treats every result but
/// `VK_ERROR_INCOMPATIBLE_DRIVER` as one, so a driver returning some other error would be run at
/// whatever it happened to write; here any error is a refusal. And the answer has to be one this
/// substrate admits AND one it was offered - each way of missing that is its own refusal, checked
/// in the order a reader would look for it.
pub fn judge(offered: u32, result: i32, answered: u32) -> Negotiated {
	if result != SUCCESS {
		return Negotiated::Refused(Refusal::DriverDeclined);
	}
	match answered {
		0 => Negotiated::Refused(Refusal::MultiExportBootstrap),
		1 => Negotiated::Refused(Refusal::NoNegotiationFunction),
		version if version > HIGHEST_ADMITTED => Negotiated::Refused(Refusal::QueryableRatherThanExported),
		version if version > offered => Negotiated::Refused(Refusal::NotOffered),
		version => Negotiated::At(version),
	}
}

/// One negotiation, as the substrate runs it on the loader's behalf.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Exchange {
	/// What the loader asked for.
	pub asked: u32,
	/// What the driver was offered, or `None` when nothing was and it was not called.
	pub offered: Option<u32>,
	/// What the driver wrote back. Meaningless when it was not called or declined.
	pub answered: u32,
	pub outcome: Negotiated,
}

/// Negotiate with `driver` on behalf of a loader that asked for `asked`.
///
/// THE DRIVER SEES THE SUBSTRATE'S OFFER AND NEVER THE LOADER'S, and the loader sees the outcome
/// and never the driver's raw answer - which is what makes 2..=6 a property of every negotiation
/// that runs here rather than of whichever driver happens to clamp itself.
///
/// # Safety
/// `driver` must be the negotiation entry point of a provider in the verified closure.
pub unsafe fn exchange(driver: NegotiateFn, asked: u32) -> Exchange {
	let Some(offered) = offer(asked) else {
		return Exchange { asked, offered: None, answered: asked, outcome: Negotiated::Refused(Refusal::NothingToOffer) };
	};
	let mut answered = offered;
	// SAFETY: the caller guarantees `driver`; `answered` is a live local the driver writes into.
	let result = unsafe { driver(&raw mut answered) };
	Exchange { asked, offered: Some(offered), answered, outcome: judge(offered, result, answered) }
}
