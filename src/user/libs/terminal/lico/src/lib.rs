//! Shared bounded terminal interaction primitives for LiberCommander.
//!
//! The crate owns no capability and performs no I/O itself. Applications supply a
//! `TerminalWriter` for mode changes, feed raw terminal bytes into `InputDecoder`,
//! and decode terminal-control messages through `decode_control`.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod control;
mod detect;
mod input;
mod session;
mod syntax;
mod text;
mod ui;

pub use control::{RESIZE_EVENT, TerminalControl, TerminalSize, WINSIZE_REPLY, WINSIZE_REQUEST, decode_control};
pub use detect::{FileType, detect_file_type};
pub use input::{InputDecoder, InputEvent, Key, PointerEvent};
pub use session::{MouseTracking, TerminalOptions, TerminalSession, TerminalWriter};
pub use syntax::{HighlightResult, LineState, MAX_CONTEXTS, MAX_DESCRIPTOR_BYTES, MAX_NESTING, MAX_RULES, MAX_STYLES, MAX_TOKEN_BYTES, StyleId, SyntaxDescriptor, SyntaxError, SyntaxMatchKind, SyntaxSelection, TokenSpan, parse_descriptor, select_descriptor};
pub use text::{DecodedText, REPLACEMENT_CHARACTER, TextDecoder};
pub use ui::{Binding, DialogKind, DialogState, Focus, MenuState, OperationState, Progress, dispatch_key};

#[cfg(test)]
mod tests;
