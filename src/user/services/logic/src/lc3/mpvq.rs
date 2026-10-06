// MPVQ enumeration of the SNS second stage pulse vectors (sections 3.3.7.3.3.8 and 3.4.7.2.2.1).
//
// A PVQ(N, K) vector is an integer vector of dimension N whose absolute values sum to K. The SNS
// quantizer sends such a vector as two codewords: the sign of its first non-zero element (the
// "leading sign") and an MPVQ index that holds the positions, magnitudes and remaining signs. The
// index space is not a power of two, which is why the SNS multiplexing packs indices together
// arithmetically. Both directions below transcribe the specification's pseudocode (MPVQenum with
// encPushSign, and MPVQdeenum with mind2vec_tab, mind2vec_one, setval_update_sign and
// get_lead_sign); the offset table MPVQ_offsets(n, k) is tables::MPVQ_OFFSETS.
//
// THE DECODER INDEX IS UNTRUSTED. deenumerate() only ever indexes the offset table with values it
// has bounded, whatever the index; an index outside the shape's size gives some vector (possibly
// with fewer than K pulses), never a panic. The SNS demultiplexing rejects such indices before
// they get here.

use crate::lc3::tables::MPVQ_OFFSETS;

/// Number of MPVQ indices of PVQ(n, k) (the leading sign excluded): the SZ values of table 3.14,
/// which the codec itself uses as constants.
#[cfg(test)]
pub(crate) fn size(n: usize, k: usize) -> u32 {
	// MPVQ_offsets(n, k) + MPVQ_offsets(n, k + 1) counts all signed vectors of dimension n and
	// norm k divided by two; MPVQ_OFFSETS has k <= 10 only, so use the recursion of equation 57
	// for k + 1 = 11.
	let a = |n: usize, k: usize| -> u32 {
		if k <= 10 {
			MPVQ_OFFSETS[n][k]
		} else {
			// Equation 57: A(n, k) = A(n - 1, k - 1) + A(n, k - 1) + A(n - 1, k), A(0, k) = 1.
			let mut row = [0u32; 12];
			let mut prev = [0u32; 12];
			for (kk, p) in prev.iter_mut().enumerate() {
				*p = if kk == 0 { 0 } else { 1 };
			}
			for _ in 1..=n {
				row[0] = 0;
				for kk in 1..12 {
					row[kk] = prev[kk - 1] + row[kk - 1] + prev[kk];
				}
				prev = row;
			}
			prev[k]
		}
	};
	(a(n - 1, k) + a(n - 1, k + 1)) >> 1
}

/// MPVQenum: returns (index, lead_sign_ind) of vec[..dim]; lead_sign_ind is 1 for a negative
/// first non-zero element and 0 for a positive one.
pub(crate) fn enumerate(dim: usize, vec: &[i32]) -> (u32, u32) {
	let mut next_sign_ind: u32 = 0x8000_0000;
	let mut k_val_acc: usize = 0;
	let mut index: u32 = 0;
	let mut n = 0;
	let mut tmp_h_row = MPVQ_OFFSETS[n][0];
	for pos in (0..dim).rev() {
		let val = vec[pos];
		// encPushSign.
		if (next_sign_ind & 0x8000_0000) == 0 && val != 0 {
			index = 2 * index + next_sign_ind;
		}
		if val < 0 {
			next_sign_ind = 1;
		}
		if val > 0 {
			next_sign_ind = 0;
		}
		index += tmp_h_row;
		k_val_acc += val.unsigned_abs() as usize;
		if pos != 0 {
			n += 1;
		}
		tmp_h_row = MPVQ_OFFSETS[n][k_val_acc];
	}
	(index, next_sign_ind)
}

/// MPVQdeenum: the PVQ(dim, k) vector of a leading sign index and an MPVQ index, into out[..dim].
pub(crate) fn deenumerate(dim: usize, k: usize, ls_ind: u32, index: u32, out: &mut [i32]) {
	for v in out.iter_mut().take(dim) {
		*v = 0;
	}
	let mut leading_sign: i32 = if ls_ind != 0 { -1 } else { 1 };
	let mut ind = index;
	let mut row = dim - 1;
	let mut k_max_local = k;
	for o in out.iter_mut().take(dim) {
		if ind == 0 {
			// mind2vec_one: all remaining pulses on this position.
			*o = leading_sign * k_max_local as i32;
			break;
		}
		let h_row = &MPVQ_OFFSETS[row];
		let mut k_acc = k_max_local;
		// Find the largest k_acc with h_row[k_acc] <= ind (h_row[0] = 0 ends the search).
		while k_acc > 0 && ind < h_row[k_acc] {
			k_acc -= 1;
		}
		ind -= h_row[k_acc];
		let k_delta = k_max_local - k_acc;
		// setval_update_sign.
		if k_delta != 0 {
			*o = leading_sign * k_delta as i32;
			// get_lead_sign.
			leading_sign = if ind & 1 != 0 { -1 } else { 1 };
			ind >>= 1;
			k_max_local -= k_delta;
		}
		if row == 0 {
			break;
		}
		row -= 1;
	}
}

#[cfg(test)]
mod tests {
	extern crate std;
	use super::*;

	#[test]
	fn offsets_follow_equation_57() {
		for n in 0..16 {
			for k in 0..11 {
				let want = if k == 0 {
					0
				} else if n == 0 {
					1
				} else {
					MPVQ_OFFSETS[n - 1][k - 1] + MPVQ_OFFSETS[n][k - 1] + MPVQ_OFFSETS[n - 1][k]
				};
				assert_eq!(MPVQ_OFFSETS[n][k], want, "n {n} k {k}");
			}
		}
	}

	#[test]
	fn sizes_match_table_3_14() {
		assert_eq!(size(10, 10), 2_390_004);
		assert_eq!(size(6, 1), 6);
		assert_eq!(size(16, 8), 15_158_272);
		assert_eq!(size(16, 6), 774_912);
	}

	fn l1(v: &[i32]) -> usize {
		v.iter().map(|x| x.unsigned_abs() as usize).sum()
	}

	/// Every index of every shape configuration decodes to a vector of the right norm whose
	/// enumeration gives the index back, and the two leading signs give opposite vectors. The
	/// largest shape (16, 8) has 15 million indices and is sampled; the others are exhaustive.
	#[test]
	fn index_vector_index_round_trip() {
		for &(n, k, step) in &[(6usize, 1usize, 1u32), (10, 10, 1), (16, 6, 1), (16, 8, 97)] {
			let sz = size(n, k);
			let mut idx = 0;
			while idx < sz {
				for ls in 0..2 {
					let mut v = [0i32; 16];
					deenumerate(n, k, ls, idx, &mut v);
					assert_eq!(l1(&v[..n]), k, "({n},{k}) index {idx}");
					let first = v[..n].iter().find(|&&x| x != 0).copied().unwrap();
					assert_eq!(first < 0, ls == 1);
					assert_eq!(enumerate(n, &v[..n]), (idx, ls), "({n},{k}) index {idx} vector {v:?}");
				}
				idx += step;
			}
		}
	}

	/// The other direction: random vectors of each shape enumerate into the index range and come
	/// back unchanged.
	#[test]
	fn vector_index_vector_round_trip() {
		let mut seed = 99u32;
		let mut rand = move |m: u32| {
			seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
			(seed >> 8) % m
		};
		for &(n, k) in &[(6usize, 1usize), (10, 10), (16, 6), (16, 8)] {
			for _ in 0..20000 {
				let mut v = [0i32; 16];
				for _ in 0..k {
					let p = rand(n as u32) as usize;
					v[p] += if v[p] != 0 {
						v[p].signum()
					} else if rand(2) == 0 {
						1
					} else {
						-1
					};
				}
				let (idx, ls) = enumerate(n, &v[..n]);
				assert!(idx < size(n, k));
				let mut back = [0i32; 16];
				deenumerate(n, k, ls, idx, &mut back);
				assert_eq!(back, v);
			}
		}
	}

	#[test]
	fn garbage_indices_do_not_panic() {
		for &(n, k) in &[(6usize, 1usize), (10, 10), (16, 6), (16, 8)] {
			for idx in [size(n, k), size(n, k) + 1, u32::MAX, u32::MAX / 3] {
				let mut v = [0i32; 16];
				deenumerate(n, k, 1, idx, &mut v);
				assert!(l1(&v[..n]) <= k);
			}
		}
	}
}
