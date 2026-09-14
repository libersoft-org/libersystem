//! THE PROGRAMMABLE SHADER MODEL: a VALIDATED PORTABLE IR, and explicitly not Rust closures.
//!
//! A CLOSURE IS THE OBVIOUS SHORTCUT AND IT IS A DEAD END. A future GPU backend cannot take one, so
//! the day that backend arrives every shader in every application is rewritten. An IR costs more now
//! and costs nothing then, which is the whole argument.
//!
//! FIXED-FUNCTION LIGHTING CANNOT BE A COMPLETE RENDERER. A procedural material, a picking pass, a
//! scientific visualisation, a postprocess and every effect an application invents are all "a
//! different fragment function", and a renderer whose fragment stage is a fixed equation can express
//! none of them.
//!
//! WHAT IS SHIPPED IS A PROGRAMMATIC BUILDER AND NOT A LANGUAGE. A text shading language is a
//! separate, later, optional piece; writing one now would mean writing a parser, a type checker and
//! a diagnostic story before the first shader runs. The builder produces the same IR the language
//! would.
//!
//! THREE THINGS ARE UNREPRESENTABLE RATHER THAN CHECKED, which is stronger than validating them:
//!
//!   * RECURSION. There is no call instruction. A stage is one body, so a cycle cannot be written.
//!   * AN UNSTRUCTURED JUMP. Control flow is `If`, `Switch`, `Loop`, `Break`, `Continue`, `Return`
//!     and `Discard` over nested bodies; there is no label and no `goto`, so there is no irreducible
//!     graph to analyse.
//!   * AN UNBOUNDED LOOP. `Loop` CARRIES ITS TRIP COUNT as a field, so a loop without one does not
//!     type-check. A `while` is a `Loop` with a `Break` in it, and the bound is still required -
//!     which is what makes a frame's worst case computable.
//!
//! AND WHAT IS VALIDATED IS VALIDATED AT MODULE LOAD, not at draw time. A shader that loads and then
//! refuses to draw is a failure nobody can attribute; the refusal names the operation and the
//! dependency path that reached it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod builder;
pub mod ir;
pub mod validate;

pub use builder::Builder;
pub use ir::{BinaryOp, Binding, CompareKind, Constant, Interpolation, Module, Op, Sampling, ScalarType, Stage, Stmt, Transcendental, Type, UnaryOp, Value, Varying};
pub use validate::{Error, validate};

#[cfg(test)]
mod tests;
