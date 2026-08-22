#![no_std]

// WHAT EVERY DRIVER BINARY SHARES, as a library, because that is what it already was.
//
// These three were `mod` declarations repeated in each binary: the transport compiled seven times,
// and every binary reported the parts it does not call as dead code - which is a true fact about
// that binary and a false one about the module. The only way to say so in a binary crate was to
// switch the lint off at the top of the file, for all seven at once, which is how a genuinely dead
// duplicate of `read_isr` sat here unnoticed.
//
// A library says it properly: this is the surface a driver may use, and no binary owes it a caller.
// Nothing here is exempt from having ONE - the items with no caller anywhere in the tree were
// deleted before the move, not carried across by it.
pub mod common;
pub mod keys;
pub mod virtio;
