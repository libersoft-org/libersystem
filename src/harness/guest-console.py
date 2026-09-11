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
import socket
import sys
import time

# What the shell prints when it is ready for a line. The colour codes around it are part of the
# stream, so the match is on the text alone.
PROMPT = b"vol://system> "

# The line the shell prints once it is attached. Waiting for it rather than for the first prompt
# keeps a prompt printed during boot from being mistaken for readiness.
READY = b"shell attached"


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
	arguments = parser.parse_args()

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
	client.settimeout(0.5)
	with open(arguments.log, "wb") as log:
		while time.monotonic() < deadline:
			try:
				chunk = client.recv(4096)
			except socket.timeout:
				chunk = b""
			except OSError:
				break
			if chunk:
				log.write(chunk)
				log.flush()
				seen += chunk
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
