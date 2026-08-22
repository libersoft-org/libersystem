// Vorbis decoder written in Rust
//
// Copyright (c) 2019 est31 <MTest31@outlook.com>
// and contributors. All rights reserved.
// Licensed under MIT license, or Apache 2 license,
// at your option. Please see the LICENSE file
// attached to this source distribution for details.

/*!
Traits for sample formats
*/

use alloc::vec::Vec;

/// Trait for a packet of multiple samples.
///
/// IT USED TO ASK TWO MORE QUESTIONS - `num_samples` and `truncate` - and nothing ever asked them:
/// the decoder hands its `samples` vector straight to the caller, which counts and trims it itself.
pub trait Samples {
	fn from_floats(floats: Vec<Vec<f32>>) -> Self;
}

impl<S: Sample> Samples for Vec<Vec<S>> {
	fn from_floats(floats: Vec<Vec<f32>>) -> Self {
		floats.into_iter().map(|samples| samples.into_iter().map(S::from_float).collect()).collect()
	}
}

/// A packet of multi-channel interleaved samples
pub struct InterleavedSamples<S: Sample> {
	pub samples: Vec<S>,
}

impl<S: Sample> Samples for InterleavedSamples<S> {
	fn from_floats(floats: Vec<Vec<f32>>) -> Self {
		let channel_count = floats.len();
		// Note that a channel count of 0 is forbidden
		// by the spec and the header decoding code already
		// checks for that.
		assert!(floats.len() > 0);
		let samples_interleaved = if channel_count == 1 {
			// Because decoded_pck[0] doesn't work...
			<Vec<Vec<S>> as Samples>::from_floats(floats).into_iter().next().unwrap()
		} else {
			let len = floats[0].len();
			let mut samples = Vec::with_capacity(len * channel_count);
			for i in 0..len {
				for ref chan in floats.iter() {
					samples.push(S::from_float(chan[i]));
				}
			}
			samples
		};
		Self { samples: samples_interleaved }
	}
}

/// Trait representing a single sample
pub trait Sample {
	fn from_float(fl: f32) -> Self;
}

impl Sample for f32 {
	fn from_float(fl: f32) -> Self {
		fl
	}
}

impl Sample for i16 {
	fn from_float(fl: f32) -> Self {
		let fl = fl * 32768.0;
		if fl > 32767. {
			32767
		} else if fl < -32768. {
			-32768
		} else {
			fl as i16
		}
	}
}
