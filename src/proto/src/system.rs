//! Compatibility facade for the former monolithic `liber:system@1` module.

pub use crate::generated::liber::audio::v1::*;
pub use crate::generated::liber::base::v1::*;
// The host stack's CLIENT contracts. The HCI transport a controller publishes is a DEVICE contract
// and is in `device` with the other driver wires, for the same reason the display's device side is
// separate from the display's.
pub use crate::generated::liber::bluetooth::v1::*;
pub use crate::generated::liber::config::v1::*;
pub use crate::generated::liber::device::v1::*;
pub use crate::generated::liber::display::v1::*;
// The DEVICE side of the display, which is a different contract from the application side: the
// service calls it and a driver serves it.
pub use crate::generated::liber::display_device::v1::*;
// The shared graphics value types - extent, rect, pixel format, colour space, alpha mode, row origin
// and damage. `liber:display@1` declared its own one-member `pixel-format` until 2026-09-13; it
// imports this one now, and so does every other graphics interface.
pub use crate::generated::liber::graphics::v1::*;
pub use crate::generated::liber::input::v1::*;
pub use crate::generated::liber::log::v1::*;
pub use crate::generated::liber::network::v1::*;
pub use crate::generated::liber::observability::v1::*;
pub use crate::generated::liber::process::v1::*;
pub use crate::generated::liber::resources::v1::*;
pub use crate::generated::liber::security::v1::*;
pub use crate::generated::liber::session::v1::*;
pub use crate::generated::liber::storage::v1::*;
pub use crate::generated::liber::time::v1::*;
