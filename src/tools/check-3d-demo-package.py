#!/usr/bin/env python3
"""The 3D demo's PACKAGE audit: what the artifact is, before anything boots it.

WHAT A PICTURE CANNOT PROVE. The live capture beside this shows the demo draws the scene; what it
cannot show is HOW - a program carrying its own copy of the rasteriser draws exactly the same
picture, and so does one holding capabilities it has no business with. These are the claims that are
about the artifact rather than the frame: it is position-independent, it imports its rendering from
the SHARED libraries rather than from a static copy, the providers it links are the ones its manifest
row declares, its identity record lists them in the order a launch will read, and its grants are the
two the milestone allows.

IT READS THE BUILT ARTIFACT AND NOT A GENERATED REPORT. The 2D audit beside it reads
`docs/DYNAMIC_EXECUTABLES.tsv`, which carries a row per target only once every target has been built;
this one walks whatever is staged under `.build/image` and audits each of them, so it says something
true on the first target and more as the others arrive.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROGRAM = 'test3d-sw'
# THE STAGED NAME HAS NO SUFFIX. `.lsexe` is what the artifact is called on the VOLUME; what the
# build leaves under `.build/image` is the program itself.
ARTIFACT = f'bin/{PROGRAM}'
# THE RENDERING STACK, WHICH IS THE POINT OF THE DUPLICATION CHECK: every one of these has to appear
# among the artifact's UNDEFINED symbols, because that is what linking against a shared library looks
# like - a static copy defines its own and imports nothing.
#
# Both halves are here on purpose. The 3D demo is also the 2D/3D interop proof, so a build in which
# the overlay quietly became a private copy of `render2d` would still draw the same HUD.
#
# `render3d` IS NOT ON THIS LIST AND MUST NOT BE. It is the backend-neutral API: what an application
# takes from it is types, constants and generics, every one of which the compiler inlines, so a
# correctly linked program imports NO symbol from it - which is also why its manifest row does not
# name it. Requiring an import here would be requiring the build to be wrong.
RENDERING = ('soft3d', 'render_shader', 'render_math', 'render2d', 'soft2d', 'graphics_app', 'graphics_core', 'surface')
GRANTS = ('Display', 'InputKeys')


def manifest_providers():
	text = (ROOT / 'src/user/services/manifest.toml').read_text()
	block = text[text.index(f'name = "{PROGRAM}"') :]
	block = block[: block.index('\n[[')]
	row = re.search(r'providers = \[(.*?)\]', block, re.S)
	if not row:
		raise SystemExit(f'3d-demo-package: the manifest row for {PROGRAM} declares no providers')
	return {name.strip().strip('"') for name in row.group(1).split(',') if name.strip()}


def granted_capabilities():
	text = (ROOT / 'src/user/services/core/src/permission_manager.rs').read_text()
	row = re.search(rf'b"{PROGRAM}" => Some\(granted\("{PROGRAM}", alloc::vec!\[(.*?)\]\)\)', text, re.S)
	if not row:
		raise SystemExit(f'3d-demo-package: the permission manager has no grant row for {PROGRAM}')
	return {name.strip().replace('Capability::', '') for name in row.group(1).split(',') if name.strip()}


def readelf(*arguments):
	return subprocess.run(['llvm-readelf', *arguments], check=True, capture_output=True, text=True).stdout


def audit(path, declared, problems):
	target = path.parents[1].name
	headers = readelf('--file-headers', str(path))
	if 'DYN (' not in headers:
		problems.append(f'{target}: the artifact is not ET_DYN, so it is not the position-independent executable the milestone asks for')

	dynamic = readelf('--dynamic', str(path))
	needed = {match.group(1).removesuffix('.lslib') for match in re.finditer(r'Shared library: \[([^\]]+)\]', dynamic)}
	if needed != declared:
		problems.append(f'{target}: it links {sorted(needed)} where its manifest row declares {sorted(declared)}')

	symbols = readelf('--wide', '--dyn-symbols', str(path))
	undefined = [line for line in symbols.splitlines() if ' UND ' in line]
	for crate in RENDERING:
		if not any(crate in line for line in undefined):
			problems.append(f'{target}: nothing is imported from {crate}, so that code is a static copy rather than the shared library')

	# THE IDENTITY RECORD'S PROVIDER LIST, IN THE ORDER A LAUNCH READS IT. `parse_identity` refuses a
	# record whose `provider=` lines are not STRICTLY ASCENDING by bytes - that is what makes a
	# duplicate impossible to express - so a record in any other order is one no launch will read,
	# with every provider present and every digest agreeing.
	note = path.read_bytes()
	providers = [match.group(1).decode() for match in re.finditer(rb'provider=([a-z0-9_-]+):[0-9a-f]{64}', note)]
	if providers != sorted(providers):
		problems.append(f'{target}: the identity record lists providers in an order no launch will read: {providers}')
	if len(providers) != len(set(providers)):
		problems.append(f'{target}: the identity record names a provider twice: {providers}')
	return target


def main():
	problems = []
	declared = manifest_providers()
	staged = sorted((ROOT / '.build/image').glob(f'*/{ARTIFACT}'))
	if not staged:
		print(f'3d-demo-package: {ARTIFACT} is staged for no target - build first:  ./build.sh')
		return 1

	targets = [audit(path, declared, problems) for path in staged]

	grants = granted_capabilities()
	if grants != set(GRANTS):
		problems.append(f'the grants are {sorted(grants)} where the demo may hold exactly {sorted(GRANTS)}')

	for problem in problems:
		print(f'3d-demo-package: {problem}')
	if problems:
		return 1
	print(f'3d-demo-package: position-independent on {len(targets)} target(s) ({", ".join(targets)}), {len(declared)} declared provider(s) the link confirms in ascending order, its rendering imported from {", ".join(RENDERING)}, and exactly the {" + ".join(GRANTS)} grants')
	return 0


if __name__ == '__main__':
	sys.exit(main())
