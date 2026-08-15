#!/usr/bin/env python3
# Extract the WebAssembly specification's EXECUTABLE test cases into a flat fixture.
#
# `extract-wasm-spec-cases.py` beside this one answers "is this module accepted", which is the first
# rung and was for a long time the only one: a module that parses, validates, instantiates and then
# computes the WRONG ANSWER passes every assertion in that fixture. That is not hypothetical - this
# engine dropped a parameterised block's parameters at runtime while its own validator handled them
# correctly, so a valid module returned 50 where the specification says 40, and nothing in the tree
# could have noticed.
#
# The specification states those answers itself, as `(assert_return (invoke "f" args) expected)`. The
# reason the other extractor cannot reach them is that they follow TEXT-format modules, and parsing
# `.wat` is a project: of 17,199 assertions in the suite, 13 follow a `(module binary ...)`. So this
# one does not parse the text format - `wasm-tools json-from-wast` does, and this reads its output.
#
# Usage:
#   git clone --depth 1 --filter=blob:none --sparse https://github.com/WebAssembly/spec
#   cd spec && git sparse-checkout set test/core
#   cargo install wasm-tools
#   ./extract-wasm-spec-runs.py path/to/spec/test/core > src/wasm/tests/spec-run-cases.tsv
#
# INTEGER ASSERTIONS ONLY. Float equality in the suite is written with `nan:canonical`,
# `nan:arithmetic` and hex float literals, each with its own comparison rule; a fixture that
# flattened them into bit patterns would be asserting something the specification does not say. An
# assertion carrying any non-integer value is skipped WHOLE rather than half understood, because a
# dropped argument turns a real assertion into a different one that happens to pass.
#
# Output is two kinds of line, so a module shared by two hundred assertions is written once - once
# per DECLARATION of it, since two identical declarations are two instances and the assertions after
# the second are written against a fresh one:
#   M <index> <module bytes as hex>
#   R <module index> <source file> <export> <args> <expected>
# where `args` and `expected` are comma-separated `i32:<decimal>` / `i64:<decimal>` and `expected`
# is the literal `trap` for `assert_trap`.

import json
import os
import shutil
import subprocess
import sys
import tempfile

INT_TYPES = ('i32', 'i64')


def value(v):
	# One JSON value from wasm-tools: {"type": "i32", "value": "4294967295"}. The value is the
	# UNSIGNED bit pattern as a decimal string, which is what this fixture keeps - reinterpreting it
	# as signed is the reader's job and doing it here would lose which it was.
	if not isinstance(v, dict) or v.get('type') not in INT_TYPES:
		return None
	raw = v.get('value')
	if not isinstance(raw, str) or not raw.isdigit():
		return None
	return '%s:%s' % (v['type'], raw)


def values(vs):
	if not isinstance(vs, list):
		return None
	out = []
	for v in vs:
		one = value(v)
		if one is None:
			return None
		out.append(one)
	return out


def main():
	if len(sys.argv) != 2:
		print('usage: extract-wasm-spec-runs.py <spec/test/core>', file=sys.stderr)
		return 2
	root = sys.argv[1]
	if not shutil.which('wasm-tools'):
		print('wasm-tools is not on PATH; see the usage note at the top of this file', file=sys.stderr)
		return 2

	modules = {}
	rows = []
	for name in sorted(os.listdir(root)):
		if not name.endswith('.wast'):
			continue
		with tempfile.TemporaryDirectory() as work:
			out = os.path.join(work, 'cases.json')
			done = subprocess.run(['wasm-tools', 'json-from-wast', os.path.join(root, name), '-o', out, '--wasm-dir', work], capture_output=True)
			if done.returncode != 0:
				# A file this version of wasm-tools cannot convert is SAID, not skipped in silence:
				# the fixture's size is a number the tests assert against, and a quiet drop would
				# move it with nobody told.
				print('%s: wasm-tools declined (%s)' % (name, done.stderr.decode('utf-8', 'replace').strip().splitlines()[:1]), file=sys.stderr)
				continue
			doc = json.load(open(out, encoding='utf-8'))
			current = None
			current_index = None
			for command in doc.get('commands', []):
				kind = command.get('type')
				if kind == 'module':
					path = os.path.join(work, os.path.basename(command.get('filename', '')))
					current = open(path, 'rb').read() if os.path.exists(path) else None
					# A NEW OCCURRENCE, even when the bytes repeat. The index used to be
					# `modules.setdefault(bytes, len(modules))`, so two separate `(module ...)`
					# declarations that happened to assemble identically collapsed into one - and the
					# runner groups by this index and instantiates once per group, so the second
					# declaration inherited the first one's mutated globals and memory. The suite
					# writes its assertions against a fresh instance; instance identity is the
					# module's OCCURRENCE in the file, not its content.
					current_index = None
					continue
				# A module this engine could never be handed - a component, a text-only module - and
				# anything else that redefines what "the current module" is: the assertions after it
				# are about something whose bytes are not here.
				if kind in ('module_definition', 'module_instance', 'register'):
					current = None
					continue
				if kind not in ('assert_return', 'assert_trap', 'action') or current is None:
					continue
				action = command.get('action') or {}
				if action.get('type') != 'invoke' or action.get('module'):
					continue
				args = values(action.get('args'))
				if args is None:
					continue
				if kind == 'assert_trap':
					expected = 'trap'
				elif kind == 'action':
					# A BARE `(invoke ...)`: no assertion, but it MUTATES the instance, and the
					# assertions after it are written against the state it leaves. `float_memory.wast`
					# is the case that proves it - every one of its checks is preceded by a `reset`,
					# and a fixture that dropped those replayed thirteen loads against a memory the
					# specification had cleared. Replayed for its effect, with its answer ignored.
					expected = 'effect'
				else:
					got = values(command.get('expected'))
					if got is None:
						continue
					expected = ','.join(got)
				field = action.get('field', '')
				# `names.wast` exports through newlines, tabs and every other byte a name may hold.
				# Those cases are about NAMING and not about what an instruction computes, and a
				# line-oriented fixture cannot carry them - so they are dropped here rather than
				# written out to break the reader.
				if not field or any(c < ' ' or c > '~' for c in field):
					continue
				# Assigned on first use, so a module no assertion reaches is not written out.
				if current_index is None:
					current_index = len(modules)
					modules[current_index] = current
				rows.append((current_index, name, field, ','.join(args), expected))

	for index, data in sorted(modules.items()):
		print('M\t%d\t%s' % (index, data.hex()))
	for index, name, field, args, expected in rows:
		print('R\t%d\t%s\t%s\t%s\t%s' % (index, name, field.replace('\t', ' '), args, expected))
	return 0


if __name__ == '__main__':
	sys.exit(main())
