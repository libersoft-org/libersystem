// The analysis half of Layer III's transform: the 32-band polyphase filterbank, the 18-point MDCT
// over each band, and the alias butterflies - in that order, which is the decoder's order reversed.
//
// Every step here is the ADJOINT of a step the decoder performs, and that is what makes it correct
// rather than approximately correct: the decoder's alias reduction is a rotation, so this applies
// its transpose; the decoder's IMDCT is windowed and overlap-added, so this windows and folds the
// same 36 samples; the decoder's synthesis is the polyphase bank with the window in `tables`, so
// this is the analysis bank with the analysis window beside it.

use super::tables::{ALIAS, ANALYSIS_COS, ANALYSIS_WINDOW, MDCT_COS};
use alloc::vec::Vec;

// One granule of one channel.
pub(crate) const LINES: usize = 576;
// The filterbank takes 32 input samples per block and 18 blocks make a granule.
pub(crate) const BLOCK: usize = 32;
pub(crate) const BLOCKS_PER_GRANULE: usize = 18;

// The analysis filterbank: 32 input samples in, 32 subband samples out, over a 512-sample history.
//
// The history is the state this carries between calls, which is what makes a stream encodable in
// bounded memory: an hour of audio costs 512 samples here, not an hour of them.
pub(crate) struct Filterbank {
	history: [f32; 512],
}

impl Filterbank {
	pub(crate) fn new() -> Filterbank {
		Filterbank { history: [0.0; 512] }
	}

	pub(crate) fn push(&mut self, samples: &[f32; BLOCK], out: &mut [f32; 32]) {
		// The newest sample goes to index 0, which is the order the format's own pseudo-code
		// shifts them in.
		self.history.copy_within(0..512 - BLOCK, BLOCK);
		for (i, sample) in samples.iter().enumerate() {
			self.history[BLOCK - 1 - i] = *sample;
		}
		// Window, fold to 64, then the cosine transform.
		let mut folded = [0.0f32; 64];
		for (i, slot) in folded.iter_mut().enumerate() {
			let mut sum = 0.0;
			for j in 0..8 {
				sum += ANALYSIS_WINDOW[i + 64 * j] * self.history[i + 64 * j];
			}
			*slot = sum;
		}
		for (value, row) in out.iter_mut().zip(ANALYSIS_COS.iter()) {
			let mut sum = 0.0;
			for (folded, coefficient) in folded.iter().zip(row.iter()) {
				sum += folded * coefficient;
			}
			*value = sum;
		}
	}
}

// One granule's 576 lines from 36 blocks of subband samples - the granule's own 18 and the 18 that
// follow it, because an MDCT with fifty per cent overlap reaches into the next granule.
//
// `1/9` is not a tuning constant: it is `2/n` for the 18-line transform, the factor that makes this
// the exact adjoint of the decoder's IMDCT under a window whose halves are power-complementary -
// which the sine window is. Measured as well as derived: the round trip reconstructs at a scale of
// 0.99985.
pub(crate) fn granule_lines(blocks: &[[f32; 32]], first: usize) -> Vec<f32> {
	let mut lines = alloc::vec![0.0f32; LINES];
	let mut window36 = [0.0f32; 36];
	for s in 0..32 {
		for (j, slot) in window36.iter_mut().enumerate() {
			// THE DECODER'S FREQUENCY INVERSION, UNDONE. It negates every other time sample of
			// every odd subband on the way out; an encoder that did not undo it would hand the
			// decoder a signal it is about to invert again.
			let value = blocks[first + j][s];
			*slot = if s % 2 == 1 && (first + j) % 2 == 1 { -value } else { value };
		}
		for (k, row) in MDCT_COS.iter().enumerate() {
			let mut sum = 0.0;
			for (sample, coefficient) in window36.iter().zip(row.iter()) {
				sum += sample * coefficient;
			}
			lines[s * 18 + k] = sum / 9.0;
		}
	}
	// The alias butterflies, transposed: the decoder mixes each boundary pair on the way in, so
	// this unmixes them on the way out.
	for i in 1..32 {
		for (k, (cs, ca)) in ALIAS.iter().enumerate() {
			let a = lines[18 * i - 1 - k];
			let b = lines[18 * i + k];
			lines[18 * i - 1 - k] = a * cs + b * ca;
			lines[18 * i + k] = b * cs - a * ca;
		}
	}
	lines
}
