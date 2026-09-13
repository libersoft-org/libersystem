//! HOW A PROFILE IS HASHED, and what the hash is a hash OF.
//!
//! Every profile in this tree carries a SHA-256, and a hash is only worth what its input rule is.
//! Two encoders that disagree about whether a list is sorted, or about how a float is written,
//! produce two hashes for one profile - and a gate comparing them reports a change nobody made.
//!
//! THE SEMANTIC INPUT IS THE REGISTRY, NOT THE MARKDOWN. A profile's meaning is its enumerations,
//! constants, equations and lookup data; the document is the normative EXPLANATION of that meaning.
//! Hashing the Markdown would make rewrapping a paragraph a profile change and would let a
//! correction to a number pass unnoticed if the prose around it was rewritten at the same time. The
//! document is bound to the profile through the manifest instead: it states the hash, and the gate
//! checks that the hash it states is the one its registry produces.

use crate::render3d_spec::Rule;

/// The rules a canonical form is written under. Every generator in `profile-doc` follows them.
pub const CANONICAL_ENCODING: &[Rule] = &[
	Rule { question: "the input inventory", answer: "every registry the profile is made of, in a FIXED order written in the generator - not in the order a directory listing or a hash map produces, which differ between runs and between machines" },
	Rule { question: "within a registry", answer: "the DECLARATION ORDER of the list, which is part of the profile: several of these lists are ordered on purpose - the clip planes, the render queues, the light accumulation - and sorting them would destroy the meaning the order carries" },
	Rule { question: "the text encoding", answer: "UTF-8, with LF line endings and exactly one trailing newline per line. A CRLF anywhere would make the same profile hash differently on a machine that checked out with translation" },
	Rule { question: "string encoding", answer: "verbatim, with no trimming, case folding or whitespace collapsing. A string in a registry is a value, and normalising it would make two different answers hash the same" },
	Rule { question: "integer encoding", answer: "decimal, no separators, no leading zeros, with a leading `-` for a negative value" },
	Rule { question: "float encoding", answer: "a fixed number of decimal places per field, chosen so the value round-trips - four places for a sample position, twelve for an epsilon. Never the shortest representation, which changes when a compiler's formatter changes" },
	Rule { question: "boolean encoding", answer: "`true` and `false`, which is what a reader of the canonical file sees rather than 1 and 0" },
	Rule { question: "the hash field", answer: "EXCLUDED from its own input. The canonical form contains no hash, so the hash is a function of the profile alone and a document that states it cannot change it" },
	Rule { question: "what the document contributes", answer: "NOTHING. The Markdown is generated FROM the canonical form and is not an input to it, so a prose rewrap or a corrected example is not a profile change" },
	Rule { question: "when the hash may change", answer: "when the semantic input changes, and then the profile's VERSION changes with it. A semantic correction is a version change; a non-normative correction is neither" },
	Rule { question: "what the gate checks", answer: "that each document states the hash its registry produces, that the manifest names the same hash, and that no generated coverage table claims a feature the profile does not have" },
];
