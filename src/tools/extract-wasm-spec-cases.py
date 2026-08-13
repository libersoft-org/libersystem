#!/usr/bin/env python3
# Extract the WebAssembly specification's BINARY-form test cases into a flat fixture.
#
# The engine's own refusal corpus is hand-written, which is the right thing to have and cannot prove
# a rule is RIGHT: the same reading of the specification wrote both the rule and the test. That is
# not hypothetical here - `a_non_canonical_leb128_is_refused` was a well-built, watched test pinning
# behaviour the specification contradicts, and it took an outside reading to notice.
#
# The `.wast` format is a full S-expression language with a text-format module syntax, and parsing it
# is a project. What is directly usable without that is every case written as `(module binary "...")`
# - raw bytes with a stated outcome - which is most of what the binary-format and validation suites
# are made of, and exactly the areas this engine has been getting wrong.
#
# Usage:
#   git clone --depth 1 --filter=blob:none --sparse https://github.com/WebAssembly/spec
#   cd spec && git sparse-checkout set test/core
#   ./extract-wasm-spec-cases.py path/to/spec/test/core > src/wasm/tests/spec-binary-cases.tsv
#
# Output is one case per line: kind, source file, module bytes as hex, and the specification's own
# reason string. `kind` is `valid` (a bare module), `malformed` (must not parse) or `invalid` (must
# not validate).

import os
import re
import sys


def unescape(s):
	out = bytearray()
	i = 0
	while i < len(s):
		if s[i] == '\\':
			nxt = s[i + 1] if i + 1 < len(s) else ''
			if nxt in '0123456789abcdefABCDEF' and i + 2 < len(s) and s[i + 2] in '0123456789abcdefABCDEF':
				out.append(int(s[i + 1:i + 3], 16))
				i += 3
				continue
			simple = {'n': 10, 't': 9, 'r': 13, '"': 34, "'": 39, '\\': 92}
			if nxt in simple:
				out.append(simple[nxt])
				i += 2
				continue
		out.append(ord(s[i]))
		i += 1
	return bytes(out)


def strip_comments(t):
	# `;;` to end of line and `(; ... ;)` nesting, leaving string literals alone.
	out = []
	i = 0
	depth = 0
	while i < len(t):
		if t[i:i + 2] == '(;':
			depth += 1
			i += 2
			continue
		if t[i:i + 2] == ';)' and depth:
			depth -= 1
			i += 2
			continue
		if depth:
			i += 1
			continue
		if t[i:i + 2] == ';;':
			j = t.find('\n', i)
			i = len(t) if j < 0 else j
			continue
		if t[i] == '"':
			j = i + 1
			while j < len(t):
				if t[j] == '\\':
					j += 2
					continue
				if t[j] == '"':
					break
				j += 1
			out.append(t[i:j + 1])
			i = j + 1
			continue
		out.append(t[i])
		i += 1
	return ''.join(out)


def forms(t):
	# Each top-level parenthesised form, with string literals treated as opaque.
	i = 0
	while i < len(t):
		if t[i] != '(':
			i += 1
			continue
		depth = 0
		j = i
		while j < len(t):
			if t[j] == '"':
				j += 1
				while j < len(t):
					if t[j] == '\\':
						j += 2
						continue
					if t[j] == '"':
						break
					j += 1
			elif t[j] == '(':
				depth += 1
			elif t[j] == ')':
				depth -= 1
				if depth == 0:
					break
			j += 1
		yield t[i:j + 1]
		i = j + 1


BARE = re.compile(r'^\(module(?:\s+\$[^\s()]+)?\s+binary((?:\s*"(?:[^"\\]|\\.)*")+)\s*\)$', re.S)
ASSERTED = re.compile(r'^\((assert_malformed|assert_invalid)\s*(\(module(?:\s+\$[^\s()]+)?\s+binary(?:\s*"(?:[^"\\]|\\.)*")+\s*\))\s*"((?:[^"\\]|\\.)*)"\s*\)$', re.S)
LITERAL = re.compile(r'"(?:[^"\\]|\\.)*"')


def main():
	if len(sys.argv) != 2:
		print('usage: extract-wasm-spec-cases.py <spec/test/core>', file=sys.stderr)
		return 2
	root = sys.argv[1]
	for name in sorted(os.listdir(root)):
		if not name.endswith('.wast'):
			continue
		text = strip_comments(open(os.path.join(root, name), encoding='utf-8').read())
		for form in forms(text):
			form = form.strip()
			bare = BARE.match(form)
			if bare:
				data = b''.join(unescape(x[1:-1]) for x in LITERAL.findall(bare.group(1)))
				print(f'valid\t{name}\t{data.hex()}\t')
				continue
			asserted = ASSERTED.match(form)
			if asserted:
				data = b''.join(unescape(x[1:-1]) for x in LITERAL.findall(asserted.group(2)))
				kind = 'malformed' if asserted.group(1) == 'assert_malformed' else 'invalid'
				reason = asserted.group(3).replace('\t', ' ')
				print(f'{kind}\t{name}\t{data.hex()}\t{reason}')
	return 0


if __name__ == '__main__':
	sys.exit(main())
