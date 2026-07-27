#!/usr/bin/env python3
# Build the fixtures the scenarios publish, into .build/scenario-fixtures.
#
# A fixture has to be a real image - the guest verifies the ELF, its target, its identity
# record and that the record names the artifact it is published as - so it is derived from a
# staged artifact rather than written from scratch. Altering one string in place keeps every
# one of those checks satisfied while making the two builds tellable apart, which is exactly
# what a scenario needs to prove which one was loaded.
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
STAGE = os.path.join(HERE, '../../../.build/system-image/x86_64-unknown-none')
OUT = os.path.join(HERE, '../../../.build/scenario-fixtures')


def shadowed_uname():
	source = os.path.join(STAGE, 'bin/uname')
	with open(source, 'rb') as handle:
		image = bytearray(handle.read())
	at = image.find(b'LiberSystem 0.0.1')
	if at < 0:
		raise SystemExit(f'{source}: no version string to alter; build the tree first')
	# Same length, so nothing in the image moves.
	image[at:at + 11] = b'SHADOWEDsys'
	return bytes(image)


os.makedirs(OUT, exist_ok=True)
target = os.path.join(OUT, 'uname-shadow')
with open(target, 'wb') as handle:
	handle.write(shadowed_uname())
print(f'scenario fixtures: wrote {os.path.relpath(target, os.path.join(HERE, "../.."))}')
sys.exit(0)
