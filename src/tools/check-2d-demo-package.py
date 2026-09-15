#!/usr/bin/env python3
"""The 2D demo's PACKAGE audit: what the artifact is, before anything boots it.

WHAT A PICTURE CANNOT PROVE. The live capture beside this shows the demo draws the scene; what it
cannot show is HOW - a program carrying its own copy of the rasteriser draws exactly the same
picture, and so does one holding capabilities it has no business with. These are the claims that are
about the artifact rather than the frame: it is position-independent, it declares the providers it
actually imports from, its drawing comes from the SHARED libraries rather than from a static copy,
and its grants are the two the milestone allows.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROGRAM = 'test2d-sw'
# THE DRAWING STACK, WHICH IS THE POINT OF THE DUPLICATION CHECK: every one of these has to appear as
# the OWNER of an imported symbol, because an owner is what a shared library is - a static copy owns
# nothing and imports nothing.
DRAWING = ('render2d', 'soft2d', 'graphics-app', 'surface')
GRANTS = ('Display', 'InputKeys')


def manifest_providers():
	text = (ROOT / 'src/user/services/manifest.toml').read_text()
	block = text[text.index(f'name = "{PROGRAM}"') :]
	block = block[: block.index('\n[[')]
	row = re.search(r'providers = \[(.*?)\]', block, re.S)
	if not row:
		raise SystemExit(f'2d-demo-package: the manifest row for {PROGRAM} declares no providers')
	return {name.strip().strip('"') for name in row.group(1).split(',') if name.strip()}


def report_rows():
	rows = []
	for line in (ROOT / 'docs/DYNAMIC_EXECUTABLES.tsv').read_text().splitlines():
		fields = line.split('\t')
		if len(fields) > 3 and fields[2] == PROGRAM:
			rows.append(fields)
	return rows


def granted_capabilities():
	text = (ROOT / 'src/user/services/core/src/permission_manager.rs').read_text()
	row = re.search(rf'b"{PROGRAM}" => Some\(granted\("{PROGRAM}", alloc::vec!\[(.*?)\]\)\)', text, re.S)
	if not row:
		raise SystemExit(f'2d-demo-package: the permission manager has no grant row for {PROGRAM}')
	return {name.strip().replace('Capability::', '') for name in row.group(1).split(',') if name.strip()}


def main():
	problems = []
	declared = manifest_providers()
	rows = report_rows()
	targets = {row[1] for row in rows}
	if len(targets) != 3:
		problems.append(f'the report carries {PROGRAM} for {sorted(targets)} rather than for all three targets')

	for row in rows:
		target = row[1]
		owners = {owner.split('=', 1)[1] for owner in row[4].split(',') if '=' in owner}
		reported = {name.removesuffix('.lslib') for name in row[6].split(',') if name}
		if reported != declared:
			problems.append(f'{target}: the built providers {sorted(reported)} are not the manifest\'s {sorted(declared)}')
		missing = [name for name in DRAWING if name not in owners]
		if missing:
			problems.append(f'{target}: nothing is imported from {missing}, so that code is a static copy rather than the shared library')
		if int(row[10]) <= 0:
			problems.append(f'{target}: the artifact reports no PIE bytes, so it is not position-independent')

	grants = granted_capabilities()
	if grants != set(GRANTS):
		problems.append(f'the grants are {sorted(grants)} where the demo may hold exactly {sorted(GRANTS)}')

	for problem in problems:
		print(f'2d-demo-package: {problem}')
	if problems:
		return 1
	print(f'2d-demo-package: position-independent on {len(targets)} target(s), {len(declared)} declared provider(s) that the build confirms, its drawing imported from {", ".join(DRAWING)}, and exactly the {" + ".join(GRANTS)} grants')
	return 0


if __name__ == '__main__':
	sys.exit(main())
