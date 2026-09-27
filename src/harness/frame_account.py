"""The frame account: one drain of the kernel's record buffer, joined into frames and terms.

This is the collector half of the frame account's instrument, and it is the frame-account mode of
`perf-trace.py` (`perf-trace.py --frame-account LOG`); it lives in its own module so the fixture
tests and the gate can import it without a serial socket.

WHAT IT READS. A serial log that holds the `\\x1ePERF tsc_hz` anchor and one or more drains. A drain
is every record the kernel held, one `\\x1ePERF` line each - the three- and four-field marker form the
console tracer parses, extended by the thread, the core and the kind:

    \\x1ePERF <site> <cycles> <value> <thread> <core> site
    \\x1ePERF switch <cycles> <outgoing> <incoming> <core> <blocked|preempted|yielded|exited>
    \\x1ePERF wake <cycles> <waker> <woken> <core> <message|deadline|other>

then the table naming each thread's process (`\\x1ePERF-THREAD <thread> <process> <name>`) and one
line of counts (`\\x1ePERF-END <records> <refused> <incomplete> <stale>`).

WHAT IT JOINS, WITH NOTHING NEW ON THE WIRE. The application and DisplayService by the present
serial the service answers (`prs-end` carries it, `ds-acc` recorded it); the driver's records by
nesting inside DisplayService's device call, which is synchronous with one call in flight; the
scheduler's records by thread. A frame is the time from one present returning to the next - the
first armed frame starts where `acct-beg` says the last warm-up present returned.

WHAT IT REFUSES. A drain that refused or lost a record; a frame missing a site; a term with no
scheduler record to split its work from its wait; a second thread of the application on a CPU
inside a draw (a pool nobody pinned); an interval the named terms do not add up to within five
percent. Each is a run that does not measure what the account says it measures.
"""

import json
import re
import sys

RS = "\x1e"
RESIDUE_BOUND = 0.05
# `perfbuf::NAME_CHUNKS`: the records one thread's name group takes.
NAME_CHUNKS = 4

SHAPES = {0: "whole", 1: "partial", 2: "multi-rect"}
SWITCH_REASONS = ("blocked", "preempted", "yielded", "exited")
WAKE_CAUSES = ("message", "deadline", "other")


class AccountError(Exception):
	"""A drain that cannot be turned into an account, and why."""


def parse_log(text):
	"""Split a serial log into the anchor and its drains.

	Returns (tsc_hz or None, [drain, ...]); a drain is a dict with `records`, `threads` and `end`.
	Lines that are not the instrument's are ignored; a drain with no END line is not a drain.
	"""
	tsc_hz = None
	drains = []
	records = []
	threads = {}
	# NOT `splitlines()`: Python splits on the record separator itself, which is the one character
	# every instrument line starts with.
	for raw in text.split("\n"):
		start = raw.find(RS)
		if start < 0:
			continue
		line = raw[start + 1 :].rstrip("\r")
		fields = line.split()
		if not fields:
			continue
		if fields[0] == "PERF" and len(fields) == 3 and fields[1] == "tsc_hz":
			tsc_hz = int(fields[2])
			continue
		if fields[0] == "PERF-THREAD" and len(fields) >= 4:
			threads[int(fields[1])] = (int(fields[2]), " ".join(fields[3:]))
			continue
		if fields[0] == "PERF-END" and len(fields) == 5:
			end = {"records": int(fields[1]), "refused": int(fields[2]), "incomplete": int(fields[3]), "stale": int(fields[4])}
			drains.append({"records": records, "threads": threads, "end": end})
			records = []
			threads = {}
			continue
		if fields[0] != "PERF" or len(fields) != 7:
			continue
		try:
			label, cycles, value, thread, core, kind = fields[1], int(fields[2]), int(fields[3]), int(fields[4]), int(fields[5]), fields[6]
		except ValueError:
			continue
		records.append({"label": label, "cycles": cycles, "value": value, "thread": thread, "core": core, "kind": kind})
	return tsc_hz, drains


class Timeline:
	"""One thread's state over the window, from its switch and wake records.

	RUNNING from a switch in to a switch out; BLOCKED from a switch out that says blocked (or exited)
	until a wake; RUNNABLE from a wake, a preemption or a yield until the next switch in. The state
	before the first record is inferred from it: a switch out or a site means the thread was running,
	a switch in means it was runnable, a wake means it was blocked.
	"""

	def __init__(self, thread, events):
		self.thread = thread
		events = sorted(events, key=lambda event: event[0])
		first = events[0][1] if events else "site"
		self.initial = {"out": "running", "site": "running", "in": "runnable", "wake": "blocked"}[first]
		self.transitions = [event for event in events if event[1] != "site"]

	def split(self, t0, t1):
		"""(running, blocked, runnable) cycles of this thread inside [t0, t1]."""
		state = self.initial
		cursor = t0
		totals = {"running": 0, "blocked": 0, "runnable": 0}
		for cycles, kind, detail in self.transitions:
			if cycles <= t0:
				state = _next_state(state, kind, detail)
				continue
			if cycles >= t1:
				break
			totals[state] += cycles - cursor
			cursor = cycles
			state = _next_state(state, kind, detail)
		totals[state] += max(0, t1 - cursor)
		return totals["running"], totals["blocked"], totals["runnable"]

	def running_intervals(self):
		"""Every [begin, end) the thread was on a CPU, as far as the records say."""
		intervals = []
		state = self.initial
		begin = float("-inf") if state == "running" else None
		for cycles, kind, detail in self.transitions:
			after = _next_state(state, kind, detail)
			if state != "running" and after == "running":
				begin = cycles
			elif state == "running" and after != "running":
				intervals.append((begin, cycles))
				begin = None
			state = after
		if state == "running":
			intervals.append((begin, float("inf")))
		return intervals


def _next_state(state, kind, detail):
	if kind == "in":
		return "running"
	if kind == "out":
		return "blocked" if detail in ("blocked", "exited") else "runnable"
	if kind == "wake":
		return "runnable" if state != "running" else state
	return state


def build_timelines(records):
	"""One Timeline per thread that any record names."""
	events = {}
	for record in records:
		if record["label"] == "switch" and record["kind"] in SWITCH_REASONS:
			outgoing, incoming = record["value"], record["thread"]
			if outgoing:
				events.setdefault(outgoing, []).append((record["cycles"], "out", record["kind"]))
			if incoming:
				events.setdefault(incoming, []).append((record["cycles"], "in", ""))
		elif record["label"] == "wake" and record["kind"] in WAKE_CAUSES:
			events.setdefault(record["thread"], []).append((record["cycles"], "wake", record["kind"]))
		elif record["kind"] == "site":
			# A site is a moment the thread was on a CPU: it changes no state, but a thread whose
			# FIRST record is a site was running when the window opened.
			events.setdefault(record["thread"], []).append((record["cycles"], "site", ""))
	return {thread: Timeline(thread, thread_events) for thread, thread_events in events.items()}


def sites_of(records, thread, label):
	return [record for record in records if record["kind"] == "site" and record["thread"] == thread and record["label"] == label]


def only_thread(records, label):
	threads = {record["thread"] for record in records if record["kind"] == "site" and record["label"] == label}
	if not threads:
		raise AccountError(f"no `{label}` site in the drain - the instrument is missing a site")
	if len(threads) > 1:
		raise AccountError(f"`{label}` was recorded by {len(threads)} threads ({sorted(threads)}) - one frame loop was expected")
	return threads.pop()


def first_after(sites, cycles, before=None):
	for site in sites:
		if site["cycles"] >= cycles and (before is None or site["cycles"] <= before):
			return site
	return None


def all_between(sites, begin, end):
	return [site for site in sites if begin <= site["cycles"] <= end]


def account(drain, tsc_hz, demo_report=None):
	"""Turn one drain into frames, terms and checks. Raises AccountError for a run that is not one."""
	if not tsc_hz:
		raise AccountError("the log holds no `tsc_hz` anchor - boot the `development-trace` profile")
	end = drain["end"]
	if end["refused"]:
		raise AccountError(f"the buffer REFUSED {end['refused']} record(s) - the drain is not the whole run")
	if end["incomplete"]:
		raise AccountError(f"{end['incomplete']} record(s) were claimed and never completed - the drain is not whole")
	records = sorted(drain["records"], key=lambda record: record["cycles"])
	# THE COUNT INCLUDES THE NAME GROUPS, four records per named thread, which leave as the table's
	# lines rather than as record lines.
	carried = len(records) + NAME_CHUNKS * len(drain["threads"])
	if carried != end["records"]:
		raise AccountError(f"the END line counts {end['records']} record(s) and the log carries {carried} - lines were lost on the serial wire")
	ns = 1e9 / tsc_hz

	app = only_thread(records, "drw-beg")
	service = only_thread(records, "ds-acc")
	driver = only_thread(records, "gp-call")
	threads = drain["threads"]
	app_process = threads.get(app, (None, "?"))[0]
	timelines = build_timelines(records)
	for role, thread in (("the application", app), ("DisplayService", service), ("the driver", driver)):
		if thread not in timelines or not timelines[thread].transitions:
			raise AccountError(f"{role}'s thread {thread} has no switch or wake record - its terms have no work/wait pair")

	app_sites = [record for record in records if record["kind"] == "site" and record["thread"] == app]
	svc_sites = [record for record in records if record["kind"] == "site" and record["thread"] == service]
	drv_sites = [record for record in records if record["kind"] == "site" and record["thread"] == driver]

	begin = [site for site in app_sites if site["label"] == "acct-beg"]
	if not begin:
		raise AccountError("no `acct-beg` site - the first armed frame has no start")
	ends = [site for site in app_sites if site["label"] == "prs-end" and site["value"] != (1 << 64) - 1]
	if not ends:
		raise AccountError("no successful `prs-end` - nothing was presented while armed")

	# THE SECOND-THREAD CHECK: nothing of the application's process but the drawing thread is on a
	# CPU inside a draw. A pooled default nobody pinned would show here.
	siblings = [thread for thread, (process, _) in threads.items() if process == app_process and thread != app]
	draws = list(zip(sites_of(records, app, "drw-beg"), sites_of(records, app, "drw-end")))
	for sibling in siblings:
		timeline = timelines.get(sibling)
		if timeline is None:
			continue
		for draw_begin, draw_end in draws:
			for run_begin, run_end in timeline.running_intervals():
				if run_begin < draw_end["cycles"] and run_end > draw_begin["cycles"]:
					raise AccountError(f"thread {sibling} of the application ran on a CPU inside a draw at {draw_begin['cycles']} - the draw is not the serial walk")

	frames = []
	start = begin[0]["value"]
	for present_end in ends:
		frame = frame_terms(start, present_end, app, service, driver, app_sites, svc_sites, drv_sites, records, timelines, ns)
		frames.append(frame)
		start = present_end["cycles"]

	shapes = {}
	for frame in frames:
		shapes.setdefault(frame["shape"], []).append(frame)
	summary = {}
	for shape, group in sorted(shapes.items()):
		summary[SHAPES.get(shape, str(shape))] = summarise(group)
	for name, shape_summary in summary.items():
		if shape_summary["residue_share"] > RESIDUE_BOUND:
			raise AccountError(f"the {name} frames' named terms leave {shape_summary['residue_ms']:.3f} ms of a {shape_summary['interval_ms']:.3f} ms interval unattributed ({shape_summary['residue_share'] * 100:.1f} %)")

	result = {
		"tsc_hz": tsc_hz,
		"records": end["records"],
		"stale": end["stale"],
		"frames": len(frames),
		"threads": {str(thread): {"process": process, "name": name} for thread, (process, name) in threads.items()},
		"roles": {"application": app, "service": service, "driver": driver},
		"shapes": summary,
		"draw_total_ms": sum(frame["draw_ms"] for frame in frames),
		"interval_total_ms": sum(frame["interval_ms"] for frame in frames),
		"dispatch": dispatch_passes(svc_sites, records, service, ns),
		"primitives": on_path_primitives(records, app, service, driver, app_sites, svc_sites, ns),
	}
	if demo_report is not None:
		compare_with_demo(result, frames, demo_report)
	return result


# THE TERMS, in the order a frame passes them. Each is (name, begin site, end site, the thread
# whose progress it measures); a begin or end of `None` means "the frame's own start or end".
def frame_terms(start, present_end, app, service, driver, app_sites, svc_sites, drv_sites, records, timelines, ns):
	t_end = present_end["cycles"]
	window = [site for site in app_sites if start <= site["cycles"] <= t_end]

	def find(label, after=start):
		site = first_after([site for site in window if site["label"] == label], after, t_end)
		if site is None:
			raise AccountError(f"the frame ending at {t_end} has no `{label}` site")
		return site

	acquire_begin = last_before(window, "acq-beg", t_end)
	if acquire_begin is None:
		raise AccountError(f"the frame ending at {t_end} has no `acq-beg` site")
	acquire_end = find("acq-end", acquire_begin["cycles"])
	record_begin = find("rec-beg", acquire_end["cycles"])
	record_end = find("rec-end", record_begin["cycles"])
	draw_begin = find("drw-beg", record_end["cycles"])
	draw_end = find("drw-end", draw_begin["cycles"])
	shape = find("shape", draw_end["cycles"])
	ready = find("ready", shape["cycles"])
	present_begin = find("prs-beg", ready["cycles"])
	serial = present_end["value"]

	accepted = [site for site in svc_sites if site["label"] == "ds-acc" and site["value"] == serial and present_begin["cycles"] <= site["cycles"] <= t_end]
	if not accepted:
		raise AccountError(f"no `ds-acc` for serial {serial} inside its present call - the join by serial failed")
	accepted = accepted[0]
	svc_window = [site for site in svc_sites if present_begin["cycles"] <= site["cycles"] <= t_end]
	request = last_before(svc_window, "ds-req", accepted["cycles"])
	if request is None:
		raise AccountError(f"no `ds-req` before the present with serial {serial} was accepted")
	reply = first_after([site for site in svc_window if site["label"] == "ds-rply"], accepted["cycles"], t_end)
	if reply is None:
		raise AccountError(f"no `ds-rply` after the present with serial {serial} was accepted")
	completion = first_after([site for site in svc_window if site["label"] == "ds-cmpl" and site["value"] == serial], accepted["cycles"], t_end)
	if completion is None:
		raise AccountError(f"no `ds-cmpl` for serial {serial}")

	terms = []

	def term(name, t0, t1, thread, layer):
		running, blocked, runnable = timelines[thread].split(t0, t1)
		terms.append({"name": name, "layer": layer, "ms": (t1 - t0) * ns / 1e6, "work_ms": running * ns / 1e6, "blocked_ms": blocked * ns / 1e6, "runnable_ms": runnable * ns / 1e6})

	# THE PACING WAIT, with its parks named: the fallback interval the loop asked for, and what the
	# tick unit rounded it to beyond that.
	parks = park_breakdown(window, start, acquire_begin["cycles"], ns)
	term("application: pacing wait and frame-loop step", start, acquire_begin["cycles"], app, "application")
	terms[-1].update(parks)
	term("application: acquire (a call to DisplayService)", acquire_begin["cycles"], acquire_end["cycles"], app, "application")
	term("application: image mapping looked up", acquire_end["cycles"], record_begin["cycles"], app, "application")
	term("application: record the scene", record_begin["cycles"], record_end["cycles"], app, "application")
	term("application: record to draw", record_end["cycles"], draw_begin["cycles"], app, "application")
	term("application: draw (prepare and render)", draw_begin["cycles"], draw_end["cycles"], app, "application")
	term("application: damage computed", draw_end["cycles"], ready["cycles"], app, "application")
	term("application: producer-ready signal", ready["cycles"], present_begin["cycles"], app, "application")
	term("transport: present call to DisplayService", present_begin["cycles"], request["cycles"], service, "transport")
	term("DisplayService: dispatch until the present is accepted", request["cycles"], accepted["cycles"], service, "DisplayService")
	cursor = accepted["cycles"]
	blit = [site for site in svc_window if site["label"] in ("ds-blt0", "ds-blt1", "ds-dev0", "ds-dev1") and site["cycles"] >= accepted["cycles"]]
	blit_begin = first_label(blit, "ds-blt0")
	shape_value = shape["value"]
	device_ms = 0.0
	spin = {"device_ms": 0.0, "spin_work_ms": 0.0, "yields": 0, "polls": 0}
	scaled = None
	pixels = {}
	if blit_begin is not None:
		blit_end = first_label(blit, "ds-blt1")
		device_begin = first_label(blit, "ds-dev0")
		device_end = first_label(blit, "ds-dev1")
		if None in (blit_end, device_begin, device_end):
			raise AccountError(f"the present with serial {serial} blitted without every blit and device site")
		term("DisplayService: accepted to blit", cursor, blit_begin["cycles"], service, "DisplayService")
		term("DisplayService: blit", blit_begin["cycles"], blit_end["cycles"], service, "DisplayService")
		term("DisplayService: blit to device call", blit_end["cycles"], device_begin["cycles"], service, "DisplayService")
		device_terms, spin = driver_breakdown(device_begin["cycles"], device_end["cycles"], drv_sites, driver, service, timelines, ns)
		for name, t0, t1, thread, layer in device_terms:
			term(name, t0, t1, thread, layer)
		for extra in ("ds-srcpx", "ds-outpx", "ds-path", "ds-scan"):
			found = first_after([site for site in svc_window if site["label"] == extra], device_end["cycles"], t_end)
			if found is not None:
				pixels[extra] = found["value"]
		scaled = pixels.get("ds-path") == 0 if "ds-path" in pixels else None
		cursor = device_end["cycles"]
	term("DisplayService: completion (release and events)", cursor, completion["cycles"], service, "DisplayService")
	term("DisplayService: completion to reply sent", completion["cycles"], reply["cycles"], service, "DisplayService")
	term("transport: reply back to the application", reply["cycles"], t_end, app, "transport")

	interval_ms = (t_end - start) * ns / 1e6
	named = sum(item["ms"] for item in terms)
	return {"shape": shape_value, "serial": serial, "interval_ms": interval_ms, "terms": terms, "named_ms": named, "draw_ms": (draw_end["cycles"] - draw_begin["cycles"]) * ns / 1e6, "device": spin, "scaled": scaled, "pixels": pixels}


def last_before(sites, label, cycles):
	found = None
	for site in sites:
		if site["label"] == label and site["cycles"] <= cycles:
			found = site
	return found


def first_label(sites, label):
	for site in sites:
		if site["label"] == label:
			return site
	return None


def park_breakdown(window, t0, t1, ns):
	"""Every park between the frame's start and its acquire: what it asked for and what it got."""
	parks = []
	pending = {}
	for site in window:
		if not (t0 <= site["cycles"] <= t1):
			continue
		if site["label"] == "park-ns":
			pending = {"asked_ms": site["value"] / 1e6, "begin": site["cycles"]}
		elif site["label"] == "park-tk":
			pending["ticks"] = site["value"]
		elif site["label"] == "park-end" and "begin" in pending:
			value = site["value"]
			woke = value if value < (1 << 63) else value - (1 << 64)
			parks.append({"asked_ms": pending["asked_ms"], "ticks": pending.get("ticks", 0), "waited_ms": (site["cycles"] - pending["begin"]) * ns / 1e6, "ended_on": "message" if woke >= 0 else "deadline"})
			pending = {}
	deadline_parks = [park for park in parks if park["ended_on"] == "deadline"]
	fallback_ms = sum(park["asked_ms"] for park in deadline_parks)
	rounding_ms = sum(park["waited_ms"] - park["asked_ms"] for park in deadline_parks)
	return {"parks": parks, "fallback_interval_ms": fallback_ms, "tick_rounding_ms": rounding_ms}


def driver_breakdown(t0, t1, drv_sites, driver, service, timelines, ns):
	"""DisplayService's device call, split by the driver's own records nested inside it."""
	inside = [site for site in drv_sites if t0 <= site["cycles"] <= t1]
	call = first_label(inside, "gp-call")
	reply = first_label([site for site in inside if call is not None and site["cycles"] >= call["cycles"]], "gp-rply")
	if call is None or reply is None:
		raise AccountError(f"the device call at {t0} has no `gp-call`/`gp-rply` nested inside it")
	terms = [("transport: device call to the driver", t0, call["cycles"], driver, "transport")]
	cursor = call["cycles"]
	spin = {"device_ms": 0.0, "spin_work_ms": 0.0, "yields": 0, "polls": 0, "commands": 0}
	for notify in [site for site in inside if site["label"] == "vq-ntfy" and call["cycles"] <= site["cycles"] <= reply["cycles"]]:
		done = first_after([site for site in inside if site["label"] == "vq-done"], notify["cycles"], reply["cycles"])
		if done is None:
			raise AccountError(f"the command notified at {notify['cycles']} has no completion inside the call")
		terms.append(("driver: build the command", cursor, notify["cycles"], driver, "driver"))
		# THE DEVICE'S ACKNOWLEDGEMENT IS DEVICE TIME, and the guest CPU the spin burned meanwhile is
		# a row of its own - the thread is on a CPU while nothing in the guest computes.
		terms.append(("device: acknowledgement (notify to observed completion)", notify["cycles"], done["cycles"], driver, "device"))
		running, _, _ = timelines[driver].split(notify["cycles"], done["cycles"])
		spin["device_ms"] += (done["cycles"] - notify["cycles"]) * ns / 1e6
		spin["spin_work_ms"] += running * ns / 1e6
		spin["polls"] += done["value"] & 0xFFFFFFFF
		spin["yields"] += done["value"] >> 32
		spin["commands"] += 1
		cursor = done["cycles"]
	terms.append(("driver: reply", cursor, reply["cycles"], driver, "driver"))
	terms.append(("transport: device call back to DisplayService", reply["cycles"], t1, service, "transport"))
	return terms, spin


def summarise(frames):
	count = len(frames)
	interval = sum(frame["interval_ms"] for frame in frames) / count
	names = []
	for frame in frames:
		for item in frame["terms"]:
			if item["name"] not in names:
				names.append(item["name"])
	terms = []
	for name in names:
		items = [item for frame in frames for item in frame["terms"] if item["name"] == name]
		terms.append({
			"name": name,
			"layer": items[0]["layer"],
			"frames": len(items),
			"ms": sum(item["ms"] for item in items) / count,
			"work_ms": sum(item["work_ms"] for item in items) / count,
			"blocked_ms": sum(item["blocked_ms"] for item in items) / count,
			"runnable_ms": sum(item["runnable_ms"] for item in items) / count,
		})
	named = sum(term["ms"] for term in terms)
	residue = interval - named
	pacing = [item for frame in frames for item in frame["terms"] if item["name"].startswith("application: pacing")]
	device = {key: sum(frame["device"].get(key, 0) for frame in frames) / count for key in ("device_ms", "spin_work_ms", "yields", "polls", "commands")}
	return {
		"frames": count,
		"interval_ms": interval,
		"named_ms": named,
		"residue_ms": residue,
		"residue_share": abs(residue) / interval if interval else 0.0,
		"terms": terms,
		"fallback_interval_ms": sum(item.get("fallback_interval_ms", 0.0) for item in pacing) / count,
		"tick_rounding_ms": sum(item.get("tick_rounding_ms", 0.0) for item in pacing) / count,
		"parks_per_frame": sum(len(item.get("parks", [])) for item in pacing) / count,
		"device": device,
		"scaled": [frame["scaled"] for frame in frames].count(True),
		"direct": [frame["scaled"] for frame in frames].count(False),
		"source_pixels": sum(frame["pixels"].get("ds-srcpx", 0) for frame in frames) / count,
		"output_pixels": sum(frame["pixels"].get("ds-outpx", 0) for frame in frames) / count,
		"scanout": next((frame["pixels"]["ds-scan"] for frame in frames if "ds-scan" in frame["pixels"]), 0),
	}


def dispatch_passes(svc_sites, records, service, ns):
	"""DisplayService's loop passes, and what a pass of its wait costs against the handle count.

	A pass that blocks costs twice: entering the wait (`ds-wait` until the thread is switched out -
	the syscall registering on every handle) and leaving it (switched back in until `ds-woke` - the
	syscall finding which handle is ready). Both are on-CPU work that grows with the handles waited on,
	which is the one cost that scales with the surface count while nothing is composed.
	"""
	outs = [record["cycles"] for record in records if record["label"] == "switch" and record["kind"] in SWITCH_REASONS and record["value"] == service]
	ins = [record["cycles"] for record in records if record["label"] == "switch" and record["kind"] in SWITCH_REASONS and record["thread"] == service]
	waits = [site for site in svc_sites if site["label"] in ("ds-wait", "ds-woke")]
	by_handles = {}
	passes = 0
	for index in range(len(waits) - 1):
		first, second = waits[index], waits[index + 1]
		if first["label"] != "ds-wait" or second["label"] != "ds-woke":
			continue
		passes += 1
		out = next((cycles for cycles in outs if first["cycles"] <= cycles <= second["cycles"]), None)
		back = next((cycles for cycles in reversed(ins) if first["cycles"] <= cycles <= second["cycles"]), None)
		entry = by_handles.setdefault(first["value"], {"passes": 0, "blocked": 0, "enter": 0.0, "leave": 0.0, "unblocked": 0.0})
		entry["passes"] += 1
		if out is not None and back is not None and back >= out:
			entry["blocked"] += 1
			entry["enter"] += (out - first["cycles"]) * ns / 1e6
			entry["leave"] += (second["cycles"] - back) * ns / 1e6
		else:
			entry["unblocked"] += (second["cycles"] - first["cycles"]) * ns / 1e6
	summary = {}
	for handles, entry in sorted(by_handles.items()):
		blocked = entry["blocked"]
		unblocked = entry["passes"] - blocked
		summary[str(handles)] = {
			"passes": entry["passes"],
			"enter_ms": entry["enter"] / blocked if blocked else None,
			"leave_ms": entry["leave"] / blocked if blocked else None,
			"unblocked_ms": entry["unblocked"] / unblocked if unblocked else None,
		}
	return {"passes": passes, "by_handles": summary}


def on_path_primitives(records, app, service, driver, app_sites, svc_sites, ns):
	"""The primitives the terms are built of, from the path's own records.

	THE IPC ROUND TRIP: each `acquire` and each `present` call as the application timed it, less
	DisplayService's own handling of it (its request received to its reply sent) - two processes in
	two Domains, on the transport and at the message sizes this path actually uses. THE CHANNEL WAKE:
	every message wake of the application, DisplayService or the driver, until that thread next ran.
	THE DEADLINE WAKE: every deadline wake of the application's pacing park, until it next ran.
	"""

	def median(values):
		values = sorted(values)
		return values[len(values) // 2] if values else None

	requests = [site for site in svc_sites if site["label"] == "ds-req"]
	replies = [site for site in svc_sites if site["label"] == "ds-rply"]
	round_trips = {"acquire": [], "present": []}
	for kind, begin_label, end_label in (("acquire", "acq-beg", "acq-end"), ("present", "prs-beg", "prs-end")):
		begins = [site for site in app_sites if site["label"] == begin_label]
		ends = [site for site in app_sites if site["label"] == end_label]
		for begin in begins:
			end = first_after(ends, begin["cycles"])
			if end is None:
				continue
			request = first_after(requests, begin["cycles"], end["cycles"])
			reply = first_after(replies, request["cycles"], end["cycles"]) if request else None
			if request is None or reply is None:
				continue
			client = end["cycles"] - begin["cycles"]
			server = reply["cycles"] - request["cycles"]
			round_trips[kind].append((client - server) * ns / 1e6)
	switch_ins = {}
	for record in records:
		if record["label"] == "switch" and record["kind"] in SWITCH_REASONS and record["thread"]:
			switch_ins.setdefault(record["thread"], []).append(record["cycles"])
	wakes = {"message": [], "deadline": []}
	for record in records:
		if record["label"] != "wake" or record["thread"] not in (app, service, driver) or record["kind"] not in wakes:
			continue
		ran = next((cycles for cycles in switch_ins.get(record["thread"], []) if cycles >= record["cycles"]), None)
		if ran is not None:
			wakes[record["kind"]].append((ran - record["cycles"]) * ns / 1e6)
	return {
		"ipc_round_trip_ms": {kind: {"calls": len(values), "median": median(values), "mean": sum(values) / len(values) if values else None} for kind, values in round_trips.items()},
		"channel_wake_ms": {"wakes": len(wakes["message"]), "median": median(wakes["message"]), "mean": sum(wakes["message"]) / len(wakes["message"]) if wakes["message"] else None},
		"deadline_wake_ms": {"wakes": len(wakes["deadline"]), "median": median(wakes["deadline"]), "mean": sum(wakes["deadline"]) / len(wakes["deadline"]) if wakes["deadline"] else None},
	}


def parse_demo_report(text):
	"""The demo's `test2d-sw: armed ...` line, as numbers."""
	for line in text.split("\n"):
		if "test2d-sw: armed" in line:
			return {key: int(value) for key, value in re.findall(r"([a-z-]+)=(\d+)", line)}
	return None


def compare_with_demo(result, frames, report):
	"""The account and the demo's own armed report must agree, beyond the clock conversion."""
	if report.get("presented") != len(frames):
		raise AccountError(f"the account holds {len(frames)} frames and the demo reports {report.get('presented')} armed presents")
	for name, mine, theirs in (("draw", result["draw_total_ms"], report["draw-total-ns"] / 1e6), ("interval", result["interval_total_ms"], report["interval-total-ns"] / 1e6)):
		if theirs <= 0:
			raise AccountError(f"the demo's armed report has no {name} total")
		# THE CLOCK CONVERSION: the demo reads the kernel's nanosecond clock around its own work and
		# the account reads cycles at the sites; one percent covers the conversion and the few
		# microseconds between a site and the demo's clock read, and nothing else.
		if abs(mine - theirs) / theirs > 0.01:
			raise AccountError(f"the account's {name} total {mine:.3f} ms and the demo's {theirs:.3f} ms disagree by more than the clock conversion")
	result["demo"] = report


def render(result):
	"""The account as text."""
	lines = []
	lines.append(f"frame account: {result['frames']} frames, {result['records']} records, tsc {result['tsc_hz'] / 1e9:.4f} GHz")
	for shape, summary in result["shapes"].items():
		lines.append("")
		lines.append(f"  {shape}: {summary['frames']} frames, interval {summary['interval_ms']:.3f} ms, named {summary['named_ms']:.3f} ms, residue {summary['residue_ms']:.3f} ms ({summary['residue_share'] * 100:.2f} %)")
		lines.append(f"    {'term':<60} {'ms':>9} {'work':>9} {'blocked':>9} {'runnable':>9}")
		for term in summary["terms"]:
			lines.append(f"    {term['name']:<60} {term['ms']:9.3f} {term['work_ms']:9.3f} {term['blocked_ms']:9.3f} {term['runnable_ms']:9.3f}")
		lines.append(f"    pacing: fallback interval {summary['fallback_interval_ms']:.3f} ms, tick rounding {summary['tick_rounding_ms']:.3f} ms, {summary['parks_per_frame']:.2f} parks per frame")
		device = summary["device"]
		lines.append(f"    device: {device['commands']:.2f} commands, device time {device['device_ms']:.3f} ms, spin on CPU {device['spin_work_ms']:.3f} ms, {device['polls']:.0f} polls, {device['yields']:.2f} yields per frame")
		lines.append(f"    pixels: {summary['source_pixels']:.0f} source, {summary['output_pixels']:.0f} output per frame; {summary['direct']} direct, {summary['scaled']} scaled; scanout {summary['scanout'] >> 32}x{summary['scanout'] & 0xFFFFFFFF}")
	lines.append("")
	lines.append(f"  DisplayService loop: {result['dispatch']['passes']} passes")
	for handles, entry in result["dispatch"]["by_handles"].items():
		def fmt(value):
			return "n/a" if value is None else f"{value:.4f} ms"
		lines.append(f"    {handles} handles: {entry['passes']} passes; entering a blocking wait {fmt(entry['enter_ms'])}, leaving it {fmt(entry['leave_ms'])}, a wait that did not block {fmt(entry['unblocked_ms'])}")
	primitives = result["primitives"]
	for kind, values in primitives["ipc_round_trip_ms"].items():
		if values["calls"]:
			lines.append(f"  IPC round trip, {kind}: median {values['median']:.4f} ms, mean {values['mean']:.4f} ms over {values['calls']} calls")
	for name in ("channel_wake_ms", "deadline_wake_ms"):
		values = primitives[name]
		if values["wakes"]:
			lines.append(f"  {name.replace('_ms', '').replace('_', ' ')}: median {values['median']:.4f} ms, mean {values['mean']:.4f} ms over {values['wakes']} wakes")
	return "\n".join(lines)


def main(argv):
	import argparse

	parser = argparse.ArgumentParser(description="the frame account: one drain of the kernel's record buffer, joined into frames and terms")
	parser.add_argument("log", help="the serial log holding the tsc_hz anchor and the drain")
	parser.add_argument("--drain", type=int, default=0, help="which drain in the log, 1-based (default: the last)")
	parser.add_argument("--demo-report", help="a file holding the demo's output, whose `armed` line the account is checked against")
	parser.add_argument("--json", help="write the account as JSON to this path")
	args = parser.parse_args(argv)
	with open(args.log, "r", encoding="latin-1") as handle:
		tsc_hz, drains = parse_log(handle.read())
	if not drains:
		raise SystemExit("frame account: the log holds no drain (no `PERF-END` line)")
	index = args.drain - 1 if args.drain > 0 else len(drains) - 1
	if not 0 <= index < len(drains):
		raise SystemExit(f"frame account: the log holds {len(drains)} drain(s), not {args.drain}")
	report = None
	if args.demo_report:
		with open(args.demo_report, "r", encoding="latin-1") as handle:
			report = parse_demo_report(handle.read())
		if report is None:
			raise SystemExit("frame account: the demo's output has no `armed` line")
	try:
		result = account(drains[index], tsc_hz, report)
	except AccountError as error:
		raise SystemExit(f"frame account: {error}")
	print(render(result))
	if args.json:
		with open(args.json, "w") as handle:
			json.dump(result, handle, indent=1)
	return 0


if __name__ == "__main__":
	sys.exit(main(sys.argv[1:]))
