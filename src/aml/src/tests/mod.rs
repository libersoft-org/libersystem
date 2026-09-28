//! THE INTERPRETER'S CONFORMANCE AND HOSTILE SUITES: the specification's constructs encoded with `build`, run against
//! a register model that records every access, and firmware written to break the bounds.

mod conformance;
mod hostile;
mod resources;

pub use crate::testing::{Model, build, eval, load, load_revision};

use crate::object::Object;

/// The error an evaluation answered - it must have failed.
pub fn failure(result: Result<Option<Object>, crate::error::Error>) -> crate::error::Error {
	match result {
		Err(error) => error,
		Ok(value) => panic!("an error was expected, not {value:?}"),
	}
}

pub fn integer(result: Result<Option<Object>, crate::error::Error>) -> u64 {
	match result {
		Ok(Some(Object::Integer(value))) => value,
		other => panic!("an integer was expected, not {other:?}"),
	}
}
