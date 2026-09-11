#!/usr/bin/env python3
"""Was the second candidate started at the declared cap, or after the whole SYN schedule?

THE CAPTURE IS THE CLOCK. Both attempts are timestamped at the peer, on arrival, so the gap between
them is measured on the wire rather than asserted about a constant. A service without the cap runs
the first candidate's entire three-minute schedule before trying the second; one that raced them
starts the second immediately. Both are outside the band below.
"""

import sys

# THE BAND IS NOT THE CAP, AND THE DIFFERENCE IS THE POINT. The cap runs from the moment the first
# candidate is STARTED - which includes resolving its next hop - and what the capture can timestamp
# is the SYN that follows that resolution. So the measured gap is the cap MINUS the resolution, and
# a band centred on three seconds would fail a service that is behaving exactly as specified.
#
# What the two bounds rule out is what matters. Below the low bound the two attempts are a race, not
# a sequence. Above the high bound the first candidate was allowed to run its schedule, which is
# 183 seconds - so any upper bound in single figures separates the two hypotheses completely.
LOW, HIGH = 1.0, 10.0


def main():
	first = None
	second = None
	for line in open(sys.argv[1], encoding="utf-8"):
		fields = line.split()
		if not fields or not fields[0].replace(".", "", 1).isdigit():
			continue
		stamp = float(fields[0])
		if first is None and "black-holed tcp6" in line and "dport=80" in line:
			first = stamp
		if second is None and "saw tcp4" in line and "dport=80" in line and "flags=S " in line:
			second = stamp
	if first is None or second is None:
		print("check-fallback-timing: one of the two attempts never happened", file=sys.stderr)
		return 1
	gap = second - first
	if not (LOW <= gap <= HIGH):
		print(f"check-fallback-timing: the second candidate started {gap:.2f}s after the first, outside [{LOW}, {HIGH}]", file=sys.stderr)
		return 1
	print(f"ipv6-peer:   the second candidate started {gap:.2f}s after the first")
	return 0


if __name__ == "__main__":
	sys.exit(main())
