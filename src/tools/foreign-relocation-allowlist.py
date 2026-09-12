"""Read the relocation allowlist from the one place that defines it.

The runtime loader and the packager both decide with `bootproto::elf::dynamic_relocation_kind`, so a
copy of the numbers anywhere else would be a second policy that agrees until it does not. The parse
is narrow and fails out loud: a shape it cannot read stops the gate rather than quietly admitting
everything.
"""

import re
import sys

source = open(sys.argv[1]).read()
body = re.search(r"pub const fn dynamic_relocation_kind\(machine: u16, relocation: u32\) -> Option<DynamicRelocationKind> \{(.*?)\n\}", source, re.S)
if body is None:
	print("the relocation policy is not in the shape this gate reads", file=sys.stderr)
	raise SystemExit(1)
machines = {"EM_X86_64": "x86_64", "EM_AARCH64": "aarch64", "EM_RISCV": "riscv64"}
found = {}
for machine, arms in re.findall(r"(EM_\w+) => match relocation \{(.*?)\n\t\t\},", body.group(1), re.S):
	numbers = []
	for arm in re.findall(r"^\s*([0-9 |]+) => Some", arms, re.M):
		numbers += [piece.strip() for piece in arm.split("|") if piece.strip()]
	found[machines[machine]] = numbers
if sorted(found) != ["aarch64", "riscv64", "x86_64"] or any(len(numbers) < 3 for numbers in found.values()):
	print(f"the relocation policy read as {found}, which is not three machines with their forms", file=sys.stderr)
	raise SystemExit(1)
for arch, numbers in sorted(found.items()):
	print(f"{arch} {' '.join(sorted(numbers, key=int))}")
