//! What a FACE is, and what makes two loads of one file the same face.
//!
//! A FACE IS AN IDENTITY PLUS AN INDEX. A TrueType or OpenType COLLECTION is one file, so every face
//! in it shares the content-derived identity of that file, and two of them can carry the same glyph
//! index meaning different glyphs. A contract that called the file the face would make those two
//! indistinguishable everywhere the identity is used - a cache key, a fallback decision, a resolve.
//!
//! THE IDENTITY IS ISSUED BY THE CATALOGUE, FROM THE FILE. It is not computed by a client from a
//! buffer the client can write: a client that rewrote its own transport buffer would otherwise have
//! changed what the identity NAMES. Two loads of the same bytes are one face because the catalogue
//! says so, and a changed file is a different face for the same reason.
//!
//! THE GENERATION IS WHAT INVALIDATES. A replaced face is a different identity, and the generation
//! is the catalogue's published counter that moved when it noticed - so everything derived from a
//! face carries the generation it was derived under, and nothing needs to be told twice.

/// The content-derived identity of a face FILE: the digest the catalogue published for it.
///
/// THIRTY-TWO BYTES, AND OPAQUE. What function produced them is the catalogue's business; a consumer
/// compares them and never interprets them, which is what keeps the digest replaceable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileIdentity(pub [u8; 32]);

/// The catalogue's published generation.
///
/// It advances when a face is published, replaced or withdrawn, and it is carried by everything
/// derived from a face so that a replacement invalidates it. Comparing generations is the only
/// operation: a consumer never guesses what a difference means beyond "re-ask".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Generation(pub u64);

/// A face: the file it is in, and which face within that file.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FaceIdentity {
	pub file: FileIdentity,
	/// Which face within the file. Zero for a single-face file, and the index within a collection.
	pub index: u32,
}

/// A face as a consumer holds it: which face, and the generation it was answered under.
///
/// THE PAIR TRAVELS TOGETHER because neither half is usable alone. An identity without a generation
/// cannot be invalidated; a generation without an identity does not say what it is about.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FaceRef {
	pub face: FaceIdentity,
	pub generation: Generation,
}

/// The validated bytes of a face, OWNED by the library that parses them.
///
/// THIS TYPE IS THE OWNERSHIP RULE, EXPRESSED SO IT CANNOT BE SKIPPED. The transport buffer a
/// resolve fills belongs to the CALLER and stays writable by it - this kernel has no seal for a
/// memory object and the caller created the object - so a parser that borrowed from that buffer
/// would hold decoded structures pointing into bytes that can change under it. The library TAKES the
/// bytes instead: it copies them into storage it owns, and every decoded structure references that
/// copy.
///
/// WHAT THIS DOES AND DOES NOT PROMISE. After the take, the caller may write its transport buffer
/// freely and nothing the library holds changes. What is left undefended, and it is named rather
/// than papered over, is a caller corrupting its OWN parse by writing the buffer BEFORE the take -
/// a caller harming itself inside its own Domain, which affects no other client, no cache and not
/// the catalogue.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FaceBytes {
	face: FaceRef,
	bytes: alloc::vec::Vec<u8>,
}

impl FaceBytes {
	/// TAKE the bytes. There is deliberately no constructor that borrows: the only way to build this
	/// is to hand over ownership of a copy.
	pub fn take(face: FaceRef, bytes: alloc::vec::Vec<u8>) -> Self {
		Self { face, bytes }
	}

	/// Copy out of a transport buffer, which is what a caller with a filled resolve buffer does.
	pub fn copy_from(face: FaceRef, transport: &[u8]) -> Self {
		Self { face, bytes: transport.to_vec() }
	}

	pub fn face(&self) -> FaceRef {
		self.face
	}

	pub fn bytes(&self) -> &[u8] {
		&self.bytes
	}

	/// Is what was derived from this face still current?
	pub fn is_current(&self, published: Generation) -> bool {
		self.face.generation == published
	}
}
