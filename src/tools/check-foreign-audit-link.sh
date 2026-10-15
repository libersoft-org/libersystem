#!/bin/bash
# Pass 2: the converged audit link, and the four answers it is the only thing that can give.
#
# WHY THIS IS THE GATE AND PASS 1 IS NOT. An archive does not resolve its external symbols, so pass 1
# measures a CANDIDATE surface - the per-object undefined closure, which over-states what a link
# needs because one upstream object may satisfy another's reference. What the substrate must PROVIDE
# is what a link actually resolved, and nothing else: a symbol built that no link requires is surface
# nobody asked for, and a symbol missing is a link that fails after every other question is answered.
#
# WHAT THE RECORDED INVENTORY IS CHECKED FOR, WITH OR WITHOUT THE UPSTREAM SOURCES:
#
#   EXACT SURFACE         every symbol the archive asks a system for was resolved from the substrate,
#                           and the substrate built nothing the link did not ask for. Both directions.
#   NO THREAD CREATION    the measured stop condition, over the finally admitted closure rather than
#                           over a candidate surface a mid-milestone pass saw.
#   NO TLS SEGMENT        the final ELF's program headers are where `PT_TLS` is answered, and this is
#                           the artifact that ships the answer rather than a prediction about it.
#   CONVERGED             two consecutive links produced the same resolved set. One link is a
#                           measurement; the fixed point is the gate.
#
# AND THE LIFECYCLE MECHANISMS ARE NAMED RATHER THAN COUNTED. A measured `.init_array` makes the
# positive lifecycle gates mandatory for exactly that mechanism, so the constructor and destructor are
# recorded BY NAME: a second one appearing is then a visible change rather than a number that moved.
#
# WITH THE SOURCES PRESENT the whole pass is re-run and the inventory must reproduce byte for byte.
# Without them that half is reported as not performed, never assumed.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

INVENTORY="src/foreign/INVENTORY-pass2.json"

fail() {
	echo "foreign-audit-link: $*" >&2
	exit 1
}

[[ -f "$INVENTORY" ]] || fail "$INVENTORY is missing - pass 2 has not been run"

python3 - "$INVENTORY" <<'PY' || exit 1
import json
import pathlib
import re
import sys

inventory = json.load(open(sys.argv[1]))
failures = []
targets = ("x86_64", "aarch64", "riscv64")

export = re.compile(r'^[ \t]*#\[(?:cfg_attr\(not\(test\), )?unsafe\(no_mangle\)\)?\]\s*\n(?:[ \t]*#\[[^\]]*\]\s*\n)*[ \t]*pub (?:unsafe )?(?:extern "C" )?(?:fn|static mut) (\w+)', re.M)
declared = set()
for crate in ("src/user/libs/foreign/abi/src", "src/user/libs/foreign/discovery/src"):
	for source in sorted(pathlib.Path(crate).glob("*.rs")):
		declared |= set(export.findall(source.read_text()))
declared.discard("__liber_stderr_stream")
# THE SUBSTRATE'S OWN CONTROL SURFACE IS NOT SOMETHING THE LINK RESOLVES. These two are how a LAUNCH
# hands the substrate what only a launch knows - its diagnostic sink, and which provider was bound
# into the closure - so nothing upstream references them, and the exact-surface rule below would
# report them as declared-and-unresolved in both directions. Named here, as they are in the
# facilities gate, so the exemption is one list and not two readings of the same fact.
declared -= {"liber_foreign_install_sink", "liber_foreign_install_icd"}

for target in targets:
	entry = inventory.get(target)
	if entry is None:
		failures.append(f"{target}: the inventory has no entry, so nothing about this target was measured")
		continue
	asked = set(entry["archive_surface"])
	resolved = set(entry["resolved_from_substrate"])
	# THE CLOSURE IS THE SUBSTRATE AND THE RUNTIME, and the split between them is a measurement rather
	# than a preference: `lsrt.lslib` owns the four memory functions in this image, so the link takes
	# them from there and a second definition in the substrate would be an export with two owners.
	# What must hold is that EVERY symbol the archive asks for came from somewhere in the closure.
	from_runtime = set(entry["resolved_from_runtime"])
	for symbol in sorted(asked - resolved - from_runtime):
		failures.append(f"{target}: <{symbol}> is asked for and was resolved by nothing in the closure")
	if entry["substrate_built_and_unrequired"]:
		failures.append(f"{target}: the substrate builds {entry['substrate_built_and_unrequired']} and the converged link asks for none of them")
	# THE DECLARED SURFACE IS THE ONE IN THE TREE, not the one the inventory remembers. A crate that
	# grew a symbol after the last pass would otherwise be checked against its own older self.
	for symbol in sorted(declared - resolved):
		failures.append(f"{target}: the substrate declares <{symbol}> and the converged link does not resolve it")
	for symbol in sorted(resolved - declared):
		failures.append(f"{target}: the converged link resolved <{symbol}> and no substrate crate declares it any more")
	# THE STOP CONDITIONS, SCOPE BY SCOPE. Reporting them together would say a condition was hit and
	# not WHERE, and the three scopes fail for different reasons: a member can carry what the link
	# never references, and an ELF can carry a segment no member asked for.
	for scope, found in entry["thread_creation_by_scope"].items():
		if found:
			failures.append(f"{target}: thread creation in {scope}: {found}")
	for scope in ("archive_members", "audit_linked_elf"):
		if entry["tls_by_scope"][scope]:
			failures.append(f"{target}: thread-local storage in {scope}: {entry['tls_by_scope'][scope]}")
	if entry["has_tls_segment"] or entry["tls_by_scope"]["elf_segment"]:
		failures.append(f"{target}: the audit-linked ELF carries a PT_TLS segment, which Variant B says it must not")
	if not entry.get("converged"):
		failures.append(f"{target}: the inventory does not record a fixed point")
	if entry["init_array"] != ["loader_init_library"] or entry["fini_array"] != ["loader_free_library"]:
		failures.append(f"{target}: the lifecycle entries moved - init {entry['init_array']}, fini {entry['fini_array']}")

# THE NEGOTIATED VERSION IS RECORDED BESIDE THE SYMBOLS, and it is the decision rather than whatever
# the last link happened to see: the substrate admits 2 through 6, the loader asks for more than that,
# so what a negotiation through the substrate can settle on is 2 through 6 - and the admitted set is
# the one the substrate declares NOW, read from its source the way the declared surface above is.
interface = inventory.get("_icd_interface")
if interface is None:
	failures.append("the inventory records no ICD interface version, so which version the substrate admits is an assumption rather than a derived fact")
else:
	version = pathlib.Path("src/user/libs/foreign/discovery/src/version.rs").read_text()
	declared_versions = [int(value) for value in re.search(r"pub const ADMITTED: \[u32; \d+\] = \[([^\]]*)\];", version).group(1).split(",")]
	if interface["substrate_admits"] != declared_versions:
		failures.append(f"the inventory records the substrate admitting {interface['substrate_admits']} and the substrate declares {declared_versions}")
	if interface["substrate_admits"] != [2, 3, 4, 5, 6] or (interface["negotiated_at_least"], interface["negotiated_at_most"]) != (2, 6):
		failures.append(f"the recorded interface range is {interface['negotiated_at_least']}..{interface['negotiated_at_most']} over {interface['substrate_admits']}, and the decision is 2..6")
	if interface["loader_asks_for"] <= interface["negotiated_at_most"]:
		failures.append(f"the pinned loader asks for {interface['loader_asks_for']}; the bound this records is only exercised if it asks for more than {interface['negotiated_at_most']}, so the inventory and the substrate's reason to bound no longer agree")
	if interface["exports_resolved"] != ["vk_icdGetInstanceProcAddr", "vk_icdNegotiateLoaderICDInterfaceVersion"] or interface["export_refused_by_name"] != "vk_icdGetPhysicalDeviceProcAddr":
		failures.append(f"the substrate resolves {interface['exports_resolved']} and refuses {interface['export_refused_by_name']}, and the closed surface is the two exports with the physical-device one refused")

shapes = {frozenset(inventory[target]["archive_surface"]) for target in targets if target in inventory}
if len(shapes) != 1:
	failures.append("the three targets ask for different symbols; one substrate cannot serve them")
sizes = {inventory[target]["elf_bytes"] for target in targets if target in inventory}
if len(sizes) != len(targets):
	failures.append("two targets produced the same ELF size, which is what copying one result into three rows looks like")

for failure in failures:
	print(f"foreign-audit-link: {failure}", file=sys.stderr)
raise SystemExit(1 if failures else 0)
PY

echo "foreign-audit-link: the recorded pass-2 inventory is a converged, exact surface with no threads and no TLS"

# THE ARCHIVES ARE WHAT THE LINK IS RE-RUN FROM: every tree it compiles is derived from them and the
# pinned patch series, so their absence - not a tree's - is what makes the second half impossible.
if ! compgen -G "$ROOT/.build/foreign/vulkan-loader-*.tar.gz" >/dev/null || ! compgen -G "$ROOT/.build/foreign/vulkan-headers-*.tar.gz" >/dev/null; then
	echo "foreign-audit-link: NOT PERFORMED: the pinned upstream archives are not under .build/foreign, so the link was not re-run"
	exit 0
fi
# THE SCAN IS PROVED ABLE TO SEE WHAT IT REPORTS ABSENT before its zeroes are believed. Two of this
# milestone's stop conditions ARE those zeroes, and an empty result is indistinguishable from a
# broken scan without this.
python3 "$HERE/foreign-audit-link.py" --self-test
python3 "$HERE/foreign-audit-link.py" --check
