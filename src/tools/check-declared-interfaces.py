#!/usr/bin/env python3
# Every interface a manifest role names has to be one LSIDL actually defines.
#
# `services/manifest.toml` says which LSIDL interface each channel role speaks. The field is a
# REFERENCE - what the interface means is LSIDL's to say - and until this check existed nothing
# compared the two. Four of the twenty names in the file were wrong the day they were written:
# three roles claimed `liber:resources@1/resource` where the interface is called `resources`, and
# one claimed `liber:process@1/supervisor` for an interface defined in `liber:observability@1`.
#
# They were wrong harmlessly, because no generator reads the field yet. That is exactly the
# condition under which a declaration rots: it is read by people, who believe it. A name that
# cannot be resolved is a name that will mislead the migration that finally does resolve it.
#
# The comparison is textual on purpose. Parsing LSIDL properly is `lsidl-gen`'s job and this needs
# one fact from it - which `package/interface` pairs exist - so it reads the declarations rather
# than the generated bindings, which would make the check depend on codegen having run.
import os
import re
import sys
import tomllib

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
idl_dir = os.path.join(root, "idl")
manifest_path = os.path.join(root, "user", "services", "manifest.toml")

# `package liber:storage@1;` and `interface volume {`, both at the start of a line: anywhere else
# they are prose. The IDL files open with a comment block that talks about packages and interfaces
# in words, and a regex that searched the whole file picked the sentence over the declaration.
defined = set()
for entry in sorted(os.listdir(idl_dir)):
	if not entry.endswith(".lsidl"):
		continue
	text = open(os.path.join(idl_dir, entry), encoding="utf-8").read()
	package = re.search(r"^package\s+([^;\s]+)\s*;", text, re.M)
	if not package:
		print(f"check-declared-interfaces: {entry} declares no package", file=sys.stderr)
		sys.exit(1)
	for interface in re.finditer(r"^\s*interface\s+([a-z0-9\-]+)", text, re.M):
		defined.add(f"{package.group(1)}/{interface.group(1)}")

manifest = tomllib.load(open(manifest_path, "rb"))
failures = []
for service in manifest.get("services", []):
	for role in service.get("roles", []):
		name = role.get("interface", "")
		if not name or name in defined:
			continue
		# Say what it probably meant. A wrong package with a real interface name, or a real package
		# with a near-miss name, is the whole of what has gone wrong here so far, and a bare "not
		# found" leaves the reader to grep for the right spelling themselves.
		tail = name.rsplit("/", 1)[-1]
		near = sorted(other for other in defined if other.rsplit("/", 1)[-1] == tail)
		if not near:
			near = sorted(other for other in defined if other.rsplit("/", 1)[-1].startswith(tail) or tail.startswith(other.rsplit("/", 1)[-1]))
		hint = f" - did you mean {' or '.join(near)}?" if near else ""
		failures.append(f"services.{service['name']}.roles.{role['tag']}.interface: no such LSIDL interface {name}{hint}")

for failure in failures:
	print(f"check-declared-interfaces: {failure}", file=sys.stderr)
if failures:
	sys.exit(1)
print(f"check-declared-interfaces: {len(defined)} interfaces defined, every declared role reference resolves")
