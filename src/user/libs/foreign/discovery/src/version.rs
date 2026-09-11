//! Which Loader/Driver interface versions this substrate admits, and why each refusal is a refusal.
//!
//! "TWO EXPORTS" IS NOT A VERSION-INDEPENDENT CONTRACT, which is the whole reason this module is a
//! decision rather than a comment. The upstream interface does not have one export shape across its
//! versions, so a substrate that said "two exports" and negotiated whatever the driver offered would
//! be admitting versions whose required lookup surface two exports do not provide.

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
