#!/usr/bin/env python3
"""Pass 2: the strict, converging audit link of the pinned configuration against the substrate.

WHAT PASS 1 COULD NOT ANSWER. An archive does not resolve its external symbols, so pass 1 measures a
CANDIDATE surface: the per-object undefined closure, which over-states what a link needs because one
upstream object may satisfy another's reference. Only a link decides. This is that link, and the
inventory it writes is the one the substrate's declared surface must EQUAL.

THE ROOT SET IS DECLARED, NOT DISCOVERED. Ordinary archive selection pulls only members reachable
from the consumer, so a link that converged would have proved something about whatever fixture
happened to call it - a member no fixture reaches is a member the surface never sees, and that is
exactly where an unmeasured relocation form or a TLS use would hide. The archive is therefore
admitted WHOLE, so member selection cannot narrow the closure below what the pinned configuration
contains, and the result is a fact about the loader rather than about a caller.

THE LINK IS STRICT. No host library paths, no default libraries, no unresolved symbols. A link that
silently pulled a host libc would measure this machine and not the substrate.

THE GATE IS THE FIXED POINT, NOT THE FIRST LINK. Removing or adding substrate symbols after one link
can itself change what the next one resolves, so the loop is: link, adjust the substrate to what the
link resolved, relink. Two consecutive links producing the same resolved set on all three targets is
what "converged" means, and this tool reports whether that holds rather than assuming it.

THE IDENTITY RECORDS BIND WHAT WAS ACTUALLY USED (2026-10-15). The audit-linked ELF and the two
quarantine consumers carry the v2 foreign record, and its language section is computed from the
inputs of the link that produced them - the flags and configuration the builder recorded, the
profile sysroot's tree, the objects that went into the link, by digest - rather than written as
prose or as the digest of a constant string. `--verify-records` recomputes every one of those fields
from the same inputs and compares, which is what the artifact gate runs.
"""

import argparse
import functools
import json
import pathlib
import hashlib
import re
import shutil
import subprocess
import sys
import tomllib

TARGETS = {
	# `force-unwind-tables=no` ON ALL THREE, AND IT IS NOT COSMETIC. Exception tables are a mechanism
	# this profile FORBIDS, and the Rust half emits them by default on aarch64 even when every C
	# object was compiled without them - which is what the artifact check found: an audit-linked ELF
	# carrying `.eh_frame` that no C source asked for. A forbidden mechanism gets the flags that stop
	# it being emitted as well as the check that catches it, and this is the flag for the Rust half.
	"x86_64": {"triple": "x86_64-unknown-none", "emulation": "elf_x86_64", "rustflags": "-C relocation-model=pic -C force-unwind-tables=no", "spec": "src/user/x86_64-unknown-none.json"},
	"aarch64": {"triple": "aarch64-unknown-none", "emulation": "aarch64elf", "rustflags": "-C relocation-model=pic -C force-unwind-tables=no", "spec": "aarch64-unknown-none"},
	"riscv64": {"triple": "riscv64gc-unknown-none-elf", "emulation": "elf64lriscv", "rustflags": "-C relocation-model=pic -C code-model=medium -C force-unwind-tables=no", "spec": "riscv64gc-unknown-none-elf"},
}

# The substrate crates, in link order. Both are `no_std` with no dependencies at all, which is what
# lets a host test build them; what they need from the runtime is the allocator and the panic paths,
# and those come from `lsrt.lslib` like they do for every other library in this image.
SUBSTRATE = ("foreign_abi", "foreign_discovery")

# THREAD CREATION IS A MEASURED STOP CONDITION, so the names are written down rather than pattern
# matched: a prefix rule would absorb a symbol that merely started the same way and report a stop
# condition nobody hit.
# THE TLS RELOCATION FORMS, by the pieces every architecture spells them with. A relocation is not a
# symbol, so this is a name rule rather than a list - and it is deliberately WIDE: a form nobody
# anticipated matching here costs a false refusal that a person reads, while a form that slips past
# costs a thread-local access nothing noticed.
TLS_RELOCATION = re.compile(r"TLS|TPOFF|TPREL|DTPMOD|DTPREL|TLSGD|TLSLD|TLSDESC|GOTTPOFF|INDNTPOFF")

THREAD_SYMBOLS = {
	"pthread_create", "pthread_join", "pthread_detach", "thrd_create", "thrd_join", "thrd_detach",
	"CreateThread", "_beginthread", "_beginthreadex", "clone", "clone3", "fork", "vfork",
}

# THE IDENTITY FIELDS THAT DESCRIBE WHAT PRODUCED AN ARTIFACT, which are what `--verify-records`
# recomputes. The provider and selection lines are not among them: they name OTHER artifacts by
# digest, and the staging and launch paths already compare those against the providers actually
# staged.
PRODUCER_FIELDS = (
	"source-sha256", "compiler", "compiler-sha256", "archiver", "archiver-sha256", "linker", "linker-sha256",
	"cflags", "sysroot-sha256", "configure-sha256", "objects-sha256", "patches-sha256", "licence",
)


def run(command, **kwargs):
	return subprocess.run(command, check=True, capture_output=True, text=True, **kwargs)


def first_line(text):
	return text.splitlines()[0] if text.splitlines() else ""


def digest_file(path):
	return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def digest_tree(path):
	"""Every file's own digest and path, in path order, hashed again - the lockfile's tree digest."""
	inner = hashlib.sha256()
	base = pathlib.Path(path)
	for entry in sorted(p for p in base.rglob("*") if p.is_file()):
		inner.update(hashlib.sha256(entry.read_bytes()).hexdigest().encode())
		inner.update(b"  ./" + str(entry.relative_to(base)).encode() + b"\n")
	return inner.hexdigest()


def digest_objects(paths):
	"""The objects that went into a link, in link order: each one's digest and its name, hashed again.

	BY NAME AND NOT BY PATH. Where a build directory lives is not what was linked, and a digest that
	moved with the checkout would bind the machine rather than the bytes."""
	return hashlib.sha256("".join(f"{digest_file(path)}  {pathlib.Path(path).name}\n" for path in paths).encode()).hexdigest()


def lockfile(root):
	return tomllib.load(open(root / "src/foreign/PIN-bootstrap.toml", "rb"))


@functools.cache
def rust_lld(root):
	"""The linker, found the way the image build finds it and held to the version the lockfile pins.

	FROM THE REPOSITORY ROOT, NOT FROM WHEREVER THIS RUNS. `rustc --print sysroot` answers for the
	toolchain the current directory selects, and a crate directory with its own `rust-toolchain.toml`
	selects another one, whose `rust-lld` is another linker. Run from the root it is the one the image
	build links every foreign artifact with - and the one the lockfile names."""
	sysroot = pathlib.Path(run(["rustc", "--print", "sysroot"], cwd=root).stdout.strip())
	lld = next(sysroot.rglob("rust-lld"))
	version = first_line(run([str(lld), "-flavor", "gnu", "--version"]).stdout)
	pinned = lockfile(root)["toolchain"]["linker_version"]
	if version != pinned:
		raise SystemExit(f"foreign-audit-link: {lld} is '{version}' and the lockfile pins '{pinned}'")
	return lld, version


def defined_symbols(path):
	out = run(["llvm-nm", "--defined-only", "--extern-only", str(path)]).stdout
	return {line.split()[-1] for line in out.splitlines() if line.strip() and not line.endswith(":")}


def undefined_symbols(path):
	out = run(["llvm-nm", "--undefined-only", "--extern-only", str(path)]).stdout
	return {line.split()[-1] for line in out.splitlines() if line.strip() and not line.endswith(":")}


def archive_surface(path):
	"""What the archive asks a SYSTEM for: undefined in some member and defined in none of them."""
	return undefined_symbols(path) - defined_symbols(path)


def program_headers(path):
	out = run(["llvm-readelf", "-l", "--wide", str(path)]).stdout
	kinds = []
	for line in out.splitlines():
		parts = line.split()
		if len(parts) >= 2 and parts[0].isupper() and parts[1].startswith("0x"):
			kinds.append(parts[0])
	return kinds


def section_geometry(path, section):
	"""The section's link-time address and its size, or (0, 0).

	READ BY NAME AND NOT BY COLUMN NUMBER. `llvm-readelf` prints the index as `[ 8]` or `[10]`, which
	splits into two tokens or one, so every column after it moves. Indexing from the section NAME is
	the only stable way to read this table - and reading the wrong column is not a crash: it returned
	the file OFFSET as the size, which made the range below match every relocation in the file."""
	out = run(["llvm-readelf", "-S", "--wide", str(path)]).stdout
	for line in out.splitlines():
		fields = line.split()
		if section in fields:
			at = fields.index(section)
			return int(fields[at + 2], 16), int(fields[at + 4], 16)
	return 0, 0


def lifecycle_entries(path, section):
	"""The functions an `.init_array` or `.fini_array` slot points at, by name.

	A COUNT IS NOT A LIFECYCLE. "One constructor" says nothing about what runs; the gate this feeds
	has to name the function so a second one appearing is a visible change rather than a number."""
	address, size = section_geometry(path, section)
	if size == 0:
		return []
	targets = []
	relocations = run(["llvm-readobj", "--relocations", str(path)]).stdout
	for line in relocations.splitlines():
		parts = line.split()
		if len(parts) >= 4 and parts[0].startswith("0x"):
			where = int(parts[0], 16)
			if address <= where < address + size:
				targets.append(int(parts[-1], 16))
	names = {}
	for line in run(["llvm-nm", "--defined-only", str(path)]).stdout.splitlines():
		fields = line.split()
		if len(fields) == 3:
			names[int(fields[0], 16)] = fields[2]
	return sorted(names.get(target, f"0x{target:x}") for target in targets)


def substrate_rlibs(root, settings):
	"""Where the substrate's two archives are, once built. Named so a check can find them unbuilt."""
	deps = root / ".build/cargo/user" / settings["triple"] / "release"
	return [deps / f"lib{crate}.rlib" for crate in SUBSTRATE]


def build_substrate(root, arch, settings):
	for crate in ("abi", "discovery"):
		directory = root / "src/user/libs/foreign" / crate
		# THE TARGET IS A JSON SPEC ON ONE OF THE THREE and a triple on the other two, exactly as the
		# image build has it: x86_64 needs a custom spec for its hard-float ABI. The spec path is
		# absolute because cargo runs in the crate directory, not at the root.
		spec = str(root / settings["spec"]) if settings["spec"].endswith(".json") else settings["spec"]
		command = ["cargo", "build", "--quiet", "--target", spec, "-Z", "build-std=core,alloc,compiler_builtins", "-Z", "build-std-features=compiler-builtins-mem", "--release"]
		if settings["spec"].endswith(".json"):
			command.append("-Zjson-target-spec")
		run(command, cwd=directory, env={**__import__("os").environ, "RUSTFLAGS": settings["rustflags"]})
	return substrate_rlibs(root, settings)


def builder_configuration(out_dir, arch):
	"""What the builder recorded it compiled: the text, and its fields by name."""
	path = out_dir / arch / "configuration"
	if not path.is_file():
		raise SystemExit(f"foreign-audit-link: {arch}: {path} is missing - the archive was not built by the current builder; run this tool without --verify-records")
	text = path.read_text()
	return text, dict(line.split("=", 1) for line in text.splitlines() if "=" in line)


def runtime_of(root, settings):
	return root / ".build/image" / settings["triple"] / "lib/runtime/lsrt.lslib"


def start_of(root, settings):
	return root / ".build/state" / f"exe-start-{settings['triple']}.o"


def described(arguments):
	"""A link's arguments as its configuration: every path by its name, so the checkout does not show."""
	return " ".join(argument.name if isinstance(argument, pathlib.Path) else argument for argument in arguments)


def audit_link_arguments(settings, archive, rlibs, runtime):
	"""The audit link's flags and inputs, without the linker and the outputs. ONE list, which both
	the link and its identity record are made from."""
	return [
		"-flavor", "gnu", "-m", settings["emulation"], "-shared", "--hash-style=sysv", "-Bsymbolic",
		# THE THREE THAT MAKE IT STRICT: nothing unresolved, nothing unresolved behind a shared
		# library either, and no default search path or default library anywhere.
		"--no-undefined", "--no-allow-shlib-undefined", "-nostdlib",
		"--whole-archive", archive, "--no-whole-archive",
		*rlibs, runtime,
		"-soname", "audit-vulkan.lslib",
	]


def quarantine_link_arguments(settings, start, object_file, archive, rlibs, runtime):
	"""The quarantine consumer's flags and inputs, without the linker and the output."""
	loader = ["--whole-archive", archive, "--no-whole-archive"] if archive is not None else []
	return [
		"-flavor", "gnu", "-m", settings["emulation"], "-pie", "--no-dynamic-linker", "--hash-style=sysv",
		"--gc-sections", "--build-id=none", "-e", "_start", start, object_file,
		*loader,
		*rlibs, runtime,
		"--no-allow-shlib-undefined", "-z", "undefs",
	]


def patch_series_digest(root):
	patches = sorted((root / "src/foreign/patches").glob("*.patch"))
	return hashlib.sha256("".join(f"{path.name}\n{digest_file(path)}\n" for path in patches).encode()).hexdigest()


def producer_lines(root, cflags, configure, objects, licence):
	"""The foreign language section, every line of it computed from what was used.

	THE THREE TOOLS BY VERSION AND BY CONTENT, the flags they were given, the profile sysroot by its
	tree digest - the one the derived part of the lockfile pins - the configure inputs and the final
	objects by digest, the patch series, and the licence."""
	lld, linker_line = rust_lld(root)
	return [
		"language=foreign",
		f"compiler={first_line(run(['clang', '--version']).stdout)}",
		f"compiler-sha256={digest_file(shutil.which('clang'))}",
		f"archiver={first_line(run(['llvm-ar', '--version']).stdout)}",
		f"archiver-sha256={digest_file(shutil.which('llvm-ar'))}",
		f"linker={linker_line}",
		f"linker-sha256={digest_file(lld)}",
		f"cflags={cflags}",
		f"sysroot-sha256={digest_tree(root / 'src/foreign/profile-sysroot')}",
		f"configure-sha256={configure}",
		f"objects-sha256={objects}",
		f"patches-sha256={patch_series_digest(root)}",
		f"licence={licence}",
	]


def audit_source_digest(root):
	"""The audit-linked artifact's SOURCE: the two pinned upstream archives, by digest."""
	upstream = lockfile(root)["upstream"]
	archives = [root / ".build/foreign" / f"{name}-{entry['revision']}.tar.gz" for name, entry in sorted(upstream.items())]
	return hashlib.sha256("".join(f"{digest_file(path)}  {path.name}\n" for path in archives).encode()).hexdigest()


def audit_record_fields(root, arch, settings, out_dir, archive, rlibs):
	"""The audit-linked artifact's source digest and language section, from what its link used."""
	configuration, fields = builder_configuration(out_dir, arch)
	arguments = audit_link_arguments(settings, archive, rlibs, runtime_of(root, settings))
	# WHAT SELECTED WHAT WAS BUILT: the builder's configuration - tree, patch series, sysroot, target,
	# defines, sources - the Rust half's flags, the substrate crates, and the link itself.
	configure = hashlib.sha256((configuration + f"rustflags={settings['rustflags']}\nsubstrate={' '.join(SUBSTRATE)}\nlink={described(arguments)}\n").encode()).hexdigest()
	cflags = f"clang: {fields['cflags']}; rustc: {settings['rustflags']}"
	return [f"source-sha256={audit_source_digest(root)}", *producer_lines(root, cflags, configure, digest_objects([archive, *rlibs]), "Apache-2.0")]


def identity_record(root, arch, settings, out_dir, archive, rlibs):
	"""The audit-linked artifact's own identity record, in the same v2 format every artifact carries.

	WHY THE QUARANTINE ARTIFACT HAS ONE AT ALL. It is required to pass the generic identity and
	provider checks as a FILE, before and independently of any staging - and an artifact with no
	record fails those checks for the most uninteresting possible reason. It also makes the artifact
	say what produced it, which for this one is the whole point: the pinned upstream under its own
	licence, this milestone's patch series, the profile sysroot and the three tools.

	`language=foreign` AND THE UPSTREAM'S OWN LICENCE. The record is not a claim that this project
	wrote the code; `licence` is the term the pinned upstream is taken under, which is what has to
	travel with a binary rather than beside it."""
	source, *language = audit_record_fields(root, arch, settings, out_dir, archive, rlibs)
	lines = [
		"format=liber-image-identity-v2",
		"kind=library",
		"artifact=audit-vulkan",
		"package=vulkan-loader",
		source,
		f"target={settings['triple']}",
		"profile=release",
		*language,
	]
	# THE PROVIDER LINE, BECAUSE THE ARTIFACT HAS AN EDGE. It links against `lsrt.lslib` for the
	# allocator shims and the panic paths, so its `DT_NEEDED` list names one provider - and the rule
	# every artifact in this system is held to is that the record ACCOUNTS for exactly that set. A
	# record that left the edge out would be refused by the same check that refuses a forged one,
	# which is the point of running the generic checks on this file at all.
	runtime = runtime_of(root, settings)
	if runtime.is_file():
		note = pathlib.Path(str(runtime) + ".identity")
		run(["llvm-objcopy", "--dump-section", f".note.liber.identity={note}", str(runtime), "/dev/null"])
		# The note is a twenty-byte header followed by the record; the digest is over the record.
		blob = note.read_bytes()
		note.unlink()
		length = int.from_bytes(blob[4:8], "little")
		lines.append(f"provider=lsrt:{hashlib.sha256(blob[20:20 + length]).hexdigest()}")
	return ("\n".join(lines) + "\n").encode()


def link(root, arch, settings, archive, rlibs, out, link_map):
	lld, _ = rust_lld(root)
	arguments = audit_link_arguments(settings, archive, rlibs, runtime_of(root, settings))
	subprocess.run([str(lld), *map(str, arguments), "-Map", str(link_map), "-o", str(out)], check=True)


def member_scan(path):
	"""Every `ET_REL` member of the archive - or the ELF itself - scanned for TLS and thread creation.

	THE SCOPE IS THREE THINGS AND THIS COVERS TWO OF THEM. The stop conditions this pass answers are
	defined over the FINALLY ADMITTED closure - every archive member that survives selection, the
	converged resolved set, and the audit-linked ELF - because each can carry something the other two
	do not: a member can hold a thread-local symbol the link never references, and an ELF can carry a
	segment no member asked for. `stop_conditions` below puts the three together.

	WHOLE-ARCHIVE ADMISSION IS WHY EVERY MEMBER COUNTS. Nothing is selected away, so "survives
	selection" is every member there is.

	TLS IS DECIDED BY THE SECTION, NOT BY THE NAME. `--format=sysv` names the section a symbol lives
	in, and a thread-local one lives in `.tbss` or `.tdata`; a name rule would both miss a
	differently spelled one and invent matches. The scan is proved able to SEE one by `--self-test`,
	because an empty result and a broken scan look identical from here."""
	tls = []
	threads = []
	# THE RELOCATION FORMS TOO, which a symbol scan cannot see. A thread-local ACCESS is a relocation
	# against the thread pointer, and an object can carry one for a variable defined elsewhere - so a
	# scan that looked only for `.tbss` and `.tdata` would report absence in exactly the object that
	# uses somebody else's. Variant B's proof is over all three: the segment, the symbols and the
	# relocation forms.
	for line in run(["llvm-readobj", "--relocations", str(path)]).stdout.splitlines():
		fields = line.split()
		if len(fields) >= 2 and fields[1].startswith("R_") and TLS_RELOCATION.search(fields[1]):
			tls.append(fields[1])
	for line in run(["llvm-nm", "--format=sysv", str(path)]).stdout.splitlines():
		if "|" not in line:
			continue
		fields = [field.strip() for field in line.split("|")]
		name = fields[0].split("[")[0].strip()
		if len(fields) >= 7 and fields[6].split()[0] in (".tbss", ".tdata"):
			tls.append(name)
		if name in THREAD_SYMBOLS:
			threads.append(name)
	return sorted(set(tls)), sorted(set(threads))


def stop_conditions(archive, elf):
	"""Both measured stop conditions, over all three scopes of the finally admitted closure.

	ONE FUNCTION FOR THE MEASUREMENT AND FOR ITS SELF-TEST, so what the self-test shows detecting is
	what the inventory is computed by. The converged resolved set is what the archive asks a system
	for together with what the linked ELF still imports - the set the link was resolving."""
	surface = archive_surface(archive)
	member_tls, member_threads = member_scan(archive)
	elf_tls, elf_threads = member_scan(elf)
	return {
		"thread_creation_by_scope": {
			"archive_members": member_threads,
			"converged_resolved_set": sorted((surface | undefined_symbols(elf)) & THREAD_SYMBOLS),
			"audit_linked_elf": elf_threads,
		},
		"tls_by_scope": {
			"archive_members": member_tls,
			"audit_linked_elf": elf_tls,
			"elf_segment": "TLS" in program_headers(elf),
		},
	}


def attach_identity(elf, record):
	"""Put the record into the artifact as the same allocated ELF note every artifact carries.

	THE NOTE IS THE FORMAT AND NOT A FILE BESIDE IT, which is the whole of why identity travels: a
	record in a sidecar is a record that can be separated from the bytes it describes. The layout is
	`namesz` (6, for "LIBER\0"), `descsz`, type 1, the padded name, the record, and its padding to a
	four-byte boundary - the same twenty-byte header the image build writes."""
	note = elf.with_suffix(".note")
	header = (6).to_bytes(4, "little") + len(record).to_bytes(4, "little") + (1).to_bytes(4, "little") + b"LIBER\0\0\0"
	padding = b"\0" * ((4 - len(record) % 4) % 4)
	note.write_bytes(header + record + padding)
	run(["llvm-objcopy", "--add-section", f".note.liber.identity={note}", "--set-section-flags", ".note.liber.identity=alloc,readonly", str(elf)])
	note.unlink()


def embedded_record(elf):
	"""The identity record an artifact carries, as text."""
	blob = subprocess.run(["llvm-objcopy", "--dump-section", ".note.liber.identity=/dev/stdout", str(elf), "/dev/null"], check=True, capture_output=True).stdout
	length = int.from_bytes(blob[4:8], "little")
	return blob[20:20 + length].decode()


def measure(root, arch, settings, out_dir):
	archive = out_dir / arch / "libvulkan.a"
	run([str(root / "src/tools/build-foreign-static.sh"), "--arch", arch, "--sysroot", "profile", "--ported", "--out", str(out_dir / arch)])
	rlibs = build_substrate(root, arch, settings)
	elf = out_dir / arch / "audit-vulkan.lslib"
	link_map = out_dir / arch / "link.map"
	link(root, arch, settings, archive, rlibs, elf, link_map)
	attach_identity(elf, identity_record(root, arch, settings, out_dir, archive, rlibs))

	surface = archive_surface(archive)
	substrate_exports = set()
	for rlib in rlibs:
		substrate_exports |= defined_symbols(rlib)
	# WHAT THE SUBSTRATE ANSWERED, and what it built that nothing asked for. The second set is the
	# one the exact-surface rule is about: a symbol nothing requires is not built.
	resolved = sorted(surface & substrate_exports)
	# THE SUBSTRATE'S OWN CONTROL SURFACE IS NOT SOMETHING THE LINK ASKS FOR, and it is named rather
	# than pattern-excluded: these are how a LAUNCH hands the substrate what only a launch knows -
	# its diagnostic sink, and which provider was bound into the closure. Nothing upstream references
	# them, which is exactly why the rule below would otherwise report them as built and unrequired.
	interface = {"liber_foreign_install_sink", "liber_foreign_install_icd"}
	unneeded = sorted(symbol for symbol in substrate_exports - surface - interface if not symbol.startswith("_R") and not symbol.startswith("__liber_"))
	# WHAT ONLY THE LINK PRODUCED: references the final ELF still carries, which are the runtime's.
	link_only = sorted(undefined_symbols(elf))
	headers = program_headers(elf)
	# THE QUARANTINE CONSUMER, linked beside the artifact it consumes. It is what the guest gate
	# launches, and it is built here for the reason the whole pass is: nothing else can link it.
	for name, with_loader, licence in QUARANTINE_CONSUMERS:
		consumer = build_quarantine(root, arch, settings, out_dir, archive, rlibs, name, with_loader)
		if consumer is None:
			continue
		attach_identity(consumer, quarantine_identity(root, arch, settings, out_dir, archive if with_loader else None, rlibs, ["lsrt"], [("vulkan-icd", ["icdprobe"])], name, licence))
		# THE UNDEFINED SET IS EXACTLY WHAT THE SLOT AND THE RUNTIME OWE, and nothing else. `-z undefs`
		# let the link finish with the two ICD exports unresolved; this is what keeps that from being
		# a hole, because an artifact carrying a third undefined symbol is one the launch would refuse
		# with nothing having said why.
		slot = {"vk_icdNegotiateLoaderICDInterfaceVersion", "vk_icdGetInstanceProcAddr"}
		unresolved = set(undefined_symbols(consumer))
		# WHAT THE RUNTIME OWES IS WHAT THE RUNTIME DEFINES, read off the staged provider rather than
		# listed here. A list would be a second description of `lsrt`'s export table, and the one
		# thing this check exists to notice is a symbol NOBODY owes - which a stale list would hide.
		runtime_owned = defined_symbols(runtime_of(root, settings))
		surprising = sorted(unresolved - slot - runtime_owned)
		if surprising:
			print(f"foreign-audit-link: {arch}: {name} carries undefined symbols nothing owes it: {surprising}", file=sys.stderr)
			raise SystemExit(1)

	scopes = stop_conditions(archive, elf)
	threads = scopes["thread_creation_by_scope"]
	return {
		"archive": str(archive.relative_to(root)) if archive.is_relative_to(root) else str(archive),
		"archive_surface": sorted(surface),
		"resolved_from_substrate": resolved,
		"substrate_built_and_unrequired": unneeded,
		"resolved_from_runtime": link_only,
		"program_headers": sorted(set(headers)),
		"has_tls_segment": "TLS" in headers,
		"init_array": lifecycle_entries(elf, ".init_array"),
		"fini_array": lifecycle_entries(elf, ".fini_array"),
		# THE STOP CONDITIONS OVER ALL THREE SCOPES, separately, so a gate can say WHERE one appeared.
		"thread_creation_symbols": sorted(set(threads["archive_members"]) | set(threads["converged_resolved_set"]) | set(threads["audit_linked_elf"])),
		"thread_creation_by_scope": threads,
		"tls_by_scope": scopes["tls_by_scope"],
		"elf_bytes": elf.stat().st_size,
	}


def icd_interface(root):
	"""Which Loader/Driver interface versions a negotiation through this substrate can settle on.

	A DERIVED FACT AND NOT A RESTATED ONE. The loader's side is read out of the pinned headers - the
	version it asks every driver for, and the lowest it would accept on its own - and the substrate's
	side out of the substrate's own declaration of what it admits. What a negotiation can agree on is
	then the intersection: the loader asks for more than the substrate admits, and the substrate's
	bounded entry point is what brings the offer down; nothing below the substrate's floor is agreed,
	whatever the loader would accept."""
	header = (root / ".build/foreign/src-headers/include/vulkan/vk_icd.h").read_text()
	asks = int(re.search(r"#define CURRENT_LOADER_ICD_INTERFACE_VERSION (\d+)", header).group(1))
	accepts = int(re.search(r"#define MIN_SUPPORTED_LOADER_ICD_INTERFACE_VERSION (\d+)", header).group(1))
	version = (root / "src/user/libs/foreign/discovery/src/version.rs").read_text()
	admitted = [int(value) for value in re.search(r"pub const ADMITTED: \[u32; \d+\] = \[([^\]]*)\];", version).group(1).split(",")]
	library = (root / "src/user/libs/foreign/discovery/src/library.rs").read_text()

	def constant(name):
		return re.search(rf'pub const {name}: &\[u8\] = b"([^"]*)";', library).group(1)

	return {
		"loader_asks_for": asks,
		"loader_accepts_down_to": accepts,
		"substrate_admits": admitted,
		"negotiated_at_most": min(asks, max(admitted)),
		"negotiated_at_least": max(accepts, min(admitted)),
		"exports_resolved": sorted([constant("NEGOTIATE_SYMBOL"), constant("GET_INSTANCE_PROC_ADDR_SYMBOL")]),
		"export_refused_by_name": constant("PHYSICAL_DEVICE_SYMBOL"),
	}


def self_test(root, work):
	"""Prove the scans can SEE what they report absent, in each of the three scopes they report over.

	AN EMPTY RESULT AND A BROKEN SCAN LOOK IDENTICAL from the outside, and this pass turns an empty
	result into two of the milestone's stop conditions. So a fixture carrying exactly what the scans
	look for - a thread-local variable and a reference to thread creation - is put into EACH scope:
	an archive member, the converged resolved set, and a LINKED ELF, through the same function the
	inventory is computed by. A run where any scope misses it is a run whose zeroes mean nothing.

	AND A CLEAN FIXTURE THROUGH THE SAME SCOPES MUST COME BACK EMPTY, because a scan that reported
	everything would pass the half above just as well.
	"""
	work.mkdir(parents=True, exist_ok=True)
	lld, _ = rust_lld(root)
	fixtures = {
		"carrying": (
			"/* Not built into anything. It exists so the scans can be shown finding what they report\n"
			"   absent, in each of the three forms they look for: a thread-local DEFINITION, which lives in\n"
			"   .tdata; a thread-local ACCESS, which is a relocation and is what an object using somebody\n"
			"   else-s variable carries instead; and a reference to thread creation. */\n"
			"__thread int liber_audit_thread_local = 1;\n"
			"extern __thread int liber_audit_elsewhere;\n"
			"int liber_audit_reads_a_thread_local(void) { return liber_audit_elsewhere + liber_audit_thread_local; }\n"
			"extern int pthread_create(void *, const void *, void *(*)(void *), void *);\n"
			"int liber_audit_starts_a_thread(void) { return pthread_create(0, 0, 0, 0); }\n"
		),
		"clean": (
			"/* The control: the same shapes with nothing thread-local and nothing that starts a thread. */\n"
			"int liber_audit_plain = 1;\n"
			"extern int liber_audit_plain_elsewhere;\n"
			"int liber_audit_reads_plainly(void) { return liber_audit_plain_elsewhere + liber_audit_plain; }\n"
		),
	}
	scopes = {}
	for label, text in fixtures.items():
		source = work / f"stop-condition-{label}.c"
		source.write_text(text)
		object_file = work / f"stop-condition-{label}.o"
		run(["clang", "--target=x86_64-unknown-none-elf", "-ffreestanding", "-nostdlibinc", "-fPIC", "-O0", "-c", str(source), "-o", str(object_file)])
		archive = work / f"stop-condition-{label}.a"
		archive.unlink(missing_ok=True)
		run(["llvm-ar", "crsD", str(archive), str(object_file)])
		# THE LINKED SCOPE IS A REAL LINK, by the pinned linker, of the fixture into a shared object -
		# which is what carries a `PT_TLS` segment and the dynamic relocations an object does not. What
		# it cannot resolve stays imported, as the audit-linked ELF's runtime references do.
		elf = work / f"stop-condition-{label}.so"
		run([str(lld), "-flavor", "gnu", "-m", "elf_x86_64", "-shared", "--hash-style=sysv", "-nostdlib", "--whole-archive", str(archive), "--no-whole-archive", "-o", str(elf)])
		scopes[label] = stop_conditions(archive, elf)

	failures = []
	carrying, clean = scopes["carrying"], scopes["clean"]
	for scope in ("archive_members", "converged_resolved_set", "audit_linked_elf"):
		if "pthread_create" not in carrying["thread_creation_by_scope"][scope]:
			failures.append(f"the thread scan did not see a thread-creation reference in the {scope} scope")
		if clean["thread_creation_by_scope"][scope]:
			failures.append(f"the thread scan reported {clean['thread_creation_by_scope'][scope]} in the {scope} scope of a fixture that has none")
	for scope in ("archive_members", "audit_linked_elf"):
		found = carrying["tls_by_scope"][scope]
		if "liber_audit_thread_local" not in found:
			failures.append(f"the TLS scan did not see a thread-local DEFINITION in the {scope} scope")
		if not any(TLS_RELOCATION.search(entry) for entry in found):
			failures.append(f"the TLS scan did not see a thread-local ACCESS - a relocation - in the {scope} scope")
		if clean["tls_by_scope"][scope]:
			failures.append(f"the TLS scan reported {clean['tls_by_scope'][scope]} in the {scope} scope of a fixture that has none")
	if not carrying["tls_by_scope"]["elf_segment"]:
		failures.append("the segment scan did not see the PT_TLS segment of a linked ELF that has one")
	if clean["tls_by_scope"]["elf_segment"]:
		failures.append("the segment scan reported PT_TLS in a linked ELF that has none")
	for failure in failures:
		print(f"foreign-audit-link: {failure}", file=sys.stderr)
	if failures:
		return 1
	print("foreign-audit-link: the scans find an injected thread-local and an injected thread creation in all three scopes - archive members, the converged resolved set and a linked ELF - and nothing in a clean fixture")
	return 0


# THE QUARANTINE CONSUMERS. Two, because they answer different questions: one runs the ported LOADER
# against the ICD bound into its closure, the other calls every admitted FACILITY once and checks the
# answer. Neither can be built by the ordinary image build - the first links an upstream this tree
# does not carry, and the second links a substrate whose C names every staged library would collide
# with - so both are linked here and staged only into the development image.
#
# ONLY THE LOADER CONSUMER LINKS THE ARCHIVE. `abiprobe` needs the substrate and not the loader, and
# linking the loader into it would put an Apache-2.0 closure inside an artifact that carries none.
QUARANTINE_CONSUMERS = (("vkprobe", True, "Apache-2.0"), ("abiprobe", False, "project"))


def build_quarantine(root, arch, settings, out_dir, archive, rlibs, name, with_loader):
	"""Link the quarantine consumer: this tree's own program, against the audit-linked loader.

	WHY IT IS BUILT HERE AND NOT BY THE IMAGE BUILD. Its object is ordinary - one more binary in the
	tools crate, compiled by the same script every other consumer uses - and its LINK is not: it
	resolves the loader's Vulkan entry points out of an archive built from an upstream that is
	deliberately not in this tree. The image build cannot do that on a machine that has not fetched
	the upstream, and it must not need to; so the link happens here, beside the audit that needs it,
	and the image build STAGES what it finds.

	THE SLOT SYMBOLS STAY UNDEFINED, on purpose. The ICD is bound by ProcessService out of the
	candidate set in the program's identity record, before the first thread runs - so the two exports
	it calls have no definition at link time and must not acquire one. `-z undefs` is what lets the
	link finish with them unresolved; what keeps that from being a hole is the audit below, which
	requires the undefined set to be EXACTLY the runtime's imports plus the kind's two symbols.
	"""
	consumer_dir = root / "src/user/apps/tools"
	object_file = out_dir / arch / f"{name}.o"
	errors = out_dir / arch / f"{name}.stderr"
	image_target = root / ".build/cargo/quarantine" / settings["triple"]
	spec = str(root / settings["spec"]) if settings["spec"].endswith(".json") else settings["spec"]
	command = [
		str(root / "src/tools/build-consumer-object.sh"), str(consumer_dir), str(image_target), "134217728",
		settings["rustflags"], spec, name, str(object_file), str(errors),
	]
	if settings["spec"].endswith(".json"):
		command.append("-Zjson-target-spec")
	subprocess.run(command, check=True)

	lld, _ = rust_lld(root)
	start = start_of(root, settings)
	if not start.is_file():
		print(f"foreign-audit-link: {arch}: no {start}; the image build has not produced the entry object, so the quarantine consumer was not linked", file=sys.stderr)
		return None
	elf = out_dir / arch / f"{name}.lsexe"
	arguments = quarantine_link_arguments(settings, start, object_file, archive if with_loader else None, rlibs, runtime_of(root, settings))
	subprocess.run([str(lld), *map(str, arguments), "-o", str(elf)], check=True)
	run(["llvm-strip", "--strip-debug", str(elf)])
	return elf


def quarantine_record_fields(root, arch, settings, out_dir, archive, rlibs, name, licence):
	"""The quarantine consumer's source digest and language section, from what its link used.

	`archive` is the loader archive when the consumer links one, and `None` when it does not."""
	start = start_of(root, settings)
	object_file = out_dir / arch / f"{name}.o"
	arguments = quarantine_link_arguments(settings, start, object_file, archive, rlibs, runtime_of(root, settings))
	# THE CONSUMER'S OWN COMPILE, then - when it links the loader - the builder's configuration of the
	# archive it links, then the link. A consumer that links no C has no C flags, and says so by
	# naming only the producer that compiled it.
	compile_line = f"consumer={name} --no-default-features --features shared-image rustflags={settings['rustflags']}\n"
	if archive is not None:
		configuration, fields = builder_configuration(out_dir, arch)
		cflags = f"clang: {fields['cflags']}; rustc: {settings['rustflags']}"
	else:
		configuration = ""
		cflags = f"rustc: {settings['rustflags']}"
	configure = hashlib.sha256((compile_line + configuration + f"substrate={' '.join(SUBSTRATE)}\nlink={described(arguments)}\n").encode()).hexdigest()
	objects = digest_objects([start, object_file, *([archive] if archive is not None else []), *rlibs])
	source = f"source-sha256={digest_file(root / 'src/user/apps/tools/src' / f'{name}.rs')}"
	return [source, *producer_lines(root, cflags, configure, objects, licence)]


def quarantine_identity(root, arch, settings, out_dir, archive, rlibs, providers, candidates, name, licence):
	"""The quarantine consumer's identity record: the same v2 format, with the slot it was built with.

	IT IS `language=foreign` BECAUSE ITS LINK IS. The object is Rust and the record could have said
	so, but what a language section describes is what PRODUCED the artifact - and what produced this
	one is the audit substrate, whose compiler, sysroot, patch series and licence are what a reader
	needs to know about the bytes in front of them.
	"""
	def provider_digest(name):
		library = root / ".build/image" / settings["triple"] / "lib"
		for candidate in library.rglob(f"{name}.lslib"):
			note = pathlib.Path(str(candidate) + ".identity")
			run(["llvm-objcopy", "--dump-section", f".note.liber.identity={note}", str(candidate), "/dev/null"])
			blob = note.read_bytes()
			note.unlink()
			return hashlib.sha256(blob[20:20 + int.from_bytes(blob[4:8], "little")]).hexdigest()
		raise SystemExit(f"foreign-audit-link: no staged {name}.lslib to take a digest from")

	source, *language = quarantine_record_fields(root, arch, settings, out_dir, archive, rlibs, name, licence)
	lines = [
		"format=liber-image-identity-v2",
		"kind=executable",
		f"artifact={name}",
		"package=tools",
		source,
		f"target={settings['triple']}",
		"profile=release",
		*language,
	]
	for provider in providers:
		lines.append(f"provider={provider}:{provider_digest(provider)}")
	# THE SLOT, WITH ITS CANDIDATES BY NAME AND DIGEST. This is what ProcessService binds before the
	# first thread runs, and it is why the two ICD exports may be undefined in the artifact.
	for kind, names in candidates:
		lines.append(f"selection={kind}:" + ",".join(f"{name}.lslib={provider_digest(name)}" for name in names))
	return ("\n".join(lines) + "\n").encode()


def verify_records(root, work):
	"""Recompute every producer field of every audit-produced record and compare it with the record.

	WHAT A WELL-FORMED DIGEST COULD NOT SAY. A field of sixty-four hex digits proves nothing about
	what it digests, and two of these records once carried digests of constant strings that passed
	that check for months. Here each field is recomputed from the inputs the link used - the archive
	and the objects beside it, the substrate archives, the entry object, the builder's recorded
	configuration, the profile sysroot, the patch series and the three tools - so a record that does
	not describe its own artifact, or an artifact whose inputs moved without a relink, fails by name.

	NOTHING IS BUILT OR LINKED. What is read is what the last link left."""
	failures = []
	checked = 0
	for arch, settings in TARGETS.items():
		directory = work / arch
		archive = directory / "libvulkan.a"
		rlibs = substrate_rlibs(root, settings)
		missing = [path for path in (archive, *rlibs) if not path.is_file()]
		if missing:
			failures.append(f"{arch}: {', '.join(str(path) for path in missing)} - the inputs of the last link are gone, so its records cannot be checked; relink")
			continue
		artifacts = [("audit-vulkan", directory / "audit-vulkan.lslib", lambda: audit_record_fields(root, arch, settings, work, archive, rlibs))]
		for name, with_loader, licence in QUARANTINE_CONSUMERS:
			artifacts.append((name, directory / f"{name}.lsexe", functools.partial(quarantine_record_fields, root, arch, settings, work, archive if with_loader else None, rlibs, name, licence)))
		for name, artifact, expected in artifacts:
			if not artifact.is_file():
				failures.append(f"{arch}: {artifact.name} is missing")
				continue
			recorded = dict(line.split("=", 1) for line in embedded_record(artifact).splitlines() if "=" in line)
			try:
				lines = expected()
			except FileNotFoundError as missing:
				failures.append(f"{arch}: {name}: {missing.filename} is gone, so what its link used cannot be recomputed")
				continue
			for line in lines:
				field, value = line.split("=", 1)
				if field not in PRODUCER_FIELDS:
					continue
				if recorded.get(field) != value:
					failures.append(f"{arch}: {name} records {field}={recorded.get(field)!r} and its inputs give {value!r}")
			checked += 1
	for failure in failures:
		print(f"foreign-audit-link: {failure}", file=sys.stderr)
	if failures:
		print("foreign-audit-link: a record that does not describe its artifact's inputs is stale or forged; relink with src/tools/foreign-audit-link.py --check", file=sys.stderr)
		return 1
	print(f"foreign-audit-link: {checked} identity record(s) describe exactly the inputs their links used, field by field")
	return 0


def main():
	parser = argparse.ArgumentParser(description="The converging audit link, and what it resolved.")
	parser.add_argument("--out", default="src/foreign/INVENTORY-pass2.json")
	parser.add_argument("--work", default=".build/foreign/pass2")
	parser.add_argument("--check", action="store_true", help="regenerate and compare instead of writing")
	parser.add_argument("--self-test", action="store_true", help="prove the scan sees what it reports absent, and stop")
	parser.add_argument("--verify-records", action="store_true", help="recompute the audit-produced identity records from their inputs, and stop")
	arguments = parser.parse_args()
	root = pathlib.Path(__file__).resolve().parents[2]
	work = root / arguments.work
	if arguments.verify_records:
		return verify_records(root, work)
	work.mkdir(parents=True, exist_ok=True)
	if arguments.self_test:
		return self_test(root, work / "self-test")

	inventory = {}
	for arch, settings in TARGETS.items():
		# TWICE, AND THE GATE IS THAT THE TWO AGREE. One link is a measurement; two consecutive links
		# producing the same resolved set is the fixed point this pass is defined by.
		first = measure(root, arch, settings, work)
		second = measure(root, arch, settings, work)
		if first["resolved_from_substrate"] != second["resolved_from_substrate"]:
			print(f"foreign-audit-link: {arch}: two consecutive links resolved different sets - this is not a fixed point", file=sys.stderr)
			return 1
		first["converged"] = True
		inventory[arch] = first
		print(f"foreign-audit-link: {arch}: {len(first['archive_surface'])} asked, {len(first['resolved_from_substrate'])} from the substrate, {len(first['resolved_from_runtime'])} from the runtime, TLS={first['has_tls_segment']}")

	surfaces = {arch: set(entry["archive_surface"]) for arch, entry in inventory.items()}
	inventory["_differences"] = {
		"common_surface": sorted(set.intersection(*surfaces.values())),
		"per_target_only": {arch: sorted(surface - set.intersection(*surfaces.values())) for arch, surface in surfaces.items()},
		"note": "the surface is a property of one C configuration, so the three targets agreeing is expected; the ELF sizes and the relocation forms are where three links are visible",
	}
	# THE NEGOTIATED VERSION, BESIDE THE SYMBOLS. Which interface version this substrate admits is a
	# derived fact rather than an assumption, and it is the same on every target - so it is recorded
	# once, from the pinned headers and the substrate it was linked against.
	inventory["_icd_interface"] = icd_interface(root)
	text = json.dumps(inventory, indent="\t", sort_keys=True) + "\n"
	destination = root / arguments.out
	if arguments.check:
		if not destination.is_file():
			print(f"foreign-audit-link: {arguments.out} is missing", file=sys.stderr)
			return 1
		if destination.read_text() != text:
			print(f"foreign-audit-link: the regenerated inventory differs from {arguments.out}", file=sys.stderr)
			return 1
		print("foreign-audit-link: the recorded pass-2 inventory reproduces")
		return 0
	destination.write_text(text)
	print(f"foreign-audit-link: wrote {arguments.out}")
	return 0


if __name__ == "__main__":
	sys.exit(main())
