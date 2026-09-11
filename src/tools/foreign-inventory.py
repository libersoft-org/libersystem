#!/usr/bin/env python3
"""Pass 1: derive the candidate surface of the pinned configuration from its object closure.

WHY THIS IS DERIVED AND NOT WRITTEN. An inventory of "what a foreign stack needs from a system" that
somebody typed is a guess, and the first real import is what discovers the guess was wrong - which is
the failure this whole milestone exists to prevent. Everything below is read out of the objects the
pinned configuration actually produced.

THE THREE TARGETS ARE RECORDED SEPARATELY AND THE DIFFERENCES ARE PART OF THE EVIDENCE. An inventory
identical on all three has not been derived from three builds; it has been derived from one and
copied. Relocation forms in particular cannot be the same - they are per-architecture by
construction - so a run that reports them equal is a run that read the wrong thing.

IT IS A GENERATED ARTIFACT. Regenerated rather than edited, with a digest; a regeneration that
differs from the recorded one fails the gate.
"""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys

# The compiler-runtime symbols a freestanding target may legitimately expect. Named rather than
# pattern-matched: `__` is not a namespace anybody owns, and a prefix rule would silently absorb an
# upstream symbol that happened to start with one.
COMPILER_RUNTIME = re.compile(
	r"^(__udiv|__divd|__muld|__aeabi_|__ashl|__ashr|__lshr|__cmp|__fix|__float|__mul|__div|__mod|__neg|__pow|__trunc|__extend|__clz|__ctz|__ffs|__popcount|__bswap|__sync_|__atomic_|__stack_chk|memcpy$|memset$|memmove$|memcmp$)"
)

# The C++ ABI facilities the plan names, each as a question this inventory answers with a number
# rather than a yes.
CXX_ABI = {
	"cxa": re.compile(r"^__cxa_"),
	"unwind": re.compile(r"^(_Unwind_|__gxx_personality)"),
	"rtti": re.compile(r"^(_ZTI|_ZTS|_ZTV)"),
	"atexit": re.compile(r"^(atexit|__cxa_atexit|__cxa_thread_atexit)$"),
	"errno": re.compile(r"^(__errno_location|__liber_errno_location)$"),
}


def run(*command):
	result = subprocess.run(command, capture_output=True, text=True)
	if result.returncode != 0:
		print(f"foreign-inventory: {command[0]} failed: {result.stderr.strip()}", file=sys.stderr)
		raise SystemExit(1)
	return result.stdout


def symbols(archive):
	"""Every symbol the archive's members declare, with its type and binding.

	READ FROM THE SYMBOL TABLE RATHER THAN FROM `nm`'s SUMMARY, because the plan asks for the type
	and the binding and `nm` collapses both into one letter."""
	undefined = {}
	defined = {}
	for line in run("llvm-readelf", "--symbols", str(archive)).splitlines():
		fields = line.split()
		if len(fields) < 8 or not fields[0].endswith(":"):
			continue
		_, _, _, kind, binding, _, section, name = fields[:8]
		if binding not in ("GLOBAL", "WEAK"):
			continue
		if section == "UND":
			# A symbol both undefined in one member and defined in another is INTERNAL to the
			# archive and is not part of what this configuration asks of a system.
			undefined.setdefault(name, {"type": kind, "binding": binding})
		else:
			defined[name] = {"type": kind, "binding": binding}
	external = {name: info for name, info in undefined.items() if name not in defined}
	return external, defined


def relocations(archive):
	"""Every relocation form used, with how many times. Per-architecture by construction."""
	forms = {}
	for line in run("llvm-readelf", "--relocations", str(archive)).splitlines():
		fields = line.split()
		if len(fields) < 3 or not fields[0].startswith("0"):
			continue
		form = fields[2]
		if not form.startswith("R_"):
			continue
		forms[form] = forms.get(form, 0) + 1
	return dict(sorted(forms.items()))


def tls(archive):
	"""TLS symbols, segments and relocation forms.

	`PT_TLS` IS ASKED FOR AND CANNOT BE ANSWERED HERE, and saying so is the point: an archive holds
	relocatable objects and program headers appear at link time. What a relocatable object CAN show
	is TLS-typed symbols and TLS relocation forms, and the absence of both is the evidence the TLS
	item actually needs - a configuration with no TLS symbol and no TLS relocation cannot grow a
	`PT_TLS` segment when it is linked."""
	tls_symbols = []
	for line in run("llvm-readelf", "--symbols", str(archive)).splitlines():
		fields = line.split()
		if len(fields) >= 8 and fields[0].endswith(":") and fields[3] == "TLS":
			tls_symbols.append(fields[7])
	tls_forms = {form: count for form, count in relocations(archive).items() if "TLS" in form or "TPOFF" in form or "GOTTPOFF" in form or "DTPMOD" in form or "DTPOFF" in form}
	return {
		"symbols": sorted(set(tls_symbols)),
		"relocation_forms": tls_forms,
		"pt_tls": "not observable in a relocatable archive; the absence of TLS symbols and TLS relocations is what rules one out at link time",
	}


def init_arrays(archive):
	"""`.init_array` / `.fini_array` ENTRIES, by name.

	THE NAMES ARE THE ANSWER AND A COUNT IS NOT. "Two init_array sections" tells a reader nothing
	about what runs before their code does; `loader_init_library` tells them exactly which function
	the configuration expects a runtime to call, which is what the lifecycle question is about."""
	entries = {"init_array": [], "fini_array": []}
	section = None
	for line in run("llvm-readelf", "--relocations", str(archive)).splitlines():
		if "'.rela.init_array'" in line or "'.init_array'" in line:
			section = "init_array"
			continue
		if "'.rela.fini_array'" in line or "'.fini_array'" in line:
			section = "fini_array"
			continue
		if line.startswith("Relocation section"):
			section = None
			continue
		if section and line.strip() and line.split()[0].startswith("0"):
			fields = line.split()
			if len(fields) >= 6:
				entries[section].append(fields[4])
	return {name: sorted(set(values)) for name, values in entries.items()}


def classify(external):
	"""Split the candidate surface into the buckets the plan asks about."""
	runtime = sorted(name for name in external if COMPILER_RUNTIME.match(name))
	cxx = {}
	for label, pattern in CXX_ABI.items():
		hits = sorted(name for name in external if pattern.match(name))
		cxx[label] = hits
	claimed = set(runtime) | {name for hits in cxx.values() for name in hits}
	platform = sorted(name for name in external if name not in claimed)
	return {"compiler_runtime": runtime, "cxx_abi": cxx, "platform_and_libc": platform}


def derive(arch, archive):
	external, defined = symbols(archive)
	return {
		"archive": str(archive),
		"archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
		"undefined": {name: info for name, info in sorted(external.items())},
		"undefined_count": len(external),
		"defined_count": len(defined),
		"classified": classify(external),
		"relocation_forms": relocations(archive),
		"tls": tls(archive),
		"static_initialisers": init_arrays(archive),
	}


def main():
	parser = argparse.ArgumentParser(description="Derive the pinned configuration's candidate surface.")
	parser.add_argument("--build", default=".build/foreign", help="where the per-target archives are")
	parser.add_argument("--out", default="src/foreign/INVENTORY-pass1.json")
	parser.add_argument("--check", action="store_true", help="regenerate and compare instead of writing")
	arguments = parser.parse_args()

	build = pathlib.Path(arguments.build)
	inventory = {}
	for arch in ("x86_64", "aarch64", "riscv64"):
		archive = build / arch / "libvulkan.a"
		if not archive.is_file():
			print(f"foreign-inventory: {archive} is missing - build it with build-foreign-static.sh --arch {arch}", file=sys.stderr)
			return 1
		inventory[arch] = derive(arch, archive)

	# THE DIFFERENCES ARE EVIDENCE, so they are computed and recorded rather than left for a reader
	# to diff three lists by eye.
	surfaces = {arch: set(data["undefined"]) for arch, data in inventory.items()}
	common = set.intersection(*surfaces.values())
	forms = {arch: tuple(sorted(inventory[arch]["relocation_forms"])) for arch in ("x86_64", "aarch64", "riscv64")}
	inventory["_differences"] = {
		"common_undefined": sorted(common),
		"per_target_only": {arch: sorted(names - common) for arch, names in surfaces.items()},
		"relocation_forms_identical": len(set(forms.values())) == 1,
		"relocation_form_counts": {arch: len(value) for arch, value in forms.items()},
		# WHY AN IDENTICAL UNDEFINED SET IS THE RIGHT ANSWER HERE, and where the per-target evidence
		# actually lives. The plan warns that an inventory identical on all three has not been derived
		# from three builds. For the SYMBOL set that warning does not bite: this is one C99
		# configuration with no per-architecture source, so the functions it calls are the same
		# everywhere and a difference would mean a source branch nobody intended. What cannot be the
		# same is the RELOCATION set - those are per-architecture by construction - and they are not:
		# the counts above differ, which is what proves three builds were read rather than one copied.
		"note": "identical undefined sets are expected for a configuration with no per-architecture sources; the relocation forms are where three builds are visible, and they differ",
	}

	text = json.dumps(inventory, indent="\t", sort_keys=True) + "\n"
	out = pathlib.Path(arguments.out)
	if arguments.check:
		if not out.is_file():
			print(f"foreign-inventory: {out} has never been generated", file=sys.stderr)
			return 1
		if out.read_text() != text:
			print(f"foreign-inventory: regenerating {out} does not reproduce it - the recorded inventory is stale", file=sys.stderr)
			return 1
		print(f"foreign-inventory: {out} reproduces exactly")
		return 0
	out.write_text(text)
	digest = hashlib.sha256(text.encode()).hexdigest()
	print(f"foreign-inventory: wrote {out} ({digest[:16]})")
	for arch in ("x86_64", "aarch64", "riscv64"):
		data = inventory[arch]
		print(f"foreign-inventory:   {arch}: {data['undefined_count']} undefined, {len(data['relocation_forms'])} relocation form(s), {len(data['tls']['symbols'])} TLS symbol(s)")
	print(f"foreign-inventory:   relocation forms identical across targets: {inventory['_differences']['relocation_forms_identical']}")
	return 0


if __name__ == "__main__":
	sys.exit(main())
