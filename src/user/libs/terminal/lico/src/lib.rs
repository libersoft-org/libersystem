//! Shared bounded terminal interaction primitives for LiberCommander.
//!
//! The crate owns no capability and performs no I/O itself. Applications supply a
//! `TerminalWriter` for mode changes, feed raw terminal bytes into `InputDecoder`,
//! and decode terminal-control messages through `decode_control`.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod assoc;
mod buffer;
mod command;
mod control;
mod detect;
mod files;
mod input;
mod panel;
mod search;
mod session;
mod syntax;
mod text;
mod ui;

pub use assoc::{Action, Association, DEFAULT_ASSOCIATIONS, SETTINGS_VERSION, Settings, is_associable, resolve};
pub use buffer::{TextBuffer, TextBufferError, UNDO_BYTE_BUDGET};
pub use command::{CommandBar, MAX_COMMAND_BYTES, MAX_HISTORY_LINES, MAX_WORDS, ParseError, Request, classify, split};
pub use control::{RESIZE_EVENT, TerminalControl, TerminalSize, WINSIZE_REPLY, WINSIZE_REQUEST, decode_control};
pub use detect::{FileType, detect_file_type};
pub use files::{Criteria, CriteriaError, Frontier, MAX_OPERATION_DEPTH, MAX_OPERATION_ENTRIES, Operation, Overwrite, Plan, PlanError, Results, Source, Step, Tags, deepest_first, expand, glob_match, is_within, join, parse_criteria, plan, should_replace};
pub use input::{Chord, InputDecoder, InputEvent, Key, PointerEvent};
pub use panel::{Bookmarks, EntryKey, History, MAX_BOOKMARKS, MAX_HISTORY, SortKey, SortSpec, compare, order, quick_search};
pub use search::{HexPattern, HexPatternError, MAX_PATTERN_BYTES, TextQuery};
pub use session::{MouseTracking, TerminalGuard, TerminalOptions, TerminalSession, TerminalWriter};
pub use syntax::{HighlightResult, LineState, MAX_CONTEXTS, MAX_DESCRIPTOR_BYTES, MAX_NESTING, MAX_RULES, MAX_STYLES, MAX_TOKEN_BYTES, StyleId, SyntaxDescriptor, SyntaxError, SyntaxMatchKind, SyntaxSelection, TokenSpan, parse_descriptor, select_descriptor};
pub use text::{DecodedText, REPLACEMENT_CHARACTER, TextDecoder, TextRenderError, append_display_line};
pub use ui::{Binding, DialogKind, DialogState, Focus, MenuState, OperationState, Progress, dispatch_key};

#[cfg(test)]
mod tests;
