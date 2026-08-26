#!/usr/bin/env bash
# Every symbol in an architecture's compiled contract is a path that architecture can execute.
#
# WHAT THIS REFUSES, AND WHY IT IS A GATE RATHER THAN A READING. The three backends used to carry
# twenty `todo!()` bodies between them - the x86 loader hand-off, answered by every port that does
# not arrive through it. None was reachable: aarch64 enters `aarch64::boot::aarch64_main` and
# riscv64 `riscv64::boot::riscv64_main`, each bringing up its own console, page tables, per-CPU
# register, interrupt controller, timer, syscall vector and secondary cores. But a static scan and a
# reader both saw unfinished interrupt and timer glue, and the only thing separating the two
# readings was a paragraph of prose. The bodies went by removing the requirement: the
# hand-off compiles for x86_64 alone. This keeps them from coming back one convenient stub at a time.
#
# A TEST FAULT PROBE IS NOT AN EXCEPTION TO THIS. The suites deliberately fault to prove a handler
# runs, and those live in `tests.rs` files or behind `#[cfg(test)]`, which is what makes them
# identifiable as tests rather than as a hole in the contract.
# THE THIRD CONSTRUCT, AND WHY THIS STOPPED BEING A LINE SCAN. The sentence this gate enforces names
# three answers - `todo!`, `unimplemented!` and an unconditional placeholder panic - and only the
# first two were ever searched for. A function whose entire production body is `panic!("not on this
# port")` passed, which is the same stub wearing a different macro.
#
# It could not simply be added to the pattern. A `panic!` in architecture code is very often CORRECT:
# a firmware value this port cannot address, a state the machine must not be in, a table that
# contradicts itself. Banning the macro would push those refusals into silent fallbacks, which is the
# failure this tree keeps finding. Telling a refusal from a stub needs the ITEM, not the line - so the
# scan is a small Rust program that walks the file as tokens, knows what is inside a comment, a string
# or a character literal, and matches a `#[cfg(test)]` item by its real extent rather than by guessing
# where a block ends. The filter it replaces skipped whatever followed that attribute until the braces
# looked balanced, which is right for a braced module and wrong for every other shape it takes.
set -euo pipefail

cd "$(dirname "$0")/../.."
SCANNER="src/tools/arch-surface/Cargo.toml"

# THE SCANNER PROVES IT REFUSES BEFORE IT IS TRUSTED TO APPROVE. Fifteen sources, six that must be
# reported and nine that must not - a panic after a check, a brace inside a string, a placeholder
# under `#[cfg(test)]` with no block of its own. A clean tree proves nothing about a scan that has
# stopped matching, which is how this gate came to be enforcing two thirds of its own sentence.
cargo run --quiet --offline --manifest-path "$SCANNER" -- --self-test || exit 1
cargo run --quiet --offline --manifest-path "$SCANNER" -- src/kernel/arch || exit 1
