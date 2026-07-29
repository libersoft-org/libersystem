#!/usr/bin/env python3
# The development loop's own self-test: one guest, several generations, one refusal, one
# rollback, no reboot.
#
# Everything else in this tree tests a piece of the loop. The protocol suite tests the wire, the
# scenarios test what a published artifact does, the performance gate tests what an iteration
# costs. What none of them tests is the property the whole persistent-instance design exists to
# provide: that a single boot can absorb generation after generation, refuse a bad one without
# damage, and give a previous one back - and that at the end of all that it is the same guest
# that started.
#
# So the assertions here are about succession rather than about any one publication:
#
#   three generations   each is published, then asked to identify itself, in order
#   a refusal           a valid image published under a name its record does not claim
#   no damage           after the refusal the artifact still answers as generation three
#   a rollback          generation two answers again, by name rather than by disappearance
#   a reset             the installed artifact answers, so the registry let go of everything
#   one boot            the guest is the same process group from the first step to the last
#
# Which generation answered has to be readable from the program's own output, not inferred from
# a generation counter the host printed. A test that only distinguished shadowed from installed
# would pass just as well if the second and third publications had quietly done nothing, and
# that is exactly the failure this is here to catch.
#
# It types at the terminal rather than launching through the agent, and that is not a style
# choice: a launch the development agent starts cannot be shadowed, because the agent is inside
# the launcher call at that moment and cannot answer the resolution query that same launch
# triggers. The shell's launch can be, so the shell is what runs the program.

import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import lab

FIXTURES = os.path.join(lab.REPO, '.build', 'scenario-fixtures')
TIMEOUT = 60
# The name the refused publication is offered under. It is a declared program, so the name
# itself is acceptable and the refusal can only come from the identity record naming another
# artifact - which is the check being tested rather than a name-validation error standing in
# for it.
WRONG_NAME = 'date'


def fail(message):
	print(f'dev-selftest: {message}', file=sys.stderr)
	sys.exit(1)


def step(message):
	print(f'dev-selftest: {message}')


def lab_command(*args):
	return subprocess.run([os.path.join(HERE, 'lab.py'), *args], cwd=lab.SRC, capture_output=True, text=True)


# Type `uname` at the guest terminal and return what it printed.
#
# Ctrl-C first, every time: a run that typed without a newline, or a program that left the line
# editor holding characters, would otherwise prefix the command and the answer would be about
# something else entirely. It costs nothing and it removes a whole class of confusing failure.
def ask_uname(guest, timeout=30):
	guest.type_text('\x03', False, timeout)
	guest.wait_prompt(5)
	mark = lab.serial_size()
	if not guest.type_text('uname', True, timeout):
		fail('the guest console refused the input')
	deadline = time.monotonic() + timeout
	while time.monotonic() < deadline:
		text = lab.serial_since(mark)
		# The echo of the command itself arrives first; the answer is whatever follows it.
		answer = text.split('uname', 1)[-1] if 'uname' in text else ''
		for marker in ('GENERATION1', 'GENERATION2', 'GENERATION3', 'LiberSystem'):
			if marker in answer:
				return marker
		time.sleep(0.2)
	fail(f'the guest never identified itself within {timeout} s; it printed {lab.serial_since(mark)!r}')


def expect(guest, marker, what):
	answered = ask_uname(guest)
	if answered != marker:
		fail(f'{what}: the guest answered {answered}, not {marker}')
	step(f'  {what}: {answered}')


def main():
	state, identity = lab.dev_state()
	if state != 'ready':
		fail(f'the development instance is {state}; this needs a running one, so start it with `just dev-up`')
	guest_at_start = identity.get('pgid')

	built = subprocess.run([os.path.join(HERE, 'scenarios', 'make-fixtures.py')], cwd=lab.SRC, capture_output=True, text=True)
	if built.returncode != 0:
		fail(f'the fixtures could not be built, so nothing below would mean anything\n{built.stdout}{built.stderr}')
	for index in (1, 2, 3):
		path = os.path.join(FIXTURES, f'uname-generation{index}')
		if not os.path.exists(path):
			fail(f'{path} is missing')

	guest = lab.LabGuest(TIMEOUT)

	step('three successive generations, published and asked to identify themselves')
	for index in (1, 2, 3):
		published = lab_command('dev-publish', 'uname', os.path.join(FIXTURES, f'uname-generation{index}'), '--timeout', str(TIMEOUT))
		if published.returncode != 0:
			fail(f'publishing generation {index} failed\n{published.stdout}{published.stderr}')
		expect(guest, f'GENERATION{index}', f'generation {index}')

	step('one refused generation: a valid image under a name its identity record does not claim')
	refused = lab_command('dev-publish', WRONG_NAME, os.path.join(FIXTURES, 'uname-generation3'), '--timeout', str(TIMEOUT))
	if refused.returncode == 0:
		fail(f'the guest accepted a uname image published as {WRONG_NAME}; manifest ownership is not being checked')
	step(f'  refused: {(refused.stderr or refused.stdout).strip().splitlines()[-1]}')
	expect(guest, 'GENERATION3', 'after the refusal the artifact is untouched')

	step('one rollback')
	rolled = lab_command('dev-rollback', 'uname')
	if rolled.returncode != 0:
		fail(f'rolling back failed\n{rolled.stdout}{rolled.stderr}')
	expect(guest, 'GENERATION2', 'after the rollback')

	step('reset, so the registry lets go of everything it held')
	reset = lab_command('dev-reset')
	if reset.returncode != 0:
		fail(f'the reset failed\n{reset.stdout}{reset.stderr}')
	expect(guest, 'LiberSystem', 'after the reset the installed artifact answers')

	_, identity_after = lab.dev_state()
	if identity_after.get('pgid') != guest_at_start:
		fail('the guest restarted during the run, so none of the above was the succession it claims to be')
	step(f'one boot throughout: process group {guest_at_start}')
	print('dev-selftest: passed')


if __name__ == '__main__':
	main()
