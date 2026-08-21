#!/usr/bin/env python3
# A hand-written `extern` declaration must match the generated function it is forwarded to.
#
# The client crates reach the generated protocol code through a symbol pair: the client declares
# `liber_channel_<iface>_<op>` in an `extern` block, and the provider crate defines that symbol as a
# BARE JUMP to `liber_channel_impl_<iface>_<op>`, which the generator emits. A jump does not adapt
# anything - it hands the callee the registers the caller set up - so the two signatures have to
# agree, and NOTHING checks that they do. An `extern` declaration is a promise the compiler
# believes, the symbol resolves at link time by name alone, and the link succeeds.
#
# WHAT IT COST. `writer.write` was declared `(u64, &[u8])` and implemented `(u64, &Vec<u8>)`. A
# slice is a fat pointer - address and length in two registers - and a `&Vec` is a thin one, so the
# callee read the caller's DATA POINTER as the address of a Vec and took the length out of the
# bytes being written. Every transactional write from every program - `cp`, `mv`, `tee`, `>`
# redirection, the editor's save, the file manager's copy - encoded a nonsense length and returned
# "no answer". It compiled without a warning and had never worked.
#
# The generator now emits `&[T]` for a borrowed list, so that pair agrees by construction. This is
# for the next one: a declaration written by hand beside a signature written by a program.
import os
import re
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# `crate::codec::Buffer` and `Buffer` are the same type seen from inside and outside the generated
# crate. Comparing the spelling would report every such pair as a mismatch, which is noise rather
# than a finding - the paths are checked by the compiler on the generated side.
def normalise(params):
	return [p.replace("crate::codec::", "").replace("crate::", "") for p in params]


def parameter_types(text):
	return [part.split(":", 1)[1].strip() for part in text.split(",") if ":" in part]


def rust_files(*roots):
	for base in roots:
		for directory, _, files in os.walk(base):
			for name in files:
				if name.endswith(".rs"):
					yield os.path.join(directory, name)


implementations = {}
for path in rust_files(os.path.join(root, "user", "libs", "protocol")):
	source = open(path, encoding="utf-8").read()
	for match in re.finditer(r'export_name = "(liber_channel_impl_[a-z0-9_]+)"\)\]\s*\n\s*(?:pub )?(?:unsafe )?fn \w+\(([^)]*)\)', source):
		implementations[match.group(1).replace("liber_channel_impl_", "liber_channel_")] = parameter_types(match.group(2))

if not implementations:
	print("check-forwarded-abi: no generated implementations found - the sweep would pass vacuously", file=sys.stderr)
	sys.exit(1)

mismatches = []
declarations = 0
for path in rust_files(os.path.join(root, "user"), os.path.join(root, "kernel")):
	if "generated" in path:
		continue
	source = open(path, encoding="utf-8").read()
	for match in re.finditer(r'#\[link_name = "(liber_channel_[a-z0-9_]+)"\]\s*\n\s*(?:pub )?(?:unsafe )?fn \w+\(([^)]*)\)', source):
		symbol = match.group(1)
		if symbol not in implementations:
			continue
		declarations += 1
		declared = parameter_types(match.group(2))
		if normalise(declared) != normalise(implementations[symbol]):
			mismatches.append((os.path.relpath(path, root), symbol, declared, implementations[symbol]))

for path, symbol, declared, implemented in mismatches:
	print(f"{path}: {symbol}", file=sys.stderr)
	print(f"  declared    ({', '.join(declared)})", file=sys.stderr)
	print(f"  implemented ({', '.join(implemented)})", file=sys.stderr)

if mismatches:
	print(f"check-forwarded-abi: {len(mismatches)} declaration(s) disagree with the generated function they jump to", file=sys.stderr)
	sys.exit(1)

print(f"check-forwarded-abi: {declarations} forwarded declaration(s) match their generated implementations")
