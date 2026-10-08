//! POWER DATA OBJECTS AND THE SELECTION RULE.
//!
//! A source offers Fixed, Variable, Battery and augmented (PPS, AVS) supplies; THIS SINK REQUESTS ONLY FIXED ONES, and
//! only at a voltage inside ONE of the sink PDOs the board describes (the connector's `sink-pdos`) - equal to a Fixed
//! sink PDO's voltage, or within a Variable or Battery sink PDO's range - with its currents at or below both the offer's
//! maximum and that sink PDO's current (a Battery sink PDO's power divided by the voltage). A board describing Fixed 5 V
//! and 15 V never requests a 9 V or a 12 V offer.
//!
//! THE PROPOSED RULE, the owner's to confirm: among the offers that qualify, the one giving the most power with its
//! current limited to its sink PDO's, requesting that limited current, ties to the lower voltage; when no offer reaches
//! the board's operational power (`op-sink-microwatt`), position 1 at 5 V, its current limited the same way, with
//! capability mismatch set.

use alloc::vec::Vec;

/// A source's offer, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Offer {
	Fixed {
		millivolts: u32,
		milliamps: u32,
	},
	Variable {
		min_millivolts: u32,
		max_millivolts: u32,
		milliamps: u32,
	},
	Battery {
		min_millivolts: u32,
		max_millivolts: u32,
		milliwatts: u32,
	},
	/// A programmable or adjustable supply: reported, never selected.
	Augmented(u32),
}

impl Offer {
	pub fn decode(object: u32) -> Offer {
		match object >> 30 {
			0 => Offer::Fixed { millivolts: ((object >> 10) & 0x3FF) * 50, milliamps: (object & 0x3FF) * 10 },
			1 => Offer::Battery { min_millivolts: ((object >> 10) & 0x3FF) * 50, max_millivolts: ((object >> 20) & 0x3FF) * 50, milliwatts: (object & 0x3FF) * 250 },
			2 => Offer::Variable { min_millivolts: ((object >> 10) & 0x3FF) * 50, max_millivolts: ((object >> 20) & 0x3FF) * 50, milliamps: (object & 0x3FF) * 10 },
			_ => Offer::Augmented(object),
		}
	}

	/// A fixed offer's voltage, in millivolts.
	pub fn fixed_millivolts(&self) -> Option<u32> {
		match self {
			Offer::Fixed { millivolts, .. } => Some(*millivolts),
			_ => None,
		}
	}
}

/// One sink PDO the board describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SinkPdo {
	Fixed { millivolts: u32, milliamps: u32 },
	Variable { min_millivolts: u32, max_millivolts: u32, milliamps: u32 },
	Battery { min_millivolts: u32, max_millivolts: u32, milliwatts: u32 },
}

impl SinkPdo {
	/// The current this sink PDO allows at `millivolts`, if the voltage lies inside it.
	pub fn allows(&self, millivolts: u32) -> Option<u32> {
		match *self {
			SinkPdo::Fixed { millivolts: at, milliamps } => (at == millivolts).then_some(milliamps),
			SinkPdo::Variable { min_millivolts, max_millivolts, milliamps } => (min_millivolts..=max_millivolts).contains(&millivolts).then_some(milliamps),
			SinkPdo::Battery { min_millivolts, max_millivolts, milliwatts } => (millivolts > 0 && (min_millivolts..=max_millivolts).contains(&millivolts)).then(|| (u64::from(milliwatts) * 1000 / u64::from(millivolts)) as u32),
		}
	}

	/// As a Sink_Capabilities object.
	pub fn encode(&self) -> u32 {
		match *self {
			SinkPdo::Fixed { millivolts, milliamps } => (millivolts / 50) << 10 | (milliamps / 10).min(0x3FF),
			SinkPdo::Variable { min_millivolts, max_millivolts, milliamps } => 2 << 30 | (max_millivolts / 50) << 20 | (min_millivolts / 50) << 10 | (milliamps / 10).min(0x3FF),
			SinkPdo::Battery { min_millivolts, max_millivolts, milliwatts } => 1 << 30 | (max_millivolts / 50) << 20 | (min_millivolts / 50) << 10 | (milliwatts / 250).min(0x3FF),
		}
	}

	/// The highest voltage it takes.
	pub fn max_millivolts(&self) -> u32 {
		match *self {
			SinkPdo::Fixed { millivolts, .. } => millivolts,
			SinkPdo::Variable { max_millivolts, .. } | SinkPdo::Battery { max_millivolts, .. } => max_millivolts,
		}
	}
}

/// What the board describes of its sink.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sink {
	pub pdos: Vec<SinkPdo>,
	/// The power the board needs to operate, in microwatts.
	pub operational_microwatts: u64,
}

/// The request chosen: an offer's position (from 1), its voltage and the current asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
	pub position: u8,
	pub millivolts: u32,
	pub milliamps: u32,
	pub mismatch: bool,
}

impl Selection {
	/// The Request data object for a fixed offer: its position, capability mismatch, USB communications capable (no),
	/// no USB suspend, and the operating and maximum current - the same, the limited current - in 10 mA units.
	pub fn request(&self) -> u32 {
		let current = (self.milliamps / 10).min(0x3FF);
		u32::from(self.position) << 28 | u32::from(self.mismatch) << 26 | 1 << 24 | current << 10 | current
	}

	pub fn microwatts(&self) -> u64 {
		u64::from(self.millivolts) * u64::from(self.milliamps)
	}
}

/// The limited current a fixed offer gives this sink, and the sink PDO that admits it.
fn admitted(sink: &Sink, millivolts: u32, milliamps: u32) -> Option<u32> {
	sink.pdos.iter().filter_map(|pdo| pdo.allows(millivolts)).map(|allowed| allowed.min(milliamps)).max()
}

/// THE SELECTION RULE over the offers of the LATEST Source_Capabilities. `None` when not even position 1 is a fixed
/// 5 V offer this sink admits - then there is nothing to request.
pub fn select(sink: &Sink, offers: &[u32]) -> Option<Selection> {
	let mut best: Option<Selection> = None;
	for (at, object) in offers.iter().enumerate() {
		let Offer::Fixed { millivolts, milliamps } = Offer::decode(*object) else { continue };
		let Some(limited) = admitted(sink, millivolts, milliamps) else { continue };
		let candidate = Selection { position: at as u8 + 1, millivolts, milliamps: limited, mismatch: false };
		best = match best {
			Some(held) if held.microwatts() > candidate.microwatts() || (held.microwatts() == candidate.microwatts() && held.millivolts <= candidate.millivolts) => Some(held),
			_ => Some(candidate),
		};
	}
	match best {
		// Millivolts by milliamps is microwatts.
		Some(chosen) if chosen.microwatts() >= sink.operational_microwatts => Some(chosen),
		_ => {
			// NOTHING REACHES THE OPERATIONAL POWER: position 1, which a source's first offer always is, at 5 V.
			let Offer::Fixed { millivolts: 5_000, milliamps } = Offer::decode(*offers.first()?) else { return None };
			let millivolts = 5_000;
			let limited = admitted(sink, millivolts, milliamps)?;
			Some(Selection { position: 1, millivolts, milliamps: limited, mismatch: true })
		}
	}
}
