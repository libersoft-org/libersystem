#!/usr/bin/env python3
"""Drive the guest's shell over its serial console, and record everything it said.

WHY THIS EXISTS. Half of what the networking gates have to prove is guest-INITIATED: a name looked
up, a connection opened, a `ping -6` sent. The peer on the other end of the wire cannot provoke any
of it - it can only answer - so a gate built on the peer alone can prove what the guest ANSWERS and
nothing about what it ASKS. This types the asking.

IT IS A CONSOLE AND NOT AN AGENT. It waits for the prompt, sends one line, waits for the prompt
again, and writes the whole conversation to a log in the same shape `SERIAL=file:` would have - so
every assertion a gate already makes about the guest's own output keeps working unchanged. It makes
no decisions about what it sees.

THE SOCKET IS QEMU'S AND THIS CONNECTS TO IT, so the guest can be started first and this can retry
until the socket exists; the alternative races the guest's own startup for the file.
"""

import argparse
import os
import re
import socket
import sys
import time

# What the shell prints when it is ready for a line. The colour codes around it are part of the
# stream, so the match is on the text alone.
PROMPT = b"vol://system> "

# The line the shell prints once it is attached. Waiting for it rather than for the first prompt
# keeps a prompt printed during boot from being mistaken for readiness.
READY = b"shell attached"

# HOW LONG THE PROMPT MUST HAVE BEEN THERE BEFORE A LINE IS TYPED. The shell prints its prompt a moment
# before the console hands its input back to the line editor, and a line typed the instant the prompt
# appears can arrive split: its first character went to whatever held the input a moment earlier, and
# the rest reached the shell as a command nobody typed (`h` / `ttpd &` -> "unknown command"). Half a
# second of quiet after the prompt is far longer than that hand-back and far shorter than any line's
# budget.
SETTLE_SECONDS = 0.5

# A LINE OF THE GUEST'S OUTPUT, as a gate names one: colour codes and carriage returns taken off, so the line the
# shell printed compares equal to the text the gate wrote down.
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")


def output_lines(data):
	# Whole lines only: the last piece has no newline yet, and a line still being printed is not the line yet.
	return [ANSI.sub(b"", line).replace(b"\r", b"").strip() for line in data.split(b"\n")[:-1]]


def connect(path, deadline):
	"""Keep trying until QEMU has created the socket, or the deadline passes."""
	while time.monotonic() < deadline:
		if os.path.exists(path):
			client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
			try:
				client.connect(path)
				return client
			except OSError:
				client.close()
		time.sleep(0.1)
	return None


def main():
	parser = argparse.ArgumentParser(description="Type a script into the guest's shell and record the console.")
	parser.add_argument("--socket", required=True, help="the unix socket QEMU's serial is bound to")
	parser.add_argument("--log", required=True, help="where to write everything the guest said")
	parser.add_argument("--script", required=True, help="a file of shell lines, one per line, # for a comment")
	parser.add_argument("--seconds", type=float, default=120.0, help="how long to drive before giving up")
	# HOW LONG A COMMAND MAY TAKE. A connect that falls back between families takes minutes by
	# design, so a per-line limit that assumed a shell command is instant would fail the very rows
	# the timing is the subject of.
	parser.add_argument("--line-seconds", type=float, default=60.0, help="how long one line may take")
	# THE GATE'S OWN COMPLETION LINE, after which nothing it judges can still arrive. Recording stops once the line
	# has been printed - a whole line of output, so the shell echoing the command that prints it does not count -,
	# every command has been typed, and `--settle` seconds more have passed since the line, the window a gate with
	# negative checks keeps for a line arriving late. The line counts wherever it came: a fixture's last event can be
	# printed while the guest is still booting, before the shell takes the first command.
	parser.add_argument("--done", help="a line of output after which nothing the gate judges can arrive")
	parser.add_argument("--settle", type=float, default=0.0, help="how long to keep recording after the --done line")
	arguments = parser.parse_args()
	done = arguments.done.encode() if arguments.done else None

	commands = []
	with open(arguments.script, encoding="utf-8") as handle:
		for line in handle:
			line = line.strip()
			if line and not line.startswith("#"):
				commands.append(line)

	deadline = time.monotonic() + arguments.seconds
	client = connect(arguments.socket, deadline)
	if client is None:
		print("guest-console: the guest never created its serial socket", file=sys.stderr)
		return 1

	seen = bytearray()
	sent = 0
	ready = False
	prompt_at = None
	prompt_first = 0.0
	# What is printed, for the completion line - kept only back to its last line end; and when that line came.
	pending = bytearray()
	done_at = None
	client.settimeout(0.1)
	with open(arguments.log, "wb") as log:
		while time.monotonic() < deadline:
			try:
				chunk = client.recv(4096)
			except socket.timeout:
				chunk = b""
			except OSError:
				break
			else:
				# A CLOSED CONSOLE IS AN END, NOT AN EMPTY READ. `recv` answers b"" once QEMU has gone, and
				# the timeout above answers b"" too, so this spun on a dead socket until `--seconds` ran
				# out - 1200 s for a port gate whose guest had powered off in about 400. A guest that is
				# gone can say nothing more, so there is nothing left to record.
				if not chunk:
					log.write(b"\n# guest-console: the console closed\n")
					log.flush()
					break
			if done_at is not None and sent >= len(commands) and time.monotonic() - done_at >= arguments.settle:
				log.write(b"\n# guest-console: the completion line came, and the settle window after it has passed\n")
				log.flush()
				break
			if chunk:
				log.write(chunk)
				log.flush()
				seen += chunk
				if done is not None and done_at is None:
					pending += chunk
					if done in output_lines(pending):
						done_at = time.monotonic()
					del pending[: pending.rfind(b"\n") + 1]
				# Only the tail can contain a prompt, and an unbounded buffer on a chatty console is
				# a memory leak with a deadline.
				if len(seen) > 8192:
					del seen[:-4096]
			elif chunk == b"" and not ready and READY not in seen:
				continue
			if not ready:
				if READY not in seen:
					continue
				ready = True
				# The banner follows the attach line, so wait for the prompt after it rather than
				# typing into the middle of it.
				seen.clear()
				line_deadline = time.monotonic() + arguments.line_seconds
				continue
			if sent >= len(commands):
				# Everything was typed; keep recording until the deadline so a late line is captured.
				continue
			if PROMPT in seen:
				# Output after the prompt restarts the wait - the shell is still talking - but never past
				# four settles, so a console that logs without pause is still typed into.
				now = time.monotonic()
				if prompt_at is None:
					prompt_at, prompt_first = now, now
				elif chunk:
					prompt_at = now
				if now - prompt_at < SETTLE_SECONDS and now - prompt_first < 4 * SETTLE_SECONDS:
					continue
				prompt_at = None
				seen.clear()
				client.sendall(commands[sent].encode() + b"\n")
				log.write(b"\n# guest-console sent: " + commands[sent].encode() + b"\n")
				log.flush()
				sent += 1
				line_deadline = time.monotonic() + arguments.line_seconds
				continue
			if time.monotonic() > line_deadline:
				log.write(b"\n# guest-console: no prompt within the line budget\n")
				log.flush()
				line_deadline = time.monotonic() + arguments.line_seconds
				seen.clear()
	client.close()
	print(f"guest-console: typed {sent} of {len(commands)} line(s)", flush=True)
	return 0 if sent == len(commands) else 1


if __name__ == "__main__":
	sys.exit(main())
