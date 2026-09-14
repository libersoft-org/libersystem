//! THE PASS GRAPH: which passes run, in what order, and what each reads.
//!
//! AN ORDER DERIVED FROM DEPENDENCIES AND NOT DECLARED. A caller that stated the order would have to
//! restate it every time a pass was added, and the first time somebody forgot, a shadow map would be
//! sampled before it was rendered - which looks like a shadow bug rather than an ordering one.
//!
//! A CYCLE IS REFUSED WITH THE PASS THAT CLOSES IT NAMED. "The graph has a cycle" is not something a
//! caller can act on in a graph of thirty passes.

use alloc::vec::Vec;

use crate::scene::Error;

/// One pass: what it writes, and what it reads.
#[derive(Clone, PartialEq, Debug)]
pub struct Pass {
	/// The caller's own identifier for this pass.
	pub id: u32,
	/// The offscreen targets it writes. A pass that writes none is a pass that draws to the screen.
	pub writes: Vec<u32>,
	/// The targets it samples.
	pub reads: Vec<u32>,
}

/// The graph.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PassGraph {
	passes: Vec<Pass>,
}

impl PassGraph {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn add(&mut self, pass: Pass) {
		self.passes.push(pass);
	}

	pub fn passes(&self) -> &[Pass] {
		&self.passes
	}

	/// The order the passes run in: every writer before every reader of what it wrote.
	///
	/// KAHN'S ALGORITHM, with the pass ORDER as the tiebreak - so a graph with two independent passes
	/// runs them in the order they were added rather than in whichever order a set happened to
	/// iterate in. A frame whose pass order varies between runs is a frame that differs between runs.
	pub fn order(&self) -> Result<Vec<u32>, Error> {
		let count = self.passes.len();
		let mut incoming: Vec<u32> = alloc::vec![0; count];
		// An edge from the writer of a target to every reader of it.
		let mut edges: Vec<(usize, usize)> = Vec::new();
		for (reader, pass) in self.passes.iter().enumerate() {
			for target in &pass.reads {
				let mut written = false;
				for (writer, other) in self.passes.iter().enumerate() {
					if writer != reader && other.writes.contains(target) {
						edges.push((writer, reader));
						incoming[reader] += 1;
						written = true;
					}
				}
				if !written {
					// A PASS THAT READS WHAT NOTHING WRITES is a mistake with a name. The alternative
					// is a pass that samples an undefined target, which the resource model already
					// refuses - but naming it here says WHICH pass.
					return Err(Error::NoSuchTarget { target: *target });
				}
			}
		}
		let mut ready: Vec<usize> = (0..count).filter(|index| incoming[*index] == 0).collect();
		let mut out: Vec<u32> = Vec::new();
		while let Some(next) = ready.first().copied() {
			ready.remove(0);
			out.push(self.passes[next].id);
			for (from, to) in &edges {
				if *from == next {
					incoming[*to] -= 1;
					if incoming[*to] == 0 {
						ready.push(*to);
						// THE TIEBREAK IS THE PASS ORDER, so the frame does not depend on which
						// dependency happened to be satisfied last.
						ready.sort_unstable();
					}
				}
			}
		}
		if out.len() != count {
			// The first pass still with an incoming edge is the one to name: it is on the cycle.
			let stuck = incoming.iter().position(|remaining| *remaining > 0).unwrap_or(0);
			return Err(Error::PassCycle { pass: self.passes[stuck].id });
		}
		Ok(out)
	}
}
