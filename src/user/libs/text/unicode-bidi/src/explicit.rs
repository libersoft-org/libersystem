//! X1 to X10: the explicit embeddings, overrides and isolates, and the run sequences they make.
//!
//! THE DIRECTIONAL STATUS STACK IS THE WHOLE OF X1 TO X8. Each embedding, override or isolate
//! initiator pushes a level and an override status; each terminator pops. What makes this fiddly is
//! that the three families fail differently when they overflow - an embedding that cannot be pushed
//! is counted as overflowed and its pop must not pop somebody else's entry - and getting that wrong
//! shows up only in deeply nested text nobody writes by hand.

use alloc::vec;
use alloc::vec::Vec;
use unicode_tables::BidiClass;

use crate::{MAX_DEPTH, Working};

/// One entry of the directional status stack (BD11).
#[derive(Clone, Copy)]
struct Status {
	level: u8,
	/// The override in force: `None`, or the class every character is treated as.
	override_class: Option<BidiClass>,
	isolate: bool,
}

/// BD9: the PDI that matches each isolate initiator, and the initiator each PDI matches.
fn match_isolates(classes: &[BidiClass]) -> (Vec<usize>, Vec<usize>) {
	let mut matching_pdi = vec![usize::MAX; classes.len()];
	let mut matching_initiator = vec![usize::MAX; classes.len()];
	let mut stack: Vec<usize> = Vec::new();
	for (index, class) in classes.iter().enumerate() {
		match class {
			BidiClass::LRI | BidiClass::RLI | BidiClass::FSI => stack.push(index),
			BidiClass::PDI => {
				if let Some(initiator) = stack.pop() {
					matching_pdi[initiator] = index;
					matching_initiator[index] = initiator;
				}
			}
			_ => {}
		}
	}
	// An initiator with no PDI matches the END of the paragraph, which BD9 states and which the run
	// sequences below depend on.
	for entry in matching_pdi.iter_mut() {
		if *entry == usize::MAX {
			*entry = classes.len();
		}
	}
	(matching_pdi, matching_initiator)
}

/// X1 to X8, and X9's removals.
pub(crate) fn resolve_explicit(classes: &[BidiClass], paragraph_level: u8) -> Working {
	let (matching_pdi, matching_initiator) = match_isolates(classes);
	let mut levels = vec![paragraph_level; classes.len()];
	let mut resolved: Vec<BidiClass> = classes.to_vec();
	let mut removed = vec![false; classes.len()];

	// X1: the stack starts with the paragraph level and no override.
	let mut stack: Vec<Status> = Vec::new();
	stack.push(Status { level: paragraph_level, override_class: None, isolate: false });
	let mut overflow_isolates = 0usize;
	let mut overflow_embeddings = 0usize;
	let mut valid_isolates = 0usize;

	for index in 0..classes.len() {
		let class = classes[index];
		match class {
			// X2 to X5: the embeddings and overrides.
			BidiClass::RLE | BidiClass::LRE | BidiClass::RLO | BidiClass::LRO => {
				// The control itself takes the level of what precedes it (X9 removes it from
				// everything else), which is what rule L1 needs it to have.
				levels[index] = stack.last().map(|status| status.level).unwrap_or(paragraph_level);
				removed[index] = true;
				let rtl = matches!(class, BidiClass::RLE | BidiClass::RLO);
				let next = next_level(stack.last().map(|status| status.level).unwrap_or(paragraph_level), rtl);
				if next <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
					stack.push(Status {
						level: next,
						override_class: match class {
							BidiClass::LRO => Some(BidiClass::L),
							BidiClass::RLO => Some(BidiClass::R),
							_ => None,
						},
						isolate: false,
					});
				} else if overflow_isolates == 0 {
					overflow_embeddings += 1;
				}
			}
			// X5a to X5c: the isolates. An isolate initiator is itself laid out at the level OUTSIDE
			// the isolate, which is the difference from an embedding.
			BidiClass::RLI | BidiClass::LRI | BidiClass::FSI => {
				let current = stack.last().map(|status| status.level).unwrap_or(paragraph_level);
				levels[index] = current;
				if let Some(status) = stack.last()
					&& let Some(override_class) = status.override_class
				{
					resolved[index] = override_class;
				}
				// X5c: a first-strong isolate takes its direction from its own contents.
				let rtl = match class {
					BidiClass::RLI => true,
					BidiClass::LRI => false,
					_ => {
						let end = matching_pdi[index].min(classes.len());
						crate::auto_level(&classes[index + 1..end]) == 1
					}
				};
				let next = next_level(current, rtl);
				if next <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
					valid_isolates += 1;
					stack.push(Status { level: next, override_class: None, isolate: true });
				} else {
					overflow_isolates += 1;
				}
			}
			// X6a: the pop directional isolate.
			BidiClass::PDI => {
				if overflow_isolates > 0 {
					overflow_isolates -= 1;
				} else if valid_isolates > 0 {
					overflow_embeddings = 0;
					while let Some(status) = stack.last() {
						if status.isolate {
							break;
						}
						stack.pop();
					}
					stack.pop();
					valid_isolates -= 1;
				}
				let current = stack.last().map(|status| status.level).unwrap_or(paragraph_level);
				levels[index] = current;
				if let Some(status) = stack.last()
					&& let Some(override_class) = status.override_class
				{
					resolved[index] = override_class;
				}
			}
			// X7: the pop directional format.
			BidiClass::PDF => {
				levels[index] = stack.last().map(|status| status.level).unwrap_or(paragraph_level);
				removed[index] = true;
				if overflow_isolates > 0 {
				} else if overflow_embeddings > 0 {
					overflow_embeddings -= 1;
				} else if stack.last().is_some_and(|status| !status.isolate) && stack.len() >= 2 {
					stack.pop();
				}
			}
			// X8: a paragraph separator takes the paragraph level, and everything resets. A
			// paragraph separator inside the text is the end of the paragraph as far as the levels
			// are concerned.
			BidiClass::B => {
				levels[index] = paragraph_level;
			}
			// X6: everything else takes the current level and the current override.
			_ => {
				let status = stack.last().copied().unwrap_or(Status { level: paragraph_level, override_class: None, isolate: false });
				levels[index] = status.level;
				if let Some(override_class) = status.override_class {
					resolved[index] = override_class;
				}
				// X9: the boundary-neutral characters take no part either.
				if class == BidiClass::BN {
					removed[index] = true;
				}
			}
		}
	}

	Working { levels, classes: resolved, removed, matching_pdi, matching_initiator }
}

/// The least greater odd or even level, as X2 to X5c ask for it.
fn next_level(current: u8, rtl: bool) -> u8 {
	if rtl { (current + 1) | 1 } else { (current + 2) & !1 }
}
