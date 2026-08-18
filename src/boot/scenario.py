# The scenario format and its host runner.
#
# A MODULE, not a command: `lab.py dev-test` and `lab.py scenario-cold` import it and nothing runs
# it directly. It carried a `#!/usr/bin/env python3` and mode 0644, which is the pair that made
# `perf-trace.py`'s documented entry point fail with `Permission denied` on a clean checkout. The
# rule `check-source-hygiene.sh` enforces is that a shebang means "run me"; this file does not mean
# that, so the shebang is gone rather than the mode changed.
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

import hashlib
import json
import os
import re
import struct
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
# treated as not having given its scope back. Zero, because the property being verified is that
# the counters returned to baseline and any tolerance above zero is a tolerance for a leak.
#
# It was 1024 for as long as one existed. A scenario used to lose about a megabyte per run - the
# runtime heap grew on every program launch and never gave a byte back - and that made a strict
# check impossible to arm, so the ceiling stood at four times the known loss just to catch
# something worse. With the heap coalescing its free list the loss is exactly zero across
# repeated launches, measured over twenty of them, so the ceiling can say what it means.
MAX_FRAME_LOSS = 0

# Which scenarios have already run in this instance, one name per line.
SEEN_PATH = os.path.join(lab.BUILD, 'dev-scenarios-seen')

# WHICH TARGET THE HOST-SIDE ARTIFACTS COME FROM.
#
# A scenario names host files it publishes into the guest, and those files are architecture-specific:
# the guest verifies an image's target and correctly refuses one built for another. The TOMLs used to
# spell out `.build/image/x86_64-unknown-none/...` and `.build/fixtures/...`, so the cold runner's
# promise of the same scenarios on all three targets could not hold - an aarch64 cold run sent x86
# images to a guest that rejects them. The documents name `{staged}` and `{fixtures}` instead and the
# runner resolves them for whichever target it is driving.
TARGET = 'x86_64'
TRIPLES = {'x86_64': 'x86_64-unknown-none', 'aarch64': 'aarch64-unknown-none', 'riscv64': 'riscv64gc-unknown-none-elf'}


def fixtures_dir(target=None):
	return os.path.join(lab.BUILD_ROOT, 'fixtures', target or TARGET)


def staged_dir(target=None):
	return os.path.join(lab.BUILD_ROOT, 'image', TRIPLES[target or TARGET])


# Resolve a document's host path. Absolute after this, so nothing depends on the working directory
# the runner happened to be started from either.
def resolve_path(path, target=None):
	return path.format(staged=staged_dir(target), fixtures=fixtures_dir(target))


# Refuse a fixture set that was not built from this tree, BEFORE anything is published into a guest.
#
# `dev-test` used to check only that the files existed. A fixture whose staged source had since been
# rebuilt stayed on disk indefinitely and was published as though it were current, so a scenario's
# pass or failure could be about bytes from an older tree. The manifest `make-fixtures.py` writes
# names the target it was built for, the staged sources it read and each fixture's digest; all three
# are checked here. Returns a list of complaints, empty when the set is current.
def stale_fixtures(target=None):
	target = target or TARGET
	manifest_path = os.path.join(fixtures_dir(target), 'fixtures.json')
	try:
		with open(manifest_path, encoding='utf-8') as handle:
			record = json.load(handle)
	except (OSError, ValueError):
		return [f'no usable fixture manifest at {manifest_path} - run boot/scenarios/make-fixtures.py']
	if record.get('target') != target:
		return [f'{manifest_path} was built for {record.get("target")!r}, not {target!r}']
	complaints = []
	for name, digest in sorted((record.get('fixtures') or {}).items()):
		path = os.path.join(fixtures_dir(target), name)
		if file_digest(path) != digest:
			complaints.append(f'the fixture {name} does not match the set it was published with')
	for relative, digest in sorted((record.get('sources') or {}).items()):
		if file_digest(os.path.join(staged_dir(target), relative)) != digest:
			complaints.append(f'{relative} has been rebuilt since the fixtures were made from it')
	return complaints


def file_digest(path):
	try:
		with open(path, 'rb') as handle:
			digest = hashlib.sha256()
			for chunk in iter(lambda: handle.read(1 << 20), b''):
				digest.update(chunk)
			return digest.hexdigest()
	except OSError:
		return None

# How much longer every deadline in a scenario may take, set by the runner for a guest that is
# emulated rather than native. A scenario states its deadlines once, for one machine, and they
# are what bounds it; a target that runs ten times slower does not make those numbers wrong, it
# makes them the wrong unit. Scaling them keeps the scenario honest about what it waits for and
# keeps the bound, because a scaled deadline is still a deadline.
TIME_SCALE = 1.0

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

# A fixture's bare name, checked here as well as in the guest so a scenario that meant to write
# a path is refused where it was written rather than by a status code at run time.
FIXTURE_NAME = re.compile(r'[A-Za-z0-9_-][A-Za-z0-9_.-]{0,47}')

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
	# Put a data file where the scenario's tools can read it. `name` is a bare name, never a
	# path - the guest joins it to its own reserved prefix - and `file` is the host file whose
	# bytes are sent. The teardown removes every fixture a run wrote, so a scenario states what
	# it needs and never has to clean up after itself.
	'fixture': {'required': ('name', 'file'), 'optional': ('timeout',)},
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
	#
	# `until` names the boundary the assertion holds to. `quiet` (the default) watches the whole
	# declared timeout; `prompt` watches until the shell prompt comes back, which is the right
	# boundary for "this command printed no such thing" and finishes as soon as the command does.
	# Either way the forbidden text is checked CONTINUOUSLY, so it fails at the moment it appears
	# rather than at whatever instant a sample happens to land on.
	'absent': {'required': ('contains',), 'optional': ('timeout', 'until')},
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
	if kind in ('publish', 'fixture'):
		try:
			resolved = resolve_path(step['file'])
		except (KeyError, IndexError, ValueError):
			raise ScenarioError(f'{where} ({kind}): {step["file"]!r} names a placeholder this runner does not have; use {{staged}} or {{fixtures}}') from None
		if not os.path.isfile(resolved):
			raise ScenarioError(f'{where} ({kind}): no such file {resolved}')
	if kind == 'fixture':
		if not FIXTURE_NAME.fullmatch(step['name']):
			raise ScenarioError(f'{where} (fixture): {step["name"]!r} is not a bare fixture name')
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
	if kind == 'absent' and 'until' in step and step['until'] not in ('quiet', 'prompt'):
		raise ScenarioError(f'{where} (absent): until must be quiet or prompt')
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

	# Everything the guest has printed since the last time this was called, AND the offset those
	# bytes end at.
	#
	# Every cursor move in this file goes through a read that answers both. It used to read from the
	# old cursor and then ask separately how big the file is, and the broker appends the whole time:
	# bytes arriving in between were stepped over without any oracle seeing them. The rule is that
	# the cursor only ever advances to the end of bytes that were actually returned.
	def output(self):
		text, end = self.read_since(self.at)
		self.at = end
		return text

	# Everything printed since `mark`, without consuming it - the text and the offset it ends at.
	def read_since(self, mark):
		raw, end = self.lab.serial_read(mark)
		return lab_module.strip_ansi(raw).decode(errors='replace'), end

	def output_since(self, mark):
		return self.read_since(mark)[0]

	# The same, with the escape sequences left in, which is the only way to see a terminal
	# being put back.
	def raw_read_since(self, mark):
		return self.lab.serial_read(mark)

	def raw_since(self, mark):
		return self.raw_read_since(mark)[0]

	def mark(self):
		return self.lab.serial_size()


def run(document, lab, verbose=False):
	lab_module.timing_event('scenario', f"start:{document.get('name', 'unnamed')}")
	guest = Guest(lab)
	baseline = lab.memory_stats(10)
	first_run = note_run(document.get('name', 'unnamed'))
	total = document.get('timeout', 300) * TIME_SCALE
	deadline = time.monotonic() + total
	started = time.monotonic()
	# PRIMARY RESULT, THEN TEARDOWN, ALWAYS.
	#
	# This used to call `teardown` from a `except ScenarioError` handler and again after a completely
	# normal loop, which covers exactly two of the ways out. The step adapters reach the protocol
	# helpers directly and those report connection, handshake, timeout and refusal failures with
	# `die`, which raises `SystemExit` - the protocol layer's NORMAL error path, not an exotic
	# programmer exception. A control socket that disappeared, a request that timed out, a malformed
	# reply reaching an unpack, or a Ctrl-C during a step all left through neither handler, so
	# teardown never ran: the launched program kept running, the terminal stayed in raw or alternate
	# screen, generations kept shadowing installed files, and the fixtures stayed. The next scenario
	# then inherited all of it and failed - or passed - for the wrong reason.
	#
	# So: capture whatever ended the run, tear down unconditionally, then decide. `KeyboardInterrupt`
	# and `SystemExit` are re-raised after the cleanup rather than converted, because a person
	# interrupting a run means to stop, not to see it recorded as a step failure.
	failure = None
	interrupted = None
	try:
		for index, step in enumerate(document['step']):
			if time.monotonic() >= deadline:
				raise ScenarioError(f'step {index + 1}: the scenario ran past its {total} s total deadline')
			remaining = deadline - time.monotonic()
			limit = min(step.get('timeout', 30) * TIME_SCALE, remaining)
			label = f'{index + 1}/{len(document["step"])} {step["do"]}'
			at = time.monotonic()
			try:
				run_step(step, guest, lab, limit, index)
			except ScenarioError:
				raise
			except SystemExit as error:
				# The protocol layer's way of reporting a refusal or a lost connection. It names the
				# step it happened in from here; as a bare SystemExit it named nothing and took the
				# whole runner with it.
				raise ScenarioError(f'step {index + 1} ({step["do"]}): the control protocol gave up: {error}') from None
			except (OSError, struct.error, ValueError) as error:
				raise ScenarioError(f'step {index + 1} ({step["do"]}): {type(error).__name__}: {error}') from None
			if verbose:
				print(f'     {label} in {(time.monotonic() - at) * 1000:.0f} ms')
	except ScenarioError as error:
		failure = error
	except BaseException as error:  # noqa: BLE001 - re-raised below, after the guest is given back
		interrupted = error
	# Exactly once, whatever happened above, and it must not be able to replace the primary result.
	try:
		left = teardown(lab, verbose, baseline, first_run)
	except BaseException as error:  # noqa: BLE001 - a broken teardown is a finding, not an exit
		left = [f'teardown raised {type(error).__name__}: {error}']
	if interrupted is not None:
		raise interrupted
	if failure is not None:
		# The scenario's own result is what the caller is waiting to hear, so a teardown that
		# also went wrong is appended to it rather than replacing it.
		raise ScenarioError(f'{failure}; and the scope was not restored: {"; ".join(left)}' if left else str(failure)) from None
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
def teardown(lab, verbose=False, baseline=None, first_run=True):
	lab_module.timing_event('scenario', 'steps-end')
	lab_module.timing_event('scenario', 'cleanup-start')
	notes = []
	left = []
	# ASSIGNED BEFORE THE `try`, all three. `free` was not, and it is read after the handler below:
	# any failure before the `teardown_state` call - a `stop_launch` that raised, terminal recovery,
	# the reset - was caught as intended and then the next line read an uninitialized local. Python
	# raises `UnboundLocalError` there, which replaced both the scenario's own result and the
	# teardown diagnosis that had just been collected, and took the remaining scenarios with it. The
	# traceback named a harness variable rather than the guest failure that started it.
	held, stuck, free = None, None, None
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
		# One question of the guest, answered in one exchange: remove the fixtures this run
		# wrote, say what is still held, and report the memory account. Held is the only account
		# worth having - a generation still in the registry or a launch still in flight would
		# both be invisible from here - and the fixtures matter for the same reason, since the
		# instance is shared and whatever is left is read by whatever runs next.
		held, stuck, free = lab.teardown_state(10)
		if stuck is None:
			left.append('the guest did not answer when asked to clear the fixtures')
		elif stuck:
			left.append(f'{stuck} fixture(s) could not be removed')
		if held is None:
			left.append('the guest did not answer when asked what it still holds')
		else:
			left.extend(held)
	# SystemExit included on purpose: the helpers below exit the process when the instance is
	# unreachable, and during teardown that is something to report, not something to die of.
	except (OSError, ValueError, SystemExit) as error:
		left.append(f'teardown did not complete: {error}')
	lost = None if baseline is None or free is None else max(0, baseline[0] - free)
	if lost is not None:
		notes.append(f'the guest is {lost} frame(s) short of where the scenario found it')
		# Strict from the second run onward. The first run of a scenario in an instance touches
		# things nothing has touched yet - a program launched for the first time grows its heap
		# to a working size and keeps it - and that residency is memory in use, not memory lost.
		# What no legitimate cost explains is the same scenario costing again what it already
		# paid, so that is what fails.
		if lost > MAX_FRAME_LOSS and first_run is False:
			left.append(f'the guest lost {lost} frames ({lost * 4} kB) on a repeat run, where a scenario that gives its scope back costs nothing')
	if verbose:
		for note in notes:
			print(f'     {note}')
	lab_module.timing_event('scenario', 'cleanup-end')
	return left


# Record that this scenario has run in this instance, and say whether it had run before. The
# record lives beside the instance and `dev-up` starts it empty, so "before" means "since this
# guest booted" - which is what first-touch residency is relative to.
def note_run(name):
	seen = set()
	# One scenario name per line; a missing file means nothing has run yet.
	try:
		with open(SEEN_PATH, encoding='utf-8') as handle:
			seen = {line.strip() for line in handle if line.strip()}
	except OSError:
		pass
	first = name not in seen
	if first:
		try:
			with open(SEEN_PATH, 'a', encoding='utf-8') as handle:
				handle.write(name + '\n')
		except OSError:
			pass
	return first



def run_step(step, guest, lab, limit, index):
	kind = step['do']
	where = f'step {index + 1} ({kind})'
	if kind == 'publish':
		if not lab.publish(step['artifact'], resolve_path(step['file']), int(limit)):
			raise ScenarioError(f'{where}: publishing {step["artifact"]} failed')
	elif kind == 'fixture':
		if not lab.fixture_put(step['name'], resolve_path(step['file']), int(limit)):
			raise ScenarioError(f'{where}: writing the fixture {step["name"]} failed')
	elif kind == 'input':
		if not lab.type_text(step['text'], step.get('enter', True), int(limit)):
			raise ScenarioError(f'{where}: the guest console refused the input')
	elif kind == 'key':
		keys = step['keys'] if 'keys' in step else lab_keys_for_text(step['text'], step.get('enter', True))
		# The step's budget reaches the device. A key batch is up to 64 separate monitor operations
		# and this step declared a deadline none of them had ever been told about.
		if not lab.send_keys(keys, limit):
			raise ScenarioError(f'{where}: the emulated keyboard refused the events')
	elif kind == 'pointer':
		if not lab.send_pointer(step.get('x'), step.get('y'), step.get('button'), step.get('action', 'click'), limit):
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
			raw, at = guest.raw_read_since(mark)
			missing = out_of_order(raw, step['expect'])
			if missing is None:
				break
			if time.monotonic() >= end:
				raise ScenarioError(f'{where}: the terminal did not restore {missing} in order within {int(limit)} s')
			time.sleep(0.2)
		guest.at = at
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
		while True:
			text, at = guest.read_since(guest.at)
			if wanted in text:
				guest.at = at
				return
			if time.monotonic() >= end:
				break
			time.sleep(0.2)
		raise ScenarioError(f'{where}: {wanted!r} did not appear within {int(limit)} s')
	elif kind == 'launch':
		koid = lab.launch(step['program'], step.get('args', ''), step.get('cwd', 'vol://system'), int(limit))
		if koid is None:
			raise ScenarioError(f'{where}: the launcher refused {step["program"]}')
		guest.launched = ''
	elif kind == 'output':
		# EACH POLL GETS WHAT IS LEFT, not the whole step budget again. Every iteration handed
		# `launch_output` the original limit, so one late call could spend another full step's worth
		# past the deadline this loop had just computed - and the scenario's total deadline with it.
		end = time.monotonic() + limit
		while True:
			text, exited = lab.launch_output(max(1, int(end - time.monotonic())))
			if text is None:
				raise ScenarioError(f'{where}: nothing has been launched')
			guest.launched += text
			if step['contains'] in guest.launched:
				return
			if exited:
				raise ScenarioError(f'{where}: {step["contains"]!r} never appeared and the program finished')
			if time.monotonic() >= end:
				break
			time.sleep(0.1)
		raise ScenarioError(f'{where}: {step["contains"]!r} did not appear within {int(limit)} s')
	elif kind == 'finished':
		end = time.monotonic() + limit
		while True:
			text, exited = lab.launch_output(max(1, int(end - time.monotonic())))
			if text is None:
				raise ScenarioError(f'{where}: nothing has been launched')
			guest.launched += text
			if exited:
				return
			if time.monotonic() >= end:
				break
			time.sleep(0.1)
		raise ScenarioError(f'{where}: the program had not finished within {int(limit)} s')
	elif kind == 'absent':
		# WATCHED FOR THE WHOLE DECLARED INTERVAL, not sampled once two seconds in.
		#
		# This used to `sleep(min(limit, 2))` and then look. Its only caller declares ten seconds,
		# so eight of them were not observed at all: text emitted after the sample passed the
		# assertion, and the cursor was then advanced past it so the next step could not see it
		# either. A negative assertion whose subject is allowed to happen unobserved is a test that
		# reports success without measuring anything - and this one guards a refusal, so what it
		# waves through is an incompatible provider being launched.
		#
		# Two boundaries, because negative assertions have two honest shapes: watch a fixed quiet
		# period, or watch until the thing that was supposed to produce nothing has finished. The
		# cursor advances only over bytes this actually read, so nothing is hidden from the step
		# after it either.
		mark = guest.at
		until = step.get('until', 'quiet')
		end = time.monotonic() + limit
		while True:
			if step['contains'] in guest.output_since(mark):
				raise ScenarioError(f'{where}: {step["contains"]!r} appeared and should not have')
			if until == 'prompt' and lab_module.has_prompt(guest.raw_since(mark)):
				break
			if time.monotonic() >= end:
				if until == 'prompt':
					raise ScenarioError(f'{where}: no shell prompt within {int(limit)} s, so nothing bounded the assertion')
				break
			time.sleep(0.1)
		# AND THE CURSOR STAYS WHERE IT WAS. This step asserts a negative; consuming the window it
		# watched would hide the guest's actual output from whatever comes next, which is how the
		# old implementation could lose a marker twice over - once past its own two-second sample
		# and once past the cursor it then advanced. A step asserting that nothing appeared has no
		# business deciding that what did appear has been dealt with.


def strip_ansi(data):
	return ANSI.sub(b'', data)
