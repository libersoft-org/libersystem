#!/usr/bin/env python3
# The scenario format and its host runner.
#
# Application-level interaction tests live here as versioned data rather than as Rust
# compiled into the kernel test binary. That is the whole point: a scenario that is data
# costs nothing to add or fix - no kernel relink, no package reassembly, no boot image, no
# QEMU restart - while a scenario that is code costs all four on every character changed.
#
# WHERE THE INTERPRETER LIVES. It lives here, on the host, and nothing new is staged into the
# guest at all. The runner drives an already-running development instance over capabilities
# that exist: the control protocol for input and artifact publication, and the broker's
# serial stream for output. So the question the roadmap asks - whether the interpreter is
# hot-publishable or deliberately stable with its cost documented - has a third answer for
# this slice: it is not in the guest, so changing it costs nothing either. When steps arrive
# that must be decided inside the guest, that part becomes a guest-side interpreter and the
# format stays exactly as it is, which is why the two are separate.
#
# NO SHELL, ANYWHERE. There is no step that hands a string to a shell, host or guest. The
# runner never invokes a host command interpreter, and a scenario cannot ask it to. The steps
# are typed events with typed fields, and an unknown step type is refused before the run
# starts rather than being passed through to something that might understand it.
#
# BOUNDED BY CONSTRUCTION. Every scenario is validated in full before its first step runs:
# the version, the step count, every field's type, every payload's size and every deadline.
# A scenario that would exceed a bound is refused as a whole, so a run never stops halfway
# through because of something that was knowable before it started.

import os
import re
import sys
import time
import tomllib

# The format revision this runner understands. A scenario states it, so a file written for a
# later format is refused rather than half-understood.
SCENARIO_VERSION = 1

# The bounds. They are here rather than left to whatever the runner tolerates, so a scenario
# that would run away is refused by a number that can be read and argued with.
MAX_STEPS = 200
MAX_INPUT_BYTES = 4096
MAX_PATTERN_BYTES = 512
MAX_STEP_SECONDS = 300
MAX_TOTAL_SECONDS = 1800

ANSI = re.compile(rb'\x1b\[[0-9;?]*[ -/]*[@-~]')


class ScenarioError(Exception):
	pass


# One step type: the fields it requires, the fields it allows, and how it runs. Keeping the
# table declarative is what makes validation total - a step type that is not here cannot be
# run, and a field that is not here cannot be silently ignored.
STEP_FIELDS = {
	# Publish a fixture artifact into the guest's volatile registry. `artifact` names a
	# manifest-declared name; `file` is a host path the runner reads, which never crosses the
	# wire - only the bytes do.
	'publish': {'required': ('artifact', 'file'), 'optional': ('timeout',)},
	# Send terminal input. `text` is literal; `enter` appends a carriage return.
	'input': {'required': ('text',), 'optional': ('enter', 'timeout')},
	# Wait for the guest's terminal output to contain `contains`, or fail on the deadline.
	'expect': {'required': ('contains',), 'optional': ('timeout',)},
	# Wait for the shell prompt to come back.
	'prompt': {'required': (), 'optional': ('timeout',)},
	# Assert that the guest's terminal output since the previous step does NOT contain
	# `contains` - the check that a fix stopped producing something.
	'absent': {'required': ('contains',), 'optional': ('timeout',)},
	# Drop the guest's development state: the artifact registry and any open candidate.
	'reset': {'required': (), 'optional': ('timeout',)},
}


def load(path):
	try:
		with open(path, 'rb') as handle:
			document = tomllib.load(handle)
	except OSError as error:
		raise ScenarioError(f'cannot read {path}: {error}') from error
	except tomllib.TOMLDecodeError as error:
		raise ScenarioError(f'{path} is not valid TOML: {error}') from error
	return validate(document, path)


# Check everything before anything runs. Every failure names the file, the step index and the
# field, because a scenario is data someone is editing and a rejection has to say where.
def validate(document, path):
	if not isinstance(document, dict):
		raise ScenarioError(f'{path}: not a table')
	unknown = set(document) - {'version', 'name', 'description', 'timeout', 'step'}
	if unknown:
		raise ScenarioError(f'{path}: unknown keys {sorted(unknown)}')
	version = document.get('version')
	if version != SCENARIO_VERSION:
		raise ScenarioError(f'{path}: version {version!r}, this runner understands {SCENARIO_VERSION}')
	name = document.get('name')
	if not isinstance(name, str) or not name:
		raise ScenarioError(f'{path}: name must be a non-empty string')
	total = document.get('timeout', 300)
	if not isinstance(total, int) or not 1 <= total <= MAX_TOTAL_SECONDS:
		raise ScenarioError(f'{path}: timeout must be 1..{MAX_TOTAL_SECONDS} seconds')
	steps = document.get('step', [])
	if not isinstance(steps, list) or not steps:
		raise ScenarioError(f'{path}: needs at least one [[step]]')
	if len(steps) > MAX_STEPS:
		raise ScenarioError(f'{path}: {len(steps)} steps, at most {MAX_STEPS}')
	for index, step in enumerate(steps):
		validate_step(step, index, path)
	return document


def validate_step(step, index, path):
	where = f'{path}: step {index + 1}'
	if not isinstance(step, dict):
		raise ScenarioError(f'{where}: not a table')
	kind = step.get('do')
	if kind not in STEP_FIELDS:
		raise ScenarioError(f'{where}: unknown step {kind!r}, expected one of {sorted(STEP_FIELDS)}')
	shape = STEP_FIELDS[kind]
	allowed = {'do', *shape['required'], *shape['optional']}
	unknown = set(step) - allowed
	if unknown:
		raise ScenarioError(f'{where} ({kind}): unknown fields {sorted(unknown)}')
	for field in shape['required']:
		if field not in step:
			raise ScenarioError(f'{where} ({kind}): missing {field}')
	timeout = step.get('timeout', 30)
	if not isinstance(timeout, int) or not 1 <= timeout <= MAX_STEP_SECONDS:
		raise ScenarioError(f'{where} ({kind}): timeout must be 1..{MAX_STEP_SECONDS} seconds')
	for field in ('artifact', 'file', 'text', 'contains'):
		if field in step and not isinstance(step[field], str):
			raise ScenarioError(f'{where} ({kind}): {field} must be a string')
	if 'enter' in step and not isinstance(step['enter'], bool):
		raise ScenarioError(f'{where} ({kind}): enter must be true or false')
	if 'text' in step and len(step['text'].encode()) > MAX_INPUT_BYTES:
		raise ScenarioError(f'{where} ({kind}): text is {len(step["text"].encode())} B, at most {MAX_INPUT_BYTES}')
	if 'contains' in step:
		if not step['contains']:
			raise ScenarioError(f'{where} ({kind}): contains must not be empty')
		if len(step['contains'].encode()) > MAX_PATTERN_BYTES:
			raise ScenarioError(f'{where} ({kind}): contains is {len(step["contains"].encode())} B, at most {MAX_PATTERN_BYTES}')
	if kind == 'publish' and not os.path.isfile(step['file']):
		raise ScenarioError(f'{where} (publish): no such file {step["file"]}')


# The guest, as the runner sees it: terminal input and artifact publication over the control
# protocol, terminal output from the broker's serial log. Injected rather than imported so a
# scenario can be validated, and the runner tested, without a running guest.
class Guest:
	def __init__(self, lab):
		self.lab = lab
		self.at = lab.serial_size()

	# Everything the guest has printed since the last time this was called.
	def output(self):
		text = self.lab.serial_since(self.at)
		self.at = self.lab.serial_size()
		return text

	# Everything printed since `mark`, without consuming it.
	def output_since(self, mark):
		return self.lab.serial_since(mark)

	def mark(self):
		return self.lab.serial_size()


def run(document, lab, verbose=False):
	guest = Guest(lab)
	total = document.get('timeout', 300)
	deadline = time.monotonic() + total
	started = time.monotonic()
	for index, step in enumerate(document['step']):
		if time.monotonic() >= deadline:
			raise ScenarioError(f'step {index + 1}: the scenario ran past its {total} s total deadline')
		remaining = deadline - time.monotonic()
		limit = min(step.get('timeout', 30), remaining)
		label = f'{index + 1}/{len(document["step"])} {step["do"]}'
		at = time.monotonic()
		run_step(step, guest, lab, limit, index)
		if verbose:
			print(f'     {label} in {(time.monotonic() - at) * 1000:.0f} ms')
	return time.monotonic() - started


def run_step(step, guest, lab, limit, index):
	kind = step['do']
	where = f'step {index + 1} ({kind})'
	if kind == 'publish':
		if not lab.publish(step['artifact'], step['file'], int(limit)):
			raise ScenarioError(f'{where}: publishing {step["artifact"]} failed')
	elif kind == 'input':
		if not lab.type_text(step['text'], step.get('enter', True), int(limit)):
			raise ScenarioError(f'{where}: the guest console refused the input')
	elif kind == 'reset':
		if not lab.reset(int(limit)):
			raise ScenarioError(f'{where}: reset failed')
	elif kind == 'prompt':
		if not lab.wait_prompt(int(limit)):
			raise ScenarioError(f'{where}: no shell prompt within {int(limit)} s')
	elif kind == 'expect':
		wanted = step['contains']
		end = time.monotonic() + limit
		while time.monotonic() < end:
			if wanted in guest.output_since(guest.at):
				guest.at = guest.mark()
				return
			time.sleep(0.2)
		raise ScenarioError(f'{where}: {wanted!r} did not appear within {int(limit)} s')
	elif kind == 'absent':
		mark = guest.at
		time.sleep(min(limit, 2))
		if step['contains'] in guest.output_since(mark):
			raise ScenarioError(f'{where}: {step["contains"]!r} appeared and should not have')
		guest.at = guest.mark()


def strip_ansi(data):
	return ANSI.sub(b'', data)
