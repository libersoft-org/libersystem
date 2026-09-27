#!/usr/bin/env python3
"""The frame account's collector, held against fixture logs before anything is booted.

WHAT IT PROVES. The join (the present serial, the device call's nesting, the thread), the grouping by
damage shape, the work/wait split from switch and wake records, and the residue - over a synthetic
drain whose every number is known because this file wrote it. And that the collector REFUSES the
runs that do not measure what an account says: a buffer that refused records, a frame missing a
site, a second thread of the application on a CPU inside a draw, a present whose serial joins
nothing, and an account that disagrees with the demo's own armed report.

It is run by `check-qemu-2d-account.sh` before it boots a guest, and on its own.
"""

import importlib.util
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = importlib.util.spec_from_file_location("frame_account", os.path.join(HERE, "..", "harness", "frame_account.py"))
frame_account = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(frame_account)

APP, SIBLING, SERVICE, DRIVER = 11, 12, 21, 31
HZ = 1_000_000_000  # one cycle is one nanosecond, so every fixture number reads as time


class Log:
	"""A synthetic drain, written line by line in the form the kernel writes."""

	def __init__(self):
		self.lines = [f"\x1ePERF tsc_hz {HZ}"]
		self.records = 0

	def site(self, label, cycles, value, thread):
		self.lines.append(f"\x1ePERF {label} {cycles} {value} {thread} 0 site")
		self.records += 1

	def switch(self, cycles, outgoing, incoming, why):
		self.lines.append(f"\x1ePERF switch {cycles} {outgoing} {incoming} 0 {why}")
		self.records += 1

	def wake(self, cycles, waker, woken, cause):
		self.lines.append(f"\x1ePERF wake {cycles} {waker} {woken} 0 {cause}")
		self.records += 1

	def end(self, refused=0):
		# The name groups are records too: four chunks per thread.
		named = [(APP, 5, "test2d-sw"), (SIBLING, 5, "test2d-sw"), (SERVICE, 6, "display_service"), (DRIVER, 7, "virtio_gpu")]
		for thread, process, name in named:
			self.lines.append(f"\x1ePERF-THREAD {thread} {process} {name}")
		self.lines.append(f"\x1ePERF-END {self.records + 4 * len(named)} {refused} 0 0")
		return "\n".join(self.lines) + "\n"


def frame(log, start, serial, shape, draw_ns=2_000_000, drop=None):
	"""One frame starting at `start`, returning where it ends. Every boundary is a known offset."""

	def site(label, offset, value, thread):
		if label != drop:
			log.site(label, start + offset, value, thread)

	# The pacing wait: the release ends the first park at once, the second waits for its deadline.
	site("park-ns", 1_000, 0, APP)
	site("park-tk", 1_100, 0, APP)
	site("park-end", 1_200, 0, APP)
	site("park-ns", 2_000, 16_000_000, APP)
	site("park-tk", 2_100, 2, APP)
	log.switch(start + 2_200, APP, 0, "blocked")
	log.wake(start + 20_000_000, 0, APP, "deadline")
	log.switch(start + 20_050_000, 0, APP, "yielded")
	site("park-end", 20_060_000, (1 << 64) - 110, APP)
	site("acq-beg", 20_100_000, 0, APP)
	site("acq-end", 20_150_000, 0, APP)
	site("rec-beg", 20_160_000, 0, APP)
	site("rec-end", 20_660_000, 0, APP)
	site("drw-beg", 20_670_000, 0, APP)
	site("drw-end", 20_670_000 + draw_ns, 0, APP)
	after_draw = 20_670_000 + draw_ns
	site("shape", after_draw + 10_000, shape, APP)
	site("ready", after_draw + 20_000, 0, APP)
	site("prs-beg", after_draw + 30_000, 0 if shape == 0 else 2, APP)
	# The present call: the application blocks, DisplayService wakes on the message and runs.
	log.switch(start + after_draw + 31_000, APP, 0, "blocked")
	log.wake(start + after_draw + 32_000, APP, SERVICE, "message")
	log.switch(start + after_draw + 40_000, 0, SERVICE, "yielded")
	site("ds-woke", after_draw + 41_000, 3, SERVICE)
	site("ds-req", after_draw + 42_000, 7, SERVICE)
	site("ds-acc", after_draw + 50_000, serial, SERVICE)
	site("ds-accs", after_draw + 50_100, 0, SERVICE)
	site("ds-blt0", after_draw + 60_000, 0, SERVICE)
	site("ds-blt1", after_draw + 1_060_000, 0, SERVICE)
	site("ds-dev0", after_draw + 1_061_000, 0, SERVICE)
	# The device call: DisplayService blocks, the driver wakes and submits two commands.
	log.switch(start + after_draw + 1_062_000, SERVICE, 0, "blocked")
	log.wake(start + after_draw + 1_062_500, SERVICE, DRIVER, "message")
	log.switch(start + after_draw + 1_070_000, 0, DRIVER, "yielded")
	site("gp-call", after_draw + 1_071_000, 3, DRIVER)
	site("vq-ntfy", after_draw + 1_080_000, 0, DRIVER)
	site("vq-done", after_draw + 1_580_000, 4096 | (1 << 32), DRIVER)
	site("vq-ntfy", after_draw + 1_590_000, 0, DRIVER)
	site("vq-done", after_draw + 2_090_000, 100, DRIVER)
	site("gp-rply", after_draw + 2_100_000, 3, DRIVER)
	log.wake(start + after_draw + 2_100_500, DRIVER, SERVICE, "message")
	log.switch(start + after_draw + 2_101_000, DRIVER, 0, "blocked")
	log.switch(start + after_draw + 2_102_000, 0, SERVICE, "yielded")
	site("ds-dev1", after_draw + 2_103_000, 1, SERVICE)
	site("ds-srcpx", after_draw + 2_104_000, 307_200, SERVICE)
	site("ds-outpx", after_draw + 2_104_100, 852_800, SERVICE)
	site("ds-path", after_draw + 2_104_200, 0, SERVICE)
	site("ds-scan", after_draw + 2_104_300, (1280 << 32) | 800, SERVICE)
	site("ds-cmpl", after_draw + 2_110_000, serial, SERVICE)
	log.wake(start + after_draw + 2_111_000, SERVICE, APP, "message")
	site("ds-rply", after_draw + 2_115_000, 7, SERVICE)
	site("ds-wait", after_draw + 2_116_000, 9, SERVICE)
	log.switch(start + after_draw + 2_117_000, SERVICE, 0, "blocked")
	log.switch(start + after_draw + 2_120_000, 0, APP, "yielded")
	end = after_draw + 2_130_000
	site("prs-end", end, serial, APP)
	return start + end


def run(frames=4, drop=None, refused=0, sibling_in_draw=False, bad_serial=False):
	"""A whole armed drain: `acct-beg` and `frames` frames, alternating the three shapes."""
	log = Log()
	cursor = 1_000_000
	log.site("acct-beg", cursor + 10, cursor, APP)
	log.switch(cursor + 20, SERVICE, 0, "blocked")
	log.switch(cursor + 30, DRIVER, 0, "blocked")
	for index in range(frames):
		serial = 100 + index
		frame_start = cursor
		cursor = frame(log, cursor, serial if not (bad_serial and index == 1) else serial, index % 3, drop=drop)
		if bad_serial and index == 1:
			# Rewrite the accepted serial so it joins nothing.
			for position, line in enumerate(log.lines):
				if " ds-acc " in line and f" {serial} " in line:
					log.lines[position] = line.replace(f" {serial} ", " 9999 ")
		if sibling_in_draw and index == 2:
			log.switch(frame_start + 20_670_000 + 500_000, 0, SIBLING, "yielded")
			log.switch(frame_start + 20_670_000 + 900_000, SIBLING, 0, "blocked")
	return log.end(refused)


def account(text, report=None):
	tsc_hz, drains = frame_account.parse_log(text)
	return frame_account.account(drains[-1], tsc_hz, report)


class Collector(unittest.TestCase):
	def test_a_whole_run_joins_into_frames_grouped_by_shape(self):
		result = account(run(frames=6))
		self.assertEqual(result["frames"], 6)
		self.assertEqual(set(result["shapes"]), {"whole", "partial", "multi-rect"})
		for summary in result["shapes"].values():
			self.assertEqual(summary["frames"], 2)
			self.assertLess(summary["residue_share"], frame_account.RESIDUE_BOUND)

	def test_the_join_finds_each_layer_and_the_device_time(self):
		whole = account(run(frames=3))["shapes"]["whole"]
		terms = {term["name"]: term for term in whole["terms"]}
		self.assertAlmostEqual(terms["application: draw (prepare and render)"]["ms"], 2.0, places=6)
		self.assertAlmostEqual(terms["DisplayService: blit"]["ms"], 1.0, places=6)
		# Two commands, half a millisecond each, and the spin's polls and yields carried through.
		self.assertAlmostEqual(whole["device"]["device_ms"], 1.0, places=6)
		self.assertEqual(whole["device"]["commands"], 2)
		self.assertEqual(whole["device"]["yields"], 1)
		self.assertEqual(whole["device"]["polls"], 4196)
		self.assertEqual(whole["scaled"], 1)

	def test_work_and_wait_come_from_the_scheduler_records(self):
		whole = account(run(frames=3))["shapes"]["whole"]
		terms = {term["name"]: term for term in whole["terms"]}
		pacing = terms["application: pacing wait and frame-loop step"]
		# Blocked from the park's switch out until the deadline wake, runnable until switched in.
		self.assertAlmostEqual(pacing["blocked_ms"], (20_000_000 - 2_200) / 1e6, places=6)
		self.assertAlmostEqual(pacing["runnable_ms"], 0.05, places=6)
		for term in whole["terms"]:
			self.assertAlmostEqual(term["work_ms"] + term["blocked_ms"] + term["runnable_ms"], term["ms"], places=6, msg=term["name"])

	def test_the_pacing_wait_names_the_fallback_and_the_tick_rounding(self):
		whole = account(run(frames=3))["shapes"]["whole"]
		self.assertAlmostEqual(whole["fallback_interval_ms"], 16.0, places=6)
		self.assertAlmostEqual(whole["tick_rounding_ms"], (20_060_000 - 2_000) / 1e6 - 16.0, places=6)
		self.assertAlmostEqual(whole["parks_per_frame"], 2.0)

	def test_a_buffer_that_refused_records_is_rejected(self):
		with self.assertRaisesRegex(frame_account.AccountError, "REFUSED"):
			account(run(refused=3))

	def test_a_frame_missing_a_site_is_rejected(self):
		with self.assertRaisesRegex(frame_account.AccountError, "rec-end"):
			account(run(drop="rec-end"))

	def test_a_second_thread_of_the_application_inside_a_draw_is_rejected(self):
		with self.assertRaisesRegex(frame_account.AccountError, "inside a draw"):
			account(run(frames=4, sibling_in_draw=True))

	def test_a_present_whose_serial_joins_nothing_is_rejected(self):
		with self.assertRaisesRegex(frame_account.AccountError, "join by serial"):
			account(run(frames=3, bad_serial=True))

	def test_lines_lost_on_the_wire_are_rejected(self):
		text = run(frames=3)
		lines = text.split("\n")
		del lines[10]
		with self.assertRaisesRegex(frame_account.AccountError, "lost on the serial wire"):
			account("\n".join(lines))

	def test_no_anchor_is_rejected(self):
		text = run(frames=3).replace(f"\x1ePERF tsc_hz {HZ}\n", "")
		with self.assertRaisesRegex(frame_account.AccountError, "tsc_hz"):
			account(text)

	def test_the_demo_report_must_agree_with_the_account(self):
		result = account(run(frames=3))
		draw_ns = int(result["draw_total_ms"] * 1e6)
		interval_ns = int(result["interval_total_ms"] * 1e6)
		agreeing = {"presented": 3, "draw-total-ns": draw_ns, "interval-total-ns": interval_ns}
		account(run(frames=3), agreeing)
		with self.assertRaisesRegex(frame_account.AccountError, "clock conversion"):
			account(run(frames=3), dict(agreeing, **{"draw-total-ns": draw_ns * 2}))
		with self.assertRaisesRegex(frame_account.AccountError, "armed presents"):
			account(run(frames=3), dict(agreeing, presented=4))

	def test_the_demo_report_line_parses(self):
		report = frame_account.parse_demo_report("noise\ntest2d-sw: armed presented=152 whole=76 partial=38 multi-rect=38 draw-total-ns=1234 interval-total-ns=5678\n")
		self.assertEqual(report["presented"], 152)
		self.assertEqual(report["multi-rect"], 38)
		self.assertEqual(report["interval-total-ns"], 5678)

	def test_several_drains_in_one_log_stay_apart(self):
		text = run(frames=3) + run(frames=6)
		tsc_hz, drains = frame_account.parse_log(text)
		self.assertEqual(len(drains), 2)
		self.assertEqual(frame_account.account(drains[0], tsc_hz)["frames"], 3)
		self.assertEqual(frame_account.account(drains[1], tsc_hz)["frames"], 6)


if __name__ == "__main__":
	result = unittest.main(argv=[sys.argv[0]], exit=False, verbosity=1).result
	sys.exit(0 if result.wasSuccessful() else 1)
