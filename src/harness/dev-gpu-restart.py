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
#   disable              the verb is ACCEPTED and the node reaches `disabled`, and `lsdev --incident`
#                        then says nothing has gone wrong on that binding - which is the claim that a
#                        planned stop is not a crash, made about a real device and asked of the
#                        surface an operator reads. Nothing else in this tree executes it: the kernel
#                        suite exits before any teardown confirms.
#   enable               the verb is ACCEPTED, the node returns to `online`, and its claim generation
#                        has MOVED. A new generation is what makes this a rebind rather than a node
#                        that never left.

#   no fault            no `iommu: FAULT` anywhere in the window: the teardown and the fresh attach
#                        did not leave the device reaching memory it no longer owns, which is the
#                        whole reason a restart under enforcement is a different question from a
#                        restart without one.
#   it publishes again   the rebound binding republished the providers it declares, which is the
#                        driver's own half of coming back.
#   the display          frames are driven through the console after the rebind and it is still
#                        serving. What this can and cannot see is written on the function.
#   one boot             the guest never restarted, so none of the above is a reboot wearing a
#                        rebind's name.
#
# ASKED OF `lsdev` AND NOT OF THE SERIAL LOG (corrected 2026-09-02). Both of those used to wait for a
# DeviceManager line on serial, and after the boot no such line arrives there: once ConsoleService has
# taken the console a service's `print` goes to its VT, and the serial line carries the shell's
# session. Measured - the disable is accepted, the node reaches `disabled`, and the serial log gains
# nothing at all. A check that waits sixty seconds for a line that cannot appear reports a working
# mechanism as broken. The kernel's own lines - `iommu: FAULT`, `KERNEL PANIC` - DO reach the serial
# log, and those are still read from it.
#
# THE DISPLAY'S CONSUMER SIDE IS NO LONGER THE GAP IT WAS. The reason it used to be was exact:
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
def wait_state(state, what, timeout=SETTLE_TIMEOUT):
	deadline = time.monotonic() + timeout
	seen = None
	while time.monotonic() < deadline:
		seen = gpu_binding()
		if seen.get('state') == state:
			return seen
		time.sleep(0.5)
	fail(f'{what}: the binding is {seen.get("state") if seen else "unreadable"} and not {state} after {timeout} s')


# Drive frames through the console after the rebind and require one to REACH the display.
#
# WHAT THIS USED TO BE ABLE TO SEE, AND WHY IT WAS NOT ENOUGH. ConsoleService latches one line per
# outcome for the whole boot, and after boot its `print` follows stdout to a VT rather than to the
# serial log - so `a frame did NOT reach the display` cannot appear in this window, its absence
# proved nothing, and the only assertion left was that the shell still answered a prompt. A live
# prompt does not establish that DisplayService adopted the rebound provider or that a present
# reached its scanout, which is exactly what M4 asks for.
#
# DisplayService says it now, from the process that owns the scanout and performs the copy: one line
# for the first present through each provider it ADOPTS, written to the debug port so it reaches the
# serial log whatever owns the console. Per adoption rather than per boot, so the line the cold boot
# printed cannot stand in for this one - the window below starts after the enable.
#
# WHAT IT DOES NOT PROVE, corrected 2026-09-03. This said the line also exercises DeviceManager's
# `Catalogue::close_channel`, on the reasoning that the replacement can only be adopted once the old
# provider channel has closed. The close is real and the adoption does depend on it - but it is not
# the CATALOGUE's close: DisplayService took the offered channel through `open`, which MOVES the
# handle out of the entry and leaves the stored one zero, so `close_channel` has nothing to close for
# this provider and is a no-op on this path. What ends the old channel is the driver process exiting
# and its peer closing, which DisplayService already handles. Emptying the catalogue's close body
# leaves this gate green, and the comment claiming otherwise was the third round of an assertion this
# harness cannot make.
PRESENTED = 'DisplayService: a frame reached the display through the provider it adopted'
# The withdrawal half of a publication, said by the SUBSCRIBER rather than by the manager - a line
# the manager prints would prove it reached its own send and no further.
WITHDRAWN = 'DisplayService: a display provider was withdrawn and this service was told'
NOT_PRESENTED = 'DisplayService: a frame did NOT reach the display through the provider it adopted'


def exercise_the_display(guest, mark, generation):
	for _ in range(3):
		guest.type_text('\x03', False, 15)
		guest.wait_prompt(5)
		guest.type_text('clear', True, 15)
	time.sleep(2)
	said = lab.serial_since(mark)
	if NOT_PRESENTED in said:
		fail('DisplayService adopted the rebound provider and its first present through it did not reach the display')
	if 'a frame did NOT reach the display' in said:
		fail('frames were driven through the console after the rebind and did NOT reach the display - the rebound provider was published and the display never adopted it')
	if PRESENTED not in said:
		fail('no frame reached the display through a provider adopted after the rebind - DisplayService either never adopted the generation-2 provider or never presented on it')
	step('  a frame reached the display through the provider adopted after the rebind')
	guest.type_text('\x03', False, 15)
	if not guest.wait_prompt(15):
		fail('the console stopped answering after frames were driven through it, so the rebind took the display path down with it')
	step('  frames were driven through the console after the rebind and it is still serving')
	# AND THE DRIVER IS STILL THERE, which M4 asks for in the same sentence as the frame (added
	# 2026-09-03). A driver that served the framebuffer handshake, presented once and then exited
	# satisfies every line above: the state was read BEFORE the frames were driven, and a present
	# that reaches no provider is not reported.
	#
	# AND IT IS THE BINDING THE ENABLE CREATED, WHICH THE RE-READ DID NOT SAY (fixed 2026-09-03).
	# The comment claimed the re-read was "on the same claim generation" and the code asked only
	# whether SOME binding was `online`. The generation-2 binding may die during the frames and an
	# automatic recovery bind to generation 3 satisfies that read - and generation 3 emits its own
	# adoption and present lines, so every check above passes too. What M4 asks is that the binding
	# the commanded restart produced survived the frame, so that generation is what this compares.
	after = gpu_binding()
	if after.get('state') != 'online':
		fail(f'the rebound binding is {after.get("state")} after the display was exercised - a frame reaching the display is only half of what M4 asks for')
	if after.get('generation') != generation:
		fail(f'the device is online on claim generation {after.get("generation")}, not on {generation} which the enable created - the commanded rebind died during the frames and something else took the device')
	step(f'  and the binding the enable created is still online on claim generation {after.get("generation")}')


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
	#
	# ASKED OF `lsdev` AND NOT OF THE SERIAL LOG (corrected 2026-09-02). This waited for
	# DeviceManager's `stopped cleanly` line to appear on serial, and after the boot that line does
	# not arrive there: once ConsoleService has taken the console, a service's `print` goes to its VT
	# and the serial line carries the shell's session. Measured - the disable is accepted, the node
	# reaches `disabled`, and nothing at all is written to the serial log. A check that waits sixty
	# seconds for a line that cannot appear reports the mechanism broken when it worked.
	#
	# What replaces it is stronger rather than weaker: the operator's own surfaces. The node reaching
	# `disabled` is the stop having completed, `--incident` below is DeviceManager's judgement of
	# whether it was clean, and a restart would have landed the node ONLINE rather than `disabled` -
	# so the state that used to be one of three assertions is now the whole of them, and each is a
	# fact an operator can read rather than a string in a log.
	mark = lab.serial_size()
	step(f'disabling device {index}')
	answered = guest_run('lsdev', '--disable', str(index))
	if 'accepted' not in answered:
		fail(f'the disable was not accepted:\n{answered}')
	stopped = wait_state('disabled', 'after the disable')
	step(f'  the stop completed and the node is {stopped.get("state")} ({answered.strip().splitlines()[-1] if answered.strip() else "no output"})')

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

	# AND THE WITHDRAWAL REACHED THE SUBSCRIBER, which is the second of the two effects an emptied
	# publication owes and had no oracle at all until now (added 2026-09-03).
	#
	# `Catalogue::announce_gone` sends a `live: false` frame to every subscriber of the kind. The
	# host race test drives the ORDER of those two effects through a recorder and cannot fail if the
	# send itself is removed; this is the production path, end to end - the operator's disable
	# withdraws the binding's providers, the manager announces it, and the service that subscribed to
	# `Display` says it was told. What the service does NOT do is release its scanout on the frame:
	# the driver's channel closing is the authoritative signal for a driver that has gone, and the
	# adoption below is what proves the replacement is picked up.
	told = lab.serial_since(mark)
	if WITHDRAWN not in told:
		fail(f'the display provider was withdrawn and no subscriber was told - the announcement half of a withdrawal reached nobody:\n{told}')
	step('  and the withdrawal was announced to the service subscribed to the kind')

	# THE REBIND. `enable` lifts the stored disable and asks for one attempt; the driver has to
	# acquire the device again, which under enforcement means a fresh domain and a fresh attach.
	rebind_mark = lab.serial_size()
	step(f'enabling device {index}')
	enabled = guest_run('lsdev', '--enable', str(index))
	if 'accepted' not in enabled:
		fail(f'the enable was not accepted:\n{enabled}')
	# THE STATE, NOT THE LINE - see the note on the disable above. `online` at a NEW claim generation
	# is what a rebind IS, and the generation check below is what makes it one rather than a node
	# that never left.
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

	# AND THE DISPLAY, over the window that starts at the enable: the adoption line has to be this
	# rebind's, not the one the cold boot printed.
	exercise_the_display(guest, rebind_mark, after['generation'])

	# AND THE FAULT-FREE WINDOW COVERS THE FRAMES TOO, WHICH IT DID NOT (fixed 2026-09-03).
	#
	# The serial snapshot above was taken and judged BEFORE the display was exercised and never read
	# again, so a fault raised by the first presentation through the rebound provider - which is
	# precisely the DMA the new domain and the new attach were built for - passed unseen. The claim
	# this milestone records is that no `iommu: FAULT` appears anywhere in the window; the window
	# ends where the exercise does, so it is read again from the same mark.
	whole = lab.serial_since(mark)
	if 'iommu: FAULT' in whole:
		fail(f'the device faulted during the restart or the presentation that followed it:\n{whole}')
	if 'KERNEL PANIC' in whole:
		fail(f'the guest panicked during the restart or the presentation that followed it:\n{whole}')
	step('  and no fault or panic appeared anywhere from the disable to the end of the frames')

	boot_at_end = lab.guest_boot()
	if boot_at_end != boot_at_start:
		fail(f'the guest restarted during the run (boot {boot_at_start} -> {boot_at_end}), so the rebind above was a reboot')
	step(f'one boot throughout: generation {boot_at_start}')
	print('dev-gpu-restart: passed')


if __name__ == '__main__':
	main()
