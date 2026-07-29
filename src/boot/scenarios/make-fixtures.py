#!/usr/bin/env python3
# Build the fixtures the scenarios publish, into .build/scenario-fixtures.
#
# A fixture has to be a real image - the guest verifies the ELF, its target, its identity
# record and that the record names the artifact it is published as - so it is derived from a
# staged artifact rather than written from scratch. Altering one string in place keeps every
# one of those checks satisfied while making the two builds tellable apart, which is exactly
# what a scenario needs to prove which one was loaded.
#
# Every alteration is same-length by construction, so nothing in the image moves and no
# offset, size or relocation recorded anywhere in it becomes wrong.
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
STAGE = os.path.join(HERE, '../../../.build/system-image/x86_64-unknown-none')
OUT = os.path.join(HERE, '../../../.build/scenario-fixtures')


def staged(path):
	source = os.path.join(STAGE, path)
	try:
		with open(source, 'rb') as handle:
			return bytearray(handle.read())
	except OSError as error:
		raise SystemExit(f'{source}: {error}; build the tree first')


# Replace one exact byte string with another of the same length, everywhere it occurs, and
# insist that it occurred exactly as often as expected. A fixture that silently patched
# nothing, or patched one place when it meant two, would still be a valid image - and the
# scenario built on it would then be asserting something other than what it says. The count is
# stated rather than tolerated so a rebuild that changes where a string lands fails here,
# loudly, instead of turning a scenario into one that passes without testing anything.
def substitute(image, old, new, what, occurrences=1):
	if len(old) != len(new):
		raise SystemExit(f'{what}: {old!r} and {new!r} differ in length')
	found = image.count(old)
	if found != occurrences:
		raise SystemExit(f'{what}: {old!r} occurs {found} times, expected {occurrences}')
	return bytearray(bytes(image).replace(old, new))


# Give the identity record a different content digest. The compatibility rule allows exactly
# this one field to differ, and a fixture that left it alone would prove less than it looks
# like it proves: the loader compares provider digests, and two images with identical records
# compare equal without the rule ever being consulted.
def alter_source_digest(image, what):
	key = b'source-sha256='
	at = image.find(key)
	if at < 0:
		raise SystemExit(f'{what}: no identity record to alter')
	first = at + len(key)
	image[first:first + 1] = b'0' if image[first:first + 1] != b'0' else b'1'
	return image


# The installed `uname` with its own version string altered: an executable generation that is
# distinguishable at a glance from the artifact it shadows.
def shadowed_uname():
	return substitute(staged('bin/uname'), b'LiberSystem', b'SHADOWEDsys', 'uname')


# Three generations of the same executable, each naming itself. The self-test publishes them in
# order against one guest, so what has to be readable from the program's own output is not that
# it was shadowed but WHICH generation answered - a test that only distinguished shadowed from
# installed would pass just as well if the second and third publications had done nothing.
# `GENERATION1` is the same eleven characters as `LiberSystem`, so nothing in the image moves.
def generation_uname(index):
	return substitute(staged('bin/uname'), b'LiberSystem', f'GENERATION{index}'.encode(), f'uname generation {index}')


# A provider generation. `process-proto` renders the process records `ps` prints, so altering
# the field name it writes changes the output of a program that is not itself published -
# which is what makes it visible that the provider, and only the provider, was replaced.
# Nothing else about the image moves, so the rule calls it hot-publishable.
#
# The name appears twice: once in the packed string the JSON and CBOR renderers index into,
# and once as an immediate the text renderer stores directly. Both are patched, so every
# rendering of a process record agrees about which build produced it.
def shadowed_process_proto():
	image = substitute(staged('lib/protocol/process-proto.lslib'), b'koid', b'PKID', 'process-proto', occurrences=2)
	return alter_source_digest(image, 'process-proto')


# The same provider, altered in a field the rule refuses: the feature set an image was built
# with changes its code under an unchanged name, which is precisely what a process that has
# already resolved against it cannot be handed. Published successfully - the registry holds
# what it is given - and refused at the launch that would load it.
def incompatible_process_proto():
	return substitute(staged('lib/protocol/process-proto.lslib'), b'features=shared-image', b'features=shadow-image', 'process-proto')


os.makedirs(OUT, exist_ok=True)
fixtures = [('uname-shadow', shadowed_uname), ('process-proto-shadow', shadowed_process_proto), ('process-proto-incompatible', incompatible_process_proto)]
fixtures += [(f'uname-generation{index}', lambda index=index: generation_uname(index)) for index in (1, 2, 3)]
for name, build in fixtures:
	target = os.path.join(OUT, name)
	with open(target, 'wb') as handle:
		handle.write(bytes(build()))
	print(f'scenario fixtures: wrote {os.path.relpath(target, os.path.join(HERE, "../.."))}')
sys.exit(0)
