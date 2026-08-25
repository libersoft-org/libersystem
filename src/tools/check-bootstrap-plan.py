#!/usr/bin/env python3
# The hand-written bootstrap ladder and the generated role plan must say the same thing.
#
# The migration moves the wiring of every managed service out of `service_manager/bootstrap.rs` - where
# it is twenty-three `if name == b"..."` branches, each calling a bespoke function with the
# capabilities that service happens to need - and into `services/manifest.toml`, where it is data a
# generator turns into a plan. The migration is one service at a time, which means both descriptions
# exist at once for a while, and two descriptions of one fact is exactly what this milestone is for.
#
# So they are compared. For every service the ladder still wires by hand, the sequence of tags the
# ladder sends must equal the sequence of roles the manifest declares - same tags, same order.
# ORDER IS THE POINT: a receiver checks the tag of the next message rather than searching for one,
# so a role in the wrong position has already displaced every read after it. That is not a
# hypothetical - three programs once read their bootstrap out of order and the failure surfaced 170
# tests away, in an unrelated service.
#
# A service the ladder no longer wires is one that has been migrated; it is skipped here and covered
# by the boot tests instead. When the ladder is empty this check has nothing left to compare, which
# is the state M6 describes.
import os
import re
import sys
import tomllib

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ladder_path = os.path.join(root, "user", "services", "core", "src", "service_manager", "bootstrap.rs")
manifest_path = os.path.join(root, "user", "services", "manifest.toml")
rt_path = os.path.join(root, "user", "runtime", "rt", "src", "lib.rs")

source = open(ladder_path, encoding="utf-8").read()
caps = dict(re.findall(r'pub const (CAP_\w+): &\[u8\] = b"(\w+)";', open(rt_path, encoding="utf-8").read()))

# Every helper that puts a tagged message on the service's bootstrap channel. They differ in what
# they duplicate and with which rights; what matters here is only which tag goes out, and when.
SENDERS = r"send_blocking|send_privilege|send_caps|send_shell_cap|send_power|send_factory|serve_root"
STEP = re.compile(rf"\b(?:{SENDERS})\(\s*manager_side\s*,\s*([^,]+?)\s*,|\bfor\s+\w+\s+in\s+\[([^\]]+)\]|\b(bootstrap_\w+)\(|\b(send_ready)\(")

bodies = {}
for match in re.finditer(r"^(?:pub\(super\) )?unsafe fn (\w+)\(", source, re.M):
	name = match.group(1)
	following = re.search(r"^(?:pub\(super\) )?unsafe fn ", source[match.end() :], re.M)
	end = match.end() + (following.start() if following else len(source) - match.end())
	bodies[name] = source[match.start() : end]


def tags(function, depth=0, seen=()):
	out = []
	if function not in bodies or depth > 4 or function in seen:
		return out
	for match in STEP.finditer(bodies[function]):
		if match.group(3):
			out.extend(tags(match.group(3), depth + 1, seen + (function,)))
		elif match.group(4):
			# The terminator that ENDS a bootstrap sequence. It carries no capability and is not a
			# role in the ordinary sense, but it is a message in the order - and a receiver that
			# never sees it waits forever for a send nobody will make.
			out.append("READY")
		elif match.group(2):
			out.extend(part.strip() for part in match.group(2).split(",") if part.strip())
		else:
			out.append(match.group(1).strip())
	return out


# A tag reaches the wire as a constant, a byte string, or a buffer built in place - the last for the
# roles that carry a payload behind the tag, which this check compares by name and not by content.
def name_of(token, expected):
	token = token.strip()
	if token in caps:
		return caps[token]
	literal = re.match(r'b"(\w+)"', token)
	if literal:
		return literal.group(1)
	# `&buf[..15]`, `&request`, `&payload`, and the one conditional tag - a live volume or a block
	# device in the same position. Compared positionally against the declaration, which is the only
	# thing available: the bytes are assembled at run time.
	return expected


ladder = {}
for match in re.finditer(r'if name == b"(\w+)" && !\(?(.*?)\)?\s*\{', source):
	service = match.group(1)
	sequence = []
	for call in re.findall(r"\b(bootstrap_\w+|send_power|send_privilege)\(", match.group(2)):
		if call == "send_power":
			sequence.append("SYSPOWER")
		elif call == "send_privilege":
			sequence.append(None)  # the tag is an argument of the branch, filled in positionally
		else:
			sequence.extend(tags(call))
	ladder[service] = sequence

declared = {service["name"]: [role["tag"] for role in service.get("roles", [])] for service in tomllib.load(open(manifest_path, "rb"))["services"]}

mismatches = []
compared = 0
for service, sequence in sorted(ladder.items()):
	if service not in declared:
		mismatches.append((service, "the ladder wires a service the manifest does not declare", [], []))
		continue
	expected = declared[service]
	actual = [name_of(token, expected[index] if index < len(expected) else "?") if token is not None else (expected[index] if index < len(expected) else "?") for index, token in enumerate(sequence)]
	compared += 1
	if actual != expected:
		mismatches.append((service, "the ladder and the plan disagree", actual, expected))

# A SERVICE THE POLICY SAYS COMES BACK MUST BE ONE THE MECHANISM CAN BRING BACK.
#
# `restartable()` used to be a hand-written list of three names beside a manifest column naming the
# same three - two editable sources for one fact, inside the milestone that exists to remove those.
# The policy now reads the manifest, and the mechanism is still `relaunch_service`, which knows how
# to re-run one bootstrap per name. Nothing in the compiler joins them: a fourth `transparent` row
# would make the supervisor promise a restart it cannot perform, spend a restart budget, reap the
# endpoints and then fail - which looks like a crash-loop rather than a wiring mistake. So they are
# compared here, where the rest of this milestone's two-sided facts are compared.
supervisor = open(os.path.join(root, "user", "services", "core", "src", "service_manager.rs"), encoding="utf-8").read()
relaunch = re.search(r"unsafe fn relaunch_service\(.*?\n\t\t\};", supervisor, re.S)
if not relaunch:
	print("check-bootstrap-plan: cannot find relaunch_service's broker-root table", file=sys.stderr)
	sys.exit(1)
can_relaunch = set(re.findall(r'b"(\w+)" => &mut broker\.\w+', relaunch.group(0)))
declared_transparent = {service["name"] for service in tomllib.load(open(manifest_path, "rb"))["services"] if service.get("restart") == "transparent"}
if can_relaunch != declared_transparent:
	for name in sorted(declared_transparent - can_relaunch):
		print(f"{name}: the manifest says `transparent` but relaunch_service cannot re-run its bootstrap", file=sys.stderr)
	for name in sorted(can_relaunch - declared_transparent):
		print(f"{name}: relaunch_service can re-run its bootstrap but the manifest does not say `transparent`", file=sys.stderr)
	mismatches.append(("restartable", "the restart policy and the restart mechanism cover different services", sorted(can_relaunch), sorted(declared_transparent)))

for service, why, actual, expected in mismatches:
	print(f"{service}: {why}", file=sys.stderr)
	if actual or expected:
		print(f"  ladder {actual}", file=sys.stderr)
		print(f"  plan   {expected}", file=sys.stderr)

if mismatches:
	print(f"check-bootstrap-plan: {len(mismatches)} service(s) whose hand-written bootstrap disagrees with the declared plan", file=sys.stderr)
	sys.exit(1)

migrated = sorted(set(declared) - set(ladder))
print(f"check-bootstrap-plan: {compared} service(s) still wired by hand agree with the plan; {len(migrated)} migrated" + (f" ({', '.join(migrated)})" if migrated else ""))
print(f"check-bootstrap-plan: {len(declared_transparent)} service(s) declared restartable, and the supervisor can re-run every one of them")
