//! THE TIMER TABLE, at the kernel's 10 ms tick.
//!
//! A TIMER NEVER EXPIRES BEFORE THE SPECIFICATION'S MINIMUM: it is armed for ceil(minimum / 10 ms) + 1 ticks, the one
//! more covering the tick already under way. Where the engine instead waits for the PARTNER - VBUS falling within
//! tSafe0V, coming back within tSrcRecover and tSrcTurnOn - it waits for the partner's maximum the same way. The only
//! timer whose maximum the rounding exceeds is SenderResponseTimer (24 to 30 ms, which fires at 30 to 40 ms), and its
//! late expiry delays nothing but the sink's own recovery.

/// The kernel's deadline resolution.
pub const TICK_MS: u64 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Timer {
	/// Rp stable on CC before the sink attaches.
	CcDebounce,
	/// CC open before the sink detaches.
	PdDebounce,
	/// Source_Capabilities awaited after attach, and after a reset.
	SinkWaitCap,
	/// An answer awaited to a message sent.
	SenderResponse,
	/// PS_RDY awaited after Accept.
	PsTransition,
	/// A request repeated after Wait.
	SinkRequest,
	/// A hard reset's VBUS awaited to fall to vSafe0V.
	Safe0V,
	/// And to come back.
	SrcRecover,
	/// CC held open for error recovery.
	ErrorRecovery,
}

impl Timer {
	/// The specification's (minimum, maximum), in milliseconds; a bound it gives no maximum for repeats its minimum.
	pub const fn bounds(self) -> (u64, u64) {
		match self {
			Timer::CcDebounce => (100, 200),
			Timer::PdDebounce => (10, 20),
			Timer::SinkWaitCap => (310, 620),
			Timer::SenderResponse => (24, 30),
			Timer::PsTransition => (450, 550),
			Timer::SinkRequest => (100, 100),
			// tSafe0V is a maximum: the source has that long to bring VBUS down.
			Timer::Safe0V => (650, 650),
			// tSrcRecover's maximum plus tSrcTurnOn's: the source has that long to bring VBUS back.
			Timer::SrcRecover => (1000 + 275, 1000 + 275),
			Timer::ErrorRecovery => (25, 25),
		}
	}

	/// Ticks to arm it for: never before its minimum.
	pub const fn ticks(self) -> u64 {
		self.bounds().0.div_ceil(TICK_MS) + 1
	}

	/// Whether the rounding lets it fire past a maximum the specification gives it.
	pub const fn late(self) -> bool {
		let (minimum, maximum) = self.bounds();
		minimum != maximum && self.ticks() * TICK_MS > maximum
	}
}

/// Hard resets sent without a contract resulting, before the sink stays at the Type-C current.
pub const HARD_RESET_COUNT: u8 = 2;
