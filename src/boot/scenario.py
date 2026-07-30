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

# The emulator's key and button names come from `lab`, because they are QEMU's vocabulary
# rather than this format's, and one list is the only way the two cannot drift. The import is
# safe in both directions: `lab` reaches this module only from inside the command that runs a
# scenario, so neither is waiting on the other to finish loading.
import lab

# The functions below take the injected guest as a parameter named `lab`, which shadows the
# module inside them, so the timing events reach it under a second name.
import lab as lab_module

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
MAX_ARGS_BYTES = 512
MAX_KEYS = 64

# How many frames a scenario may leave the guest short of where it found it before the run is
# treated as not having given its scope back. Measured rather than chosen: on an otherwise idle
# instance the reading does not drift at all (six consecutive samples, identical to the frame),
# and a shell-basics run loses 256 to 259 frames on about two thirds of runs and nothing on the
# rest - a real per-run loss of about 1 MB that is recorded against this item and not yet fixed.
# The ceiling is four times that known cost, so it catches a run that loses memory by the tens of
# megabytes today while the 1 MB remains. It is the wrong number in the end: once the per-run loss
# is fixed this wants to be zero, because "the counters returned to baseline" is the property the
# item actually asks to verify, and any tolerance above zero is a tolerance for a leak.
MAX_FRAME_LOSS = 1024

# The closed vocabularies the input and restoration steps validate against. Key names and
# pointer buttons come from the runner's own tables so there is one list, not two that drift;
# what a terminal restores is named here because it is the scenario format's word for it.
#
# Each entry is the escape sequence that puts one thing back. A program that entered the
# alternate screen, hid the cursor, took raw input or turned on mouse reporting has to undo
# exactly these, and a scenario says which of them it expects to see.
RESTORED = {
	'screen': b'\x1b[?1049l',
	'cursor': b'\x1b[?25h',
	'raw': b'\x1b[?9001l',
	'mouse': (b'\x1b[?1000l', b'\x1b[?1002l', b'\x1b[?1003l'),
	'paste': b'\x1b[?2004l',
}

# A component name, not a path and not a command line. The guest checks the same thing; this
# is here so a scenario that meant to run a shell line is refused where it was written.
PROGRAM_NAME = re.compile(r'[A-Za-z0-9_-][A-Za-z0-9_.-]{0,47}')

ANSI = re.compile(rb'\x1b\[[0-9;?]*[ -/]*[@-~]')


class ScenarioError(Exception):
	pass


# One entry of RESTORED as the alternatives that satisfy it: mouse reporting has three forms
# and a program turns off the one it turned on.
def as_sequences(entry):
	return entry if isinstance(entry, tuple) else (entry,)


# The first name in `wanted` that does not appear after the ones before it, or None when all
# of them appear in that order. Each match resumes from where the previous one ended, so the
# question asked is the order they were written in and not merely whether each is somewhere.
def out_of_order(raw, wanted):
	at = 0
	for name in wanted:
		found = [raw.find(sequence, at) for sequence in as_sequences(RESTORED[name])]
		found = [position for position in found if position >= 0]
		if not found:
			return name
		at = min(found) + 1
	return None


# The keys a line of text is typed as. Validation has already established that every character
# maps, so this cannot fail here.
def lab_keys_for_text(text, enter):
	return lab.text_keys(text, enter)


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
	# Send key events through the emulated keyboard: `text` types a line character by character,
	# `keys` names them one at a time (`ctrl-c`, `up`, `f1`). Unlike `input`, which reaches the
	# console directly, this takes the path a person's keyboard takes - the device, its driver,
	# InputService and the session - so it is what exercises any of that.
	'key': {'required': (), 'optional': ('text', 'keys', 'enter', 'timeout')},
	# Send a pointer event through the emulated tablet. `x` and `y` are fractions of the screen,
	# absolute because the device is a tablet; `button` and `action` press, release or click.
	'pointer': {'required': (), 'optional': ('x', 'y', 'button', 'action', 'timeout')},
	# Assert the terminal was put back: each name in `expect` is one thing an interactive
	# program turns on and has to turn off again. Asserted against the raw console bytes,
	# because restoration is escape sequences and nothing else.
	'restored': {'required': ('expect',), 'optional': ('timeout',)},
	# Wait for the guest's terminal output to contain `contains`, or fail on the deadline.
	'expect': {'required': ('contains',), 'optional': ('timeout',)},
	# Wait for the shell prompt to come back.
	'prompt': {'required': (), 'optional': ('timeout',)},
	# Assert that the guest's terminal output since the previous step does NOT contain
	# `contains` - the check that a fix stopped producing something.
	'absent': {'required': ('contains',), 'optional': ('timeout',)},
	# Drop the guest's development state: the artifact registry and any open candidate.
	'reset': {'required': (), 'optional': ('timeout',)},
	# Replace the guest's development agent with a fresh one and wait until it serves. Larger
	# than `reset` in what it costs and identical in what a scenario sees afterwards, which is
	# the point of having both: what follows this step is asserting that a replacement is as
	# capable as the agent it replaced.
	'restart': {'required': (), 'optional': ('timeout',)},
	# Launch a canonical program through PermissionManager. `program` names it, `args` and
	# `cwd` are separate typed fields - never concatenated into anything an interpreter would
	# parse - and the component gets exactly its installed manifest's grants.
	'launch': {'required': ('program',), 'optional': ('args', 'cwd', 'timeout')},
	# Wait for the launched program's own output to contain `contains`.
	'output': {'required': ('contains',), 'optional': ('timeout',)},
	# Wait for the launched program to finish.
	'finished': {'required': (), 'optional': ('timeout',)},
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
	for field in ('artifact', 'file', 'text', 'contains', 'program', 'args', 'cwd'):
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
	# Key names, pointer buttons and the things a terminal restores are closed vocabularies,
	# checked here rather than at the emulator. This is the one place where what a scenario
	# says becomes what QEMU does, and a name passed through unchecked would be a way to run
	# monitor commands from scenario data.
	if kind == 'key':
		if ('text' in step) == ('keys' in step):
			raise ScenarioError(f'{where} (key): needs exactly one of text or keys')
		if 'keys' in step:
			if not isinstance(step['keys'], list) or not step['keys'] or len(step['keys']) > MAX_KEYS:
				raise ScenarioError(f'{where} (key): keys must be a list of 1..{MAX_KEYS} names')
			for name in step['keys']:
				if not isinstance(name, str) or lab.key_sequence(name) is None:
					raise ScenarioError(f'{where} (key): {name!r} is not a key this runner knows')
		elif lab.text_keys(step['text'], step.get('enter', True)) is None:
			raise ScenarioError(f'{where} (key): text has a character with no key mapping')
	if kind == 'pointer':
		for axis in ('x', 'y'):
			if axis in step and not (isinstance(step[axis], (int, float)) and not isinstance(step[axis], bool) and 0.0 <= step[axis] <= 1.0):
				raise ScenarioError(f'{where} (pointer): {axis} must be a fraction of the screen, 0.0 to 1.0')
		if 'button' in step and step['button'] not in lab.POINTER_BUTTONS:
			raise ScenarioError(f'{where} (pointer): {step["button"]!r} is not a pointer button, expected one of {sorted(lab.POINTER_BUTTONS)}')
		if 'action' in step and step['action'] not in ('press', 'release', 'click'):
			raise ScenarioError(f'{where} (pointer): action must be press, release or click')
		if 'x' not in step and 'y' not in step and 'button' not in step:
			raise ScenarioError(f'{where} (pointer): needs a position, a button, or both')
	if kind == 'restored':
		if not isinstance(step['expect'], list) or not step['expect']:
			raise ScenarioError(f'{where} (restored): expect must be a non-empty list')
		for name in step['expect']:
			if name not in RESTORED:
				raise ScenarioError(f'{where} (restored): {name!r} is not restorable state, expected one of {sorted(RESTORED)}')
	if 'args' in step and len(step['args'].encode()) > MAX_ARGS_BYTES:
		raise ScenarioError(f'{where} ({kind}): args is {len(step["args"].encode())} B, at most {MAX_ARGS_BYTES}')
	if kind == 'launch' and not PROGRAM_NAME.fullmatch(step['program']):
		raise ScenarioError(f'{where} (launch): program {step["program"]!r} is not a plain component name')


# The guest, as the runner sees it: terminal input and artifact publication over the control
# protocol, terminal output from the broker's serial log. Injected rather than imported so a
# scenario can be validated, and the runner tested, without a running guest.
class Guest:
	def __init__(self, lab):
		self.lab = lab
		self.at = lab.serial_size()
		# What the launched program has printed so far, accumulated across `output` steps so a
		# later assertion can match something an earlier read already consumed.
		self.launched = ''

	# Everything the guest has printed since the last time this was called.
	def output(self):
		text = self.lab.serial_since(self.at)
		self.at = self.lab.serial_size()
		return text

	# Everything printed since `mark`, without consuming it.
	def output_since(self, mark):
		return self.lab.serial_since(mark)

	# The same, with the escape sequences left in, which is the only way to see a terminal
	# being put back.
	def raw_since(self, mark):
		return self.lab.serial_raw_since(mark)

	def mark(self):
		return self.lab.serial_size()


def run(document, lab, verbose=False):
	lab_module.timing_event('scenario', f"start:{document.get('name', 'unnamed')}")
	guest = Guest(lab)
	baseline = lab.memory_stats(10)
	total = document.get('timeout', 300)
	deadline = time.monotonic() + total
	started = time.monotonic()
	try:
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
	except ScenarioError as failure:
		# The scenario's own result is what the caller is waiting to hear, so a teardown that
		# also went wrong is appended to it rather than replacing it.
		left = teardown(lab, verbose, baseline)
		raise ScenarioError(f'{failure}; and the scope was not restored: {"; ".join(left)}' if left else str(failure)) from None
	left = teardown(lab, verbose, baseline)
	if left:
		# A run that passed but left the instance dirty is not a pass. The instance is shared by
		# every scenario after it, and a scope that could not be given back is the failure this
		# whole item exists to prevent - reported here, where it is known, rather than by
		# whichever innocent run trips over it next.
		raise ScenarioError(f'the scenario passed but its scope was not restored: {"; ".join(left)}')
	return time.monotonic() - started


# Put the instance back the way the next run needs to find it, whether this one passed, failed
# or ran out of time. A failed run is the case that matters: it is the one that stops halfway,
# and everything it was holding when it stopped is still held.
#
# The instance is persistent and shared by every scenario, so a run that leaves a program in
# the foreground leaves the next run looking at that program's screen instead of a prompt -
# which reads as an instance that never became ready, so the failure lands on the innocent run
# after the guilty one. Three things are given back, in the order they can be: the launched
# program, the terminal, and the registry.
#
# The scope is then verified rather than assumed. Performing a teardown and reporting success
# because the steps were attempted is how a run comes to leave state behind quietly; the guest
# is asked afterwards what it is actually holding. Returns the list of things that could not be
# given back, empty when the instance is as the next run needs to find it.
def teardown(lab, verbose=False, baseline=None):
	lab_module.timing_event('scenario', 'steps-end')
	lab_module.timing_event('scenario', 'cleanup-start')
	notes = []
	left = []
	try:
		if lab.stop_launch(10):
			notes.append('stopped the launched program')
		# Give the terminal back, escalating only as far as it takes. Ctrl+C first, which drops a
		# half-typed line at a prompt and is what a cooked terminal turns into a signal. A raw
		# terminal does not: a program that asked for raw input is handed the byte, so Ctrl+C
		# means nothing to it and the next two are what the tree's interactive tools answer to.
		# Nothing here is sent unless the one before it left no prompt, so a run that ended
		# tidily pays for one keystroke and one check.
		for key in ('\x03', '\x1b', 'q'):
			lab.type_text(key, False, 10)
			if lab.wait_prompt(5):
				break
		else:
			left.append('the terminal is not at a prompt')
		if not lab.reset(10):
			left.append('the development state was not dropped')
		# What the guest says it is holding, which is the only account of it worth having. A
		# generation still in the registry or a launch still in flight would both be invisible
		# from here otherwise, and both would be inherited by the next run.
		held = lab.scope_held(10)
		if held is None:
			left.append('the guest did not answer when asked what it still holds')
		else:
			left.extend(held)
	# SystemExit included on purpose: the helpers below exit the process when the instance is
	# unreachable, and during teardown that is something to report, not something to die of.
	except (OSError, ValueError, SystemExit) as error:
		left.append(f'teardown did not complete: {error}')
	lost = frames_lost(lab, baseline)
	if lost is not None:
		notes.append(f'the guest is {lost} frame(s) short of where the scenario found it')
		if lost > MAX_FRAME_LOSS:
			left.append(f'the guest lost {lost} frames ({lost * 4} kB), past the {MAX_FRAME_LOSS} the known per-run cost accounts for')
	if verbose:
		for note in notes:
			print(f'     {note}')
	lab_module.timing_event('scenario', 'cleanup-end')
	return left


# How much system memory the run did not give back, in frames, or None when either reading is
# unavailable - an instance older than the opcode, or one that stopped answering. Never negative:
# a run that ended with more free memory than it started with has nothing to answer for.
def frames_lost(lab, baseline):
	if baseline is None:
		return None
	after = lab.memory_stats(10)
	if after is None:
		return None
	return max(0, baseline[0] - after[0])


def run_step(step, guest, lab, limit, index):
	kind = step['do']
	where = f'step {index + 1} ({kind})'
	if kind == 'publish':
		if not lab.publish(step['artifact'], step['file'], int(limit)):
			raise ScenarioError(f'{where}: publishing {step["artifact"]} failed')
	elif kind == 'input':
		if not lab.type_text(step['text'], step.get('enter', True), int(limit)):
			raise ScenarioError(f'{where}: the guest console refused the input')
	elif kind == 'key':
		keys = step['keys'] if 'keys' in step else lab_keys_for_text(step['text'], step.get('enter', True))
		if not lab.send_keys(keys):
			raise ScenarioError(f'{where}: the emulated keyboard refused the events')
	elif kind == 'pointer':
		if not lab.send_pointer(step.get('x'), step.get('y'), step.get('button'), step.get('action', 'click')):
			raise ScenarioError(f'{where}: the emulated tablet refused the event')
	elif kind == 'restored':
		# In the order named, not merely all present. Order is the property that matters: a
		# terminal handed back with the alternate screen left before the cursor was shown, or
		# raw input still on while the screen switches, is a terminal a person is looking at
		# in a wrong state, however briefly. It is also what the kernel-side harness proves,
		# so a scenario asserting only presence would be the weaker of the two tests.
		#
		# Watched rather than sampled once: the program is exiting while this runs and writes
		# its restore sequences one at a time.
		mark = guest.at
		end = time.monotonic() + limit
		while True:
			missing = out_of_order(guest.raw_since(mark), step['expect'])
			if missing is None:
				break
			if time.monotonic() >= end:
				raise ScenarioError(f'{where}: the terminal did not restore {missing} in order within {int(limit)} s')
			time.sleep(0.2)
		guest.at = guest.mark()
	elif kind == 'reset':
		if not lab.reset(int(limit)):
			raise ScenarioError(f'{where}: reset failed')
	elif kind == 'restart':
		if not lab.restart(int(limit)):
			raise ScenarioError(f'{where}: the development agent did not restart')
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
	elif kind == 'launch':
		koid = lab.launch(step['program'], step.get('args', ''), step.get('cwd', 'vol://system'), int(limit))
		if koid is None:
			raise ScenarioError(f'{where}: the launcher refused {step["program"]}')
		guest.launched = ''
	elif kind == 'output':
		end = time.monotonic() + limit
		while time.monotonic() < end:
			text, exited = lab.launch_output(int(limit))
			if text is None:
				raise ScenarioError(f'{where}: nothing has been launched')
			guest.launched += text
			if step['contains'] in guest.launched:
				return
			if exited:
				raise ScenarioError(f'{where}: {step["contains"]!r} never appeared and the program finished')
			time.sleep(0.1)
		raise ScenarioError(f'{where}: {step["contains"]!r} did not appear within {int(limit)} s')
	elif kind == 'finished':
		end = time.monotonic() + limit
		while time.monotonic() < end:
			text, exited = lab.launch_output(int(limit))
			if text is None:
				raise ScenarioError(f'{where}: nothing has been launched')
			guest.launched += text
			if exited:
				return
			time.sleep(0.1)
		raise ScenarioError(f'{where}: the program had not finished within {int(limit)} s')
	elif kind == 'absent':
		mark = guest.at
		time.sleep(min(limit, 2))
		if step['contains'] in guest.output_since(mark):
			raise ScenarioError(f'{where}: {step["contains"]!r} appeared and should not have')
		guest.at = guest.mark()


def strip_ansi(data):
	return ANSI.sub(b'', data)
