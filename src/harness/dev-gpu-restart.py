#!/usr/bin/env python3
# A CONTROLLED RESTART OF THE DISPLAY DRIVER, ON THE MACHINE THAT TRANSLATES ITS DMA.
#
# P02M0159's M4 asks the enforcing profile to show the GPU surviving a restart, and every gate that
# looks at that profile proves the opposite half: `check-qemu-virtio-iommu-x86_64.sh` requires
# `driver.virtio-gpu: online (` EXACTLY ONCE and fails on `DeviceManager: restarting virtio-gpu`,
# because on a cold boot a second bind means a restart loop. A gate whose subject is that the driver
# comes up once cannot also be the gate that asks it to come up twice, so this is a separate check
# and it drives the transition rather than watching for it.
#
# WHY THIS IS A DEV CHECK AND NOT A GATE. It needs a guest that can be TOLD to do something after it
# has booted, which is a running development instance and its agent - the same reason `dev-selftest`,
# `proto-test` and `perf-gate` live here. The earlier claim that no such seam existed was wrong: the
# instance boots through `run.sh`, whose default x86_64 machine has a virtio-iommu with every virtio
# endpoint behind it, and `lab dev-launch` runs a program in it and reads what the program printed.
# `lsdev --disable` and `--enable` are the operator verbs P02M0166 built, so the restart is asked for
# through the production policy path rather than by killing something.
#
# WHAT IS ASSERTED, and what each assertion is worth:
#
#   translated first     the machine is the enforcing one, from the boot's own isolation summary.
#                        Everything below is about a translated device or it is about nothing.
#   online, generation G the driver holds a claim before the restart.
#   disable             `DeviceManager` says the stop was CLEAN, and `lsdev --incident` then says
#                        nothing has gone wrong on that binding - which is the P02M0165 claim that a
#                        planned stop is not a crash, made about a real device and asked of the
#                        surface an operator reads. Nothing else in this tree executes it: the kernel
#                        suite exits before any teardown confirms.
#   enable              a SECOND `driver.virtio-gpu: online (` and a NEW claim generation. A new
#                        generation is what makes this a rebind rather than a node that never left.
#   no fault            no `iommu: FAULT` anywhere in the window: the teardown and the fresh attach
#                        did not leave the device reaching memory it no longer owns, which is the
#                        whole reason a restart under enforcement is a different question from a
#                        restart without one.
#   it publishes again   the rebound binding republished the providers it declares, which is the
#                        driver's own half of coming back.
#   frames again         a frame driven through the console AFTER the rebind reaches the display.
#                        This is M4's own sentence and it is now an assertion (2026-09-02).
#   one boot             the guest never restarted, so none of the above is a reboot wearing a
#                        rebind's name.
#
# THE DISPLAY WAS REPORTED AND IS NOW ASSERTED. It could not be before, and the reason was exact:
# `route_offers` - the function that handed a published provider to DisplayService - was called only
# from the phase-two bring-up loop and filled each fixed consumer slot only `if *client == 0`, so a
# driver rebound after bring-up published its provider into the catalogue and nothing routed it while
# DisplayService went on holding the handle of a binding that had ended. There is no such slot now:
# DisplayService subscribes to the display kind and adopts the replacement provider itself, which is
# the seam the provider catalogue exists to be, and the restore this check is named for. A check
# that reports what it was
# built to prove is a check nobody fails, so this one fails.

import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import lab

# The driver artifact, as the registry names it and as `lsdev` reports it.
ARTIFACT = 'virtio_gpu'
ONLINE_LINE = 'driver.virtio-gpu: online ('
CHILD_TIMEOUT = 180
SETTLE_TIMEOUT = 60


def fail(message):
	print(f'dev-gpu-restart: {message}', file=sys.stderr)
	sys.exit(1)


def step(message):
	print(f'dev-gpu-restart: {message}')


def lab_command(*args, timeout=CHILD_TIMEOUT):
	try:
		return subprocess.run([os.path.join(HERE, 'lab.py'), *args], cwd=lab.SRC, capture_output=True, text=True, timeout=timeout)
	except subprocess.TimeoutExpired as expired:
		return subprocess.CompletedProcess(args, 124, expired.stdout or '', (expired.stderr or '') + f'\ndev-gpu-restart: lab.py {args[0]} did not finish within {timeout} s')


# Run one program in the guest and answer what it printed.
#
# `dev-launch` reports the exit of the REQUEST rather than of the program, so a non-zero status here
# means the agent could not run it at all - which is a different failure from the program refusing,
# and both have to be told apart from the program succeeding and printing nothing.
def guest_run(*argv):
	done = lab_command('dev-launch', '--timeout', str(SETTLE_TIMEOUT), *argv)
	if done.returncode != 0:
		fail(f'the guest could not run `{" ".join(argv)}`\n{done.stdout}{done.stderr}')
	# A REFUSAL THE PROGRAM PRINTS IS NOT A FAILED REQUEST, and `dev-launch` reports only the second.
	#
	# `lsdev` needs the operator's device-policy endpoint and says so when this boot granted it none.
	# Without this the disable below would simply never happen and the check would fail forty seconds
	# later on a missing log line, which names the symptom and not the cause.
	if 'granted no device-policy authority' in done.stdout:
		fail(f'`{" ".join(argv)}` was refused: this guest granted the launched program no device-policy authority, so the operator verbs cannot be driven here at all')
	return done.stdout


# Every binding `lsdev` knows about, as records.
#
# `json-min` rather than the text listing: the text is for a person and its columns are free to
# change, while the record is the IDL's and changing it is a wire change somebody has to make on
# purpose. A listing that cannot be parsed is a failure rather than an empty list, because an empty
# list would read as "this machine has no bindings" - which is a claim about the guest, not about
# the parsing.
def bindings():
	text = guest_run('lsdev', 'json-min')
	start, end = text.find('['), text.rfind(']')
	if start < 0 or end < start:
		fail(f'`lsdev json-min` did not answer a JSON array; it printed {text!r}')
	try:
		return json.loads(text[start:end + 1])
	except json.JSONDecodeError as broken:
		fail(f'`lsdev json-min` answered something that is not JSON ({broken}); it printed {text!r}')


def gpu_binding():
	for record in bindings():
		if record.get('artifact') == ARTIFACT:
			return record
	fail(f'no binding for `{ARTIFACT}` in this guest; `lsdev` listed {[r.get("artifact") for r in bindings()]}')


# Wait for `needle` to appear in the serial log after `mark`, and answer the text that arrived.
def wait_for(mark, needle, what, timeout=SETTLE_TIMEOUT):
	deadline = time.monotonic() + timeout
	while time.monotonic() < deadline:
		text = lab.serial_since(mark)
		if needle in text:
			return text
		time.sleep(0.3)
	fail(f'{what}: the guest never printed {needle!r} within {timeout} s. It printed:\n{lab.serial_since(mark)}')


# Wait for a binding to reach `state`, and answer the record.
def wait_state(state, what, timeout=SETTLE_TIMEOUT):
	deadline = time.monotonic() + timeout
	seen = None
	while time.monotonic() < deadline:
		seen = gpu_binding()
		if seen.get('state') == state:
			return seen
		time.sleep(0.5)
	fail(f'{what}: the binding is {seen.get("state") if seen else "unreadable"} and not {state} after {timeout} s')


# Make the console present frames and require that they land.
#
# ASSERTED, not reported - see the header. `ConsoleService` latches one line per outcome, so
# `a frame did NOT reach the display` appearing in this window is the consumer-side route being dead
# after the rebind, which is exactly what M4 forbids.
def require_the_display(guest):
	mark = lab.serial_size()
	for _ in range(3):
		guest.type_text('\x03', False, 15)
		guest.wait_prompt(5)
		guest.type_text('clear', True, 15)
	time.sleep(2)
	said = lab.serial_since(mark)
	if 'a frame did NOT reach the display' in said:
		fail('frames were driven through the console after the rebind and did NOT reach the display - the rebound provider was published and the display never adopted it')
	step('  frames were driven through the console after the rebind and none was reported as refused')


def main():
	state, _ = lab.dev_state()
	if state != 'ready':
		fail(f'the development instance is {state}; this needs a running one, so start it with `./dev.sh up`')
	boot_at_start = lab.guest_boot()
	if boot_at_start is None:
		fail('the guest did not answer a handshake, so nothing below could be attributed to one boot')

	# THE MACHINE THIS IS ABOUT. The isolation summary is the kernel's own answer, printed once the
	# scan is done, and a run without it is a run on the machine this check has nothing to say about.
	boot_log = lab.serial_since(0)
	if 'dma: every bus-mastering device is translated' not in boot_log:
		fail('this guest is not the translated machine - its boot printed no clean isolation summary, so a restart here proves nothing about enforcement')
	if 'dma: DEGRADED ISOLATION' in boot_log or 'dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY' in boot_log:
		fail('this guest has endpoints reaching memory untranslated, so it is not the enforcing machine')
	step('the guest is the translated machine')

	before = gpu_binding()
	if before.get('state') != 'online':
		fail(f'`{ARTIFACT}` is {before.get("state")} rather than online, so there is no running binding to restart')
	index, generation = before['index'], before['generation']
	step(f'{ARTIFACT} is online at device {index}, claim generation {generation}')

	guest = lab.LabGuest(SETTLE_TIMEOUT)

	# THE STOP. Through the operator's own verb, so what is proved is the path an operator has.
	mark = lab.serial_size()
	step(f'disabling device {index}')
	answered = guest_run('lsdev', '--disable', str(index))
	wait_for(mark, 'stopped cleanly', 'the planned stop')
	stopped = wait_state('disabled', 'after the disable')
	if 'restarting' in lab.serial_since(mark):
		fail(f'the disable was answered with a RESTART: {lab.serial_since(mark)}')
	step(f'  it stopped cleanly and the node is {stopped.get("state")} ({answered.strip().splitlines()[-1] if answered.strip() else "no output"})')

	# AND A CLEAN STOP IS NOT AN INCIDENT, asked of the surface an operator actually reads.
	#
	# This is the one place in the tree that executes it. The kernel suite sends `STOP` at every
	# shutdown and the machine exits before any teardown confirms - no `resolve_teardown` completes in
	# a whole run - so the arm that decides whether a planned stop is recorded as a failure is never
	# reached there. Here the driver answers, the teardown settles, and `lsdev --incident` is what
	# says which of the two it was judged to be.
	incident = guest_run('lsdev', '--incident', str(index))
	if 'nothing has gone wrong' not in incident:
		fail(f'a clean planned stop was recorded as an incident on this binding:\n{incident}')
	step('  and it is not recorded as an incident')

	# THE REBIND. `enable` lifts the stored disable and asks for one attempt; the driver has to
	# acquire the device again, which under enforcement means a fresh domain and a fresh attach.
	rebind_mark = lab.serial_size()
	step(f'enabling device {index}')
	guest_run('lsdev', '--enable', str(index))
	wait_for(rebind_mark, ONLINE_LINE, 'the rebind')
	after = wait_state('online', 'after the enable')
	if after['generation'] == generation:
		fail(f'the binding came back on claim generation {after["generation"]}, the same one it had - nothing was re-acquired, so this is not a rebind')
	step(f'  {ARTIFACT} is online again on claim generation {after["generation"]}')

	# AND NOTHING FAULTED WHILE ALL THAT HAPPENED. A restart under enforcement is a teardown of a
	# translation and a fresh one; a device left able to reach the old mapping says so here.
	window = lab.serial_since(mark)
	if 'iommu: FAULT' in window:
		fail(f'the device faulted during the restart:\n{window}')
	if 'KERNEL PANIC' in window:
		fail(f'the guest panicked during the restart:\n{window}')

	# AND THE DRIVER PUBLISHED AGAIN, which is its own half of coming back: a binding that acquired
	# the device and declared nothing would be online and useless.
	if after.get('providers', 0) < 1:
		fail(f'the rebound binding publishes {after.get("providers")} provider(s) - it came back without offering what it declares')
	step(f'  and it republished {after["providers"]} provider(s)')

	# AND THE DISPLAY, WHICH IS M4'S OWN SENTENCE - see the header.
	require_the_display(guest)

	boot_at_end = lab.guest_boot()
	if boot_at_end != boot_at_start:
		fail(f'the guest restarted during the run (boot {boot_at_start} -> {boot_at_end}), so the rebind above was a reboot')
	step(f'one boot throughout: generation {boot_at_start}')
	print('dev-gpu-restart: passed')


if __name__ == '__main__':
	main()
