#![cfg_attr(not(test), no_std)]

extern crate alloc;

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
pub mod blk;
pub mod common;
pub mod descriptor;
pub mod gpu;
// THE HID REPORT-DESCRIPTOR PARSER AND REPORT DECODER. It was a module INSIDE the xHCI binary,
// where it could not be tested on the host at all - which is how a descriptor whose bit cursor
// overflows the admission check, and a short report that leaves the previous one's tail standing,
// both survived. Nothing about it is transport-specific: a USB HID device, an I2C one and a
// Bluetooth one all speak the same descriptors.
pub mod hid;
pub mod input;
pub mod keys;
pub mod net;
pub mod port;
pub mod snd;
pub mod usb;
// The in-controller class-module execution model: what a USB class driver IS in this system, and the
// per-class budget that stops two of them inside one Domain from starving each other.
pub mod usb_class;
pub mod virtio;
