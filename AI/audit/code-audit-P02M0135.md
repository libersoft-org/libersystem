IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T12:47:28Z):

FOREIGN GRAPHICS-STACK PREREQUISITES - THE FIRST THREE STEPS OF THE FREEZE ORDER.

WHAT WAS DONE, AND WHERE IT STOPS. This milestone's own freeze order is LICENSING POLICY -> BOOTSTRAP
INPUTS -> bootstrap pin -> portable static target -> static-target pin -> pass 1 -> substrate ->
platform port -> pass 2 -> derived pin, and it says in as many words that nothing but the first two
may start before the third. The first three are done and are recorded below. The rest are not, and
the milestone is NOT complete.

A CORRECTION FIRST, BECAUSE IT IS WHY THIS WORK HAPPENED AT ALL. A previous session recorded this
milestone as blocked on a missing network and parked it. That was wrong: `git ls-remote` against
KhronosGroup answers, and both pinned archives were fetched here. What actually makes this milestone
large is the substrate itself, not access to the sources.

THE LICENSING POLICY (`docs/DEPENDENCY_POLICY.md`), written before any upstream source was fetched,
which is the order the milestone insists on - a policy recorded after a dependency is chosen
describes what that dependency happens to satisfy, and the check it then performs can only pass. It
states what this repository's own Unlicense does and does not cover, what an import must record
(source, exact revision and archive digest, patch series), what "compatible" means as a specific test
rather than a feeling, and the three refusals. It also states the rule for a dual-licensed upstream:
recorded under the ONE term this project takes it under, because "available under A or B" is the
upstream's offer and which was accepted is this repository's fact.

AND IT IS ENFORCED RATHER THAN DESCRIBED (`src/tools/check-dependency-policy.sh`, gate
`dependency-policy`). A refusal nothing checks is a paragraph. The gate refuses an unreviewed
licence, an incomplete entry, a branch where a revision belongs, a digest that is not a SHA-256, a
vendored directory with no entry, a build script that reaches the network, and a source file carrying
a third-party copyright notice that no recorded import covers. Each was verified by introducing the
condition and watching it fail, then removing it: a GPL-3.0 entry, half a pin, `main` as a revision,
an unrecorded `third_party/` directory, a `curl` added to a build script, and a planted notice file.

THE GATE FOUND A REAL ONE ON ITS FIRST RUN. `src/user/libs/audio/vorbis` is an adaptation of
`lewton` and carries est31's notices and the upstream LICENSE - the attribution is intact - but
nothing in the tree recorded where it came from. Its import commit says nothing, and no revision is
recoverable from the contents. `third_party/INVENTORY.tsv` now records it under the term this project
takes it under (MIT), with its revision and digest as `unrecorded` - which is the honest answer, not
an exemption. Writing a plausible digest would have made the inventory agree with nothing. The gate
ACCEPTS `unrecorded` and NAMES the entry on every run, so the gap stays visible; a new import may not
use it, and half a pin is refused outright.

THE BOOTSTRAP INPUTS. Three cross files (`src/foreign/cross/{x86_64,aarch64,riscv64}.cmake`) and a
minimal bootstrap sysroot (`src/foreign/bootstrap-sysroot/`).

  THE CROSS FILES EXIST BECAUSE WITHOUT THEM CMAKE COMPILES FOR THE HOST, and that failure is silent:
  the build succeeds against glibc's ABI on a hosted x86_64 Linux and the inventory describes a
  system this one is not. Every ABI value in them is taken from what the RUST side is actually built
  with rather than chosen again - the x86_64 target's SSE2-only feature line and its disabled red
  zone from `src/user/x86_64-unknown-none.json`, aarch64's general-registers-only, riscv64's
  `rv64gc`/`lp64d`/`medany` from `src/tools/build-shared.sh`. Two descriptions of one ABI is how a C
  object and a Rust object come to disagree about a type.

  `-nostdlibinc` AND NOT `-nostdinc`, which was found by trying the wrong one. `-nostdinc` drops the
  compiler's own resource headers as well, and those - `stddef.h`, `stdint.h`, `stdarg.h`, `float.h`,
  `limits.h`, `stdbool.h` - describe the TARGET. Dropping them forces the sysroot to restate the
  target's own type widths, which is the second description this milestone exists to avoid.

  THE SYSROOT IS NOT A LIBC and its README says so at the top. It holds the nine headers the pinned
  option set actually needs - determined by parsing the unconditional includes out of the loader's
  own source list rather than guessed - and each declaration is there because a pinned source
  includes the header it lives in. Whether the substrate will PROVIDE a symbol is the derived
  inventory's decision, and a declaration here is explicitly not a promise that it will. That is the
  failure the milestone names: a header declaring a function with no symbol behind it is a compile
  that succeeds and a link that fails, which is the POSIX layer arriving through the include path.

THE BOOTSTRAP PIN (`src/foreign/PIN-bootstrap.toml`), frozen after those inputs existed. It carries
both projects by tag, commit and archive SHA-256 with the statement that the loader revision builds
against the header revision; the CMake option set IN FULL with its values chosen rather than named;
the generated-source policy; the three cross files with digests; the toolchain identities; and the
bootstrap sysroot digest.

  TWO OF ITS VALUES WERE DETERMINED RATHER THAN COPIED FROM THE PLAN. The licence closure was read
  out of the archive: Apache-2.0 majority, MIT for cJSON and loader_json, HPND-Kevlin-Henney for the
  Windows dirent shim, CC-BY-4.0 for documentation - all permissive, and the last two outside this
  configuration's build closure. And the generated-source question has an answer the plan left open:
  the registry-generated sources are VENDORED in the upstream archive at `loader/generated/`, so
  `LOADER_CODEGEN` is OFF and their digest is recorded. That is the more reproducible of the two
  answers the plan permits.

THE PIN IS READ RATHER THAN REMEMBERED (`src/tools/check-foreign-pin.sh`, gate `foreign-pin`). It
verifies every cross file and the sysroot against their frozen digests, checks the sysroot holds the
headers it is pinned to hold, verifies the archive digests when the archives are staged, and refuses
to accept a pin with no policy in front of it. When the archives are absent it SAYS their half did
not run rather than passing silently. Both failure directions were verified: a cross file with a byte
appended, and an archive with a byte appended.

WHAT WAS VERIFIED, AND HOW.

THE BOOTSTRAP GATE, WHICH IS THIS MILESTONE'S OWN WORDING: "the three files and the sysroot exist,
each digested, and a compile of the pinned configuration reaches a diagnostic from the SOURCES rather
than a missing-header error, on all three targets." Compiling `loader/cJSON.c` under each cross
file's flags gives, on all three targets, zero missing-header errors and
`vk_loader_platform.h:248: use of undeclared identifier 'DIRECTORY_SYMBOL'` - the loader saying its
platform port is missing, which is a later deliverable of this milestone. Reaching that diagnostic is
what proves the headers resolved and the ABI was the cross file's rather than the host's.

THE GATES. `./check.sh --gate dependency-policy --gate foreign-pin --gate source-hygiene --gate
host-tests --gate verify-model --gate milestone-index --gate gate-oracles` - all passed. Registering
the two new gates required three further changes the model insisted on and was right to: the catalog
entry, its array length, and the release-required list. `verify-model` reports 607 checks and a
consistent model.

ONE DEFECT OF MY OWN, FOUND BY A GATE. `source-hygiene` caught `grep -m1 ... | head -1` in the new
dependency check: under `pipefail` a reader that stops early makes a MATCH read as a failed pipeline.
Replaced with a single `sed` that quits after the first match, and the detection was re-verified
against a planted file.

WHAT IS NOT DONE. Seven of this milestone's eleven items remain, and they are the substrate itself:
the portable static target and its pin, the two derivation passes, the profile sysroot, the foreign
ABI facilities, the C++ ABI and TLS decisions, the manifest/build/cache integration for foreign
artifacts, and the discovery model. Nothing above claims otherwise, and the milestone's own rule -
nothing may be declared done before the derived pin - is not being bent.

---

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T13:15:36Z):

THE PORTABLE STATIC TARGET AND PASS 1.

Two more steps of the freeze order, in order. Three of the eleven items are now done; eight remain.

THE PORTABLE STATIC TARGET, which the plan makes its own deliverable because upstream's
`BUILD_STATIC_LOADER` was renamed `APPLE_STATIC_LOADER` precisely for implying a portability it never
had - non-Apple use is made to fail, so none of this system's three targets can obtain a static
loader from it.

  THE PATCH (`src/foreign/patches/0001-portable-static-target.patch`) does three things and no more:
  `__LiberSystem__` joins the common-unix functionality set so the shared declarations apply; a
  LiberSystem block supplies the macros and types that section needs; and the platform FUNCTIONS are
  declared EXTERN rather than defined `static inline`.

  THE EXTERN DECISION IS THE ONE WORTH DEFENDING. Every other platform writes bodies in that header
  because its bodies are one call into a libc that already exists. LiberSystem's do not exist - they
  are the foreign ABI facilities, a later item - and writing them now would invent the substrate from
  the shape of a header rather than from the measured inventory. And the undefined references those
  declarations leave in the archive ARE the candidate surface pass 1 is meant to measure: a header
  full of inline bodies would have hidden that surface inside the objects.

  THE BUILDER (`src/tools/build-foreign-static.sh`) compiles the fourteen sources of the pinned
  configuration and archives them with `llvm-ar crsD` - the `D` is not decoration, without
  deterministic members two archives of identical objects differ and "builds reproducibly" is
  unprovable. All three targets compile, archive, and produce the same digest on a second clean
  build. `src/foreign/PIN-static-target.toml` records the patch digest, the builder digest and the
  three archive results.

  WHAT IT DELIBERATELY DOES NOT CLAIM: that it LINKS. An archive does not resolve its external
  symbols, and the providers a link needs are all assigned after pass 1. The strict converging link
  is pass 2's.

MY OWN CROSS FILE WAS WRONG, AND THE BUILD FOUND IT. aarch64 refused to compile `cJSON.c`:
"requires 'double' type support, but ABI 'aapcs' does not support it". I had written
`-mgeneral-regs-only` into that cross file on the assumption that a freestanding target must be
soft-float. That is a SECOND description of an ABI the Rust half already fixes -
`aarch64-unknown-none` reports `target_feature="neon"` - which is exactly the failure the cross
file's own header warns about, committed by the file that warns about it. Corrected in both the cross
file and the builder, and the bootstrap pin records the refreeze with its reason rather than as a
changed number.

PASS 1 (`src/tools/foreign-inventory.py`, `src/foreign/INVENTORY-pass1.json`). Derived from the
objects rather than written, which is the whole point: an inventory somebody typed is a guess, and
the first real import is what discovers the guess was wrong.

  WHAT IT FOUND, PER TARGET:
    58 undefined symbols - the candidate surface: 4 compiler-runtime (`memcpy`, `memset`, `memmove`,
      `memcmp`), 1 errno accessor, and 53 platform and libc symbols.
    0 TLS symbols and 0 TLS relocation forms, on all three targets.
    0 C++ ABI facilities: no `__cxa_*`, no unwind personality, no RTTI, no `atexit`. The pinned
      configuration is C99 and the object closure says so.
    1 static constructor and 1 destructor, BY NAME: `loader_init_library` and `loader_free_library`.
      A count would have told a reader nothing; the names say exactly what the configuration expects
      a runtime to call.
    0 thread-CREATION symbols and 4 thread-SYNCHRONISATION symbols - mutex create, lock, unlock and
      delete. No condition variable and no once-initialisation are referenced at all.
    Relocation forms 5 / 8 / 12 on x86_64 / aarch64 / riscv64.

  THE RELOCATION COUNTS ARE WHY THIS IS THREE BUILDS AND NOT ONE COPIED. The plan warns that an
  inventory identical on all three has not been derived from three builds. The SYMBOL sets here ARE
  identical, and that is the right answer for a C99 configuration with no per-architecture sources -
  a difference would mean a source branch nobody intended. What cannot be identical is the relocation
  set, and it is not. The inventory records that reasoning rather than leaving a reader to wonder.

PASS 1 CAUGHT A CONFIGURATION ERROR OF MINE ON ITS FIRST RUN, which is the best evidence that
deriving it was worth doing. The surface named `linux_read_sorted_physical_devices` and
`linux_sort_physical_device_groups` - symbols from `loader_linux.c`, a source this configuration does
not compile. The cause: the builder passed `-DLOADER_ENABLE_LINUX_SORT=0`, and the source guards it
with `#if defined(...)`, so defining it to zero switched it ON. A candidate surface holding two
symbols no object can ever provide is the inventory describing a build nobody made. The define is
gone, the surface is 58 rather than 60, and the static-target pin records the refreeze and its
reason.

WHAT THE GATE NOW HOLDS. `foreign-pin` verifies the bootstrap part, the static-target part, and
REGENERATES the pass-1 inventory to compare - the plan says a regeneration that differs fails the
gate, and it does. It also checks the static patch applies to the revision the bootstrap part pins,
and refuses an archive recorded without a second build agreeing, because that is an attempt rather
than a result.

A GAP IN MY OWN GATE, FOUND BY USING IT. The bootstrap pin recorded a sysroot digest that the gate
checked only by listing filenames, so a header edited after the freeze would have passed - which is
the exact failure the digest is in the pin to prevent. The gate now computes the tree digest and
compares it.

VERIFICATION. `./check.sh --gate foreign-pin --gate dependency-policy --gate source-hygiene --gate
milestone-index --gate verify-model --gate host-tests` - all passed. `verify-model` required the new
foreign inputs to be owned or declared, and was right to: `src/foreign` and `third_party` are now
declared non-code with reasons that say why - they are inputs to an audit-only build whose output
never enters an image, and they are held to their digests by `foreign-pin` rather than left
unguarded. `source-hygiene` caught two new scripts with a shebang and no executable bit.

WHAT IS NOT DONE, AND WHAT THE INVENTORY HAS ALREADY DECIDED. Eight items remain. Two of them -
the C++ ABI and TLS - now have their measurement: zero C++ ABI facilities and zero TLS, on all three
targets. Neither may be TICKED on that evidence, because the plan puts both stop conditions on the
FULL closure rather than on pass 1, and the platform port, the generated objects and the
compiler-runtime selection can each still introduce what pass 1 did not see. The same holds for the
no-threads item. Recording the pass-1 measurement is not the same as passing the gate, and this audit
does not claim it is.

---

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T13:40:31Z):

THE FOREIGN ABI FACILITIES.

Four of eleven items now; seven remain. This one is the substrate itself, and its rule is the whole
difficulty: provide EXACTLY what the derived inventory names and nothing beyond it.

THE SPLIT, MEASURED RATHER THAN CHOSEN. Pass 1's fifty-eight symbols divide cleanly once you ask
which question each one is: thirty-seven are translations of a C function - an allocator, memory and
string work, one math call, formatted output, the process hooks, the mutual exclusion - and
twenty-one are decisions about what a provider or a configuration directory IS on this system, which
is the DISCOVERY item's question and not this one's. `src/user/libs/foreign/abi` holds the
thirty-seven. The other twenty-one are named in the gate so the split is a written rule rather than
an omission somebody later reads as an oversight.

WHAT IS IN IT, AND THE PARTS THAT ARE NOT OBVIOUS:

  THE ALLOCATOR CARRIES A HEADER, because C's `free` is not told the layout and Rust's is. A block
  that lost its size would be returned to the allocator as a different allocation than it took. The
  header is found by subtracting its own size from the payload - which works because the padding
  rounds UP, so the header always ends exactly where the padding begins. `calloc` checks its
  multiplication, which is the entire reason it exists as a separate call; a failed `realloc` leaves
  the original alive; and a zero-size request still returns a distinct pointer, because C callers
  compare against NULL to decide whether the allocation failed.

  THE MUTUAL EXCLUSION IS A COUNTER AND SAYS SO. The pinned configuration creates no threads - pass 1
  measured zero creation symbols on all three targets - so what these need to do is what an
  uncontended lock does. A `lock` that finds the mutex already held has discovered the
  single-threaded contract is broken and REFUSES, loudly. A spin loop would have looked like it
  worked right up until it did not, and a real lock would be a concurrency implementation nobody has
  reviewed backing an interface nobody calls concurrently.

  THE ONE OUTPUT PATH IS THE PROCESS'S DIAGNOSTIC STREAM, and `fputs` to anything else is REFUSED
  rather than discarded. A write that silently goes nowhere is a message somebody spends an afternoon
  looking for.

  THE CONVERSION SET IS BOUNDED AND AN UNRECOGNISED ONE IS COPIED THROUGH. Dropping it loses the
  value and hides the reason; copying it means the caller sees what it wrote.

THREE DESIGN DECISIONS WERE FORCED BY THINGS THAT WENT WRONG, and each is better for it:

  1. THE CRATE HAS NO DEPENDENCIES, INCLUDING `rt`. It reached `rt` for diagnostics at first, and
     that makes it untestable on the host - `rt` and `std` both define `panic_impl`, so `cargo test`
     fails on a duplicate lang item before a test runs. That is exactly why `service-logic` exists as
     its own crate. Every function here implements a C contract whose violations are SILENT, which
     makes host fixtures worth more here than almost anywhere, so the dependency was inverted: the
     substrate installs a diagnostic sink and this crate calls it. The default is to panic rather
     than to discard, because this crate says the same thing about `fputs` in the same breath.

  2. THE EXPORTED NAMES ARE SUPPRESSED UNDER `cfg(test)`. This crate defines `malloc`, `free`,
     `memcpy` and `strlen` as unmangled C symbols; in a host test binary those OVERRIDE the platform
     libc's, so the harness's own allocation lands in an allocator implemented over the Rust
     allocator over that same libc. The process segfaulted before the first test printed its name,
     which is how this was found. `cfg_attr` rather than `cfg`, so the bodies and the `extern "C"`
     ABI are still what the fixtures exercise and only the linker-visible name is withheld.

  3. THE VARIADIC ENTRY POINTS ARE GATED ON `target_os = "none"`. `c_variadic` is unstable and the
     host suite runs on the pinned stable toolchain. Gating them on `not(test)` was not enough and
     the GATE found it: the host suite builds the LIB as well as the test harness, and the lib build
     has no `test` cfg, so the unstable feature was still requested on stable. What this forced is an
     improvement rather than a workaround: the argument list became an INTERFACE, so `render` and
     `scan` - where the format parsing, the padding, the sign-and-zero-pad interaction, the
     unrecognised conversion and the matching-failure rule all live - are now reachable from host
     fixtures, and the `VaList` implementation is four forwarding lines.

A BUG I WROTE AND THE TESTS CAUGHT. The field padding was built on a `Deref` impl returning a slice
of a `const` array - and a `const` referenced in a function body is a fresh temporary, so the
returned reference dangled. It segfaulted. Replaced with a plain `push_repeated`, which is what it
should have been.

VERIFICATION. Twenty host fixtures, all passing, and every one of them is a C contract whose
violation is silent: `strncpy` padding and not terminating a full copy while `strncat` always
terminates; `memcmp` comparing unsigned, because reading the bytes as signed reverses the answer for
half the byte range; `strchr(s, 0)` finding the terminator; an empty needle matching at the start;
`atoi` saturating rather than wrapping; `strtoul` reporting where it stopped so a caller can tell
zero from not-a-number; `strtod` leaving a bare exponent marker alone; `fabs` clearing the sign bit
so `-0.0` becomes `+0.0` and a NaN stays one; `strerror` not calling an unknown number a success;
`snprintf` returning what it WOULD have written; `%05d` of -42 being "-0042"; a scan's matching
failure ending the whole call; and the allocator's header surviving a `realloc`.

The crate builds for all three targets. `./check.sh --gate host-tests --gate foreign-facilities
--gate foreign-pin --gate dependency-policy --gate source-hygiene --gate verify-model --gate
milestone-index` - all passed, and `./build.sh --arch x86_64` is clean.

THE EXACT-SURFACE RULE IS A GATE (`foreign-facilities`), because "do not grow a general POSIX layer"
is a rule about a direction of travel and every individual step along it looks reasonable. It fails
in both directions - a symbol provided and not named is the POSIX layer arriving one function at a
time; a symbol named and not provided is a link that fails after every other question is answered -
and both were verified by introducing the condition. Registering it needed the crate to gain a source
owner in the services manifest and entries in the model catalog and the release-required list, all of
which the model insisted on and was right to.

WHAT REMAINS. Seven items: pass 2's converging link, the profile sysroot, the C++ ABI and TLS
decisions, the no-threads gate on the full closure, foreign-artifact integration, and the discovery
model. The twenty-one ambient symbols are deliberately absent until that last one decides what they
mean here.

---

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T13:51:01Z):

THE DISCOVERY MODEL - THE LOADING HALF.

This item has two halves and this section covers one: the LOADER PORT, which the plan calls a
deliverable of this milestone and "the largest single piece of work the pinned configuration
implies". The other half - ProcessService's selection slot - is the authority, and is not done. The
item is NOT ticked.

WHAT THE UPSTREAM LOADER DOES, AND WHY NONE OF IT CAN HAPPEN HERE. Its Unix platform layer reads
environment variables for search paths, scans directories under them for JSON manifests, treats
whatever it finds as a candidate driver, opens the selected one with `dlopen`, resolves entry points
with `dlsym` and releases it with `dlclose`. Every step is authority this system does not hand out.

AND THE CALLS STAY. Patching them out is a fork: every line removed is one this tree then owns and
has to re-remove at the next revision. Answering them is the smaller change and it keeps the audit
honest - what the inventory measured is what the configuration asks for, and `foreign-discovery` is
what the system chooses to say back.

  `getenv` ANSWERS NOTHING, FOR EVERY NAME, and that is a decision rather than a stub. Those
  variables exist precisely to let something outside the process point it at a driver, which is the
  authority the selection slot holds instead. The loader's own code treats an absent variable as "no
  override", so no upstream branch had to be patched to get the answer this system means.

  A DIRECTORY CONTAINS EXACTLY WHAT THE RECORD NAMES UNDER IT. The loader's walk is unchanged and
  finds exactly the set the selection slot already admitted, so the code path is exercised for real
  rather than removed - and the answer cannot be widened by anything outside the launch. A directory
  the record does not name returns NULL, which is the branch upstream already handles for a
  search-path element that holds nothing.

  A MANIFEST IS BYTES THE RECORD ALREADY HOLDS. `fopen` finds it, `fread` copies from memory,
  `fstat` reports the one field the loader reads - the size it allocates its buffer from. A write
  mode is REFUSED rather than ignored, because a loader that believed it had opened a manifest for
  writing would report success for a write that went nowhere.

  `dlopen` BECOMES "TAKE A REFERENCE TO THE ALREADY-VERIFIED PROVIDER" AND `dlclose` BECOMES "DROP
  IT". They open nothing and can fail in exactly one way: by naming a provider that is not in the
  closure. Nothing is unmapped on close - the provider is in the verified closure for the life of the
  process, and unloading it would be the replacement this model forbids.

  THE IDENTITY CALLS ANSWER CONSISTENTLY AND THE CONSISTENCY IS THE POINT. The loader compares the
  real and effective ids to decide whether the process is setuid and whether to trust environment
  search paths. Equal ids mean "not elevated", which is true here, and it keeps the loader on the
  code path this substrate actually ported. Zero rather than an invented non-zero id, because this
  system has no user identities and a made-up number is a fact somebody later relies on.

THE ADMITTED VERSION SET IS DECIDED, NOT INHERITED. "Two exports" is not a version-independent
contract - the upstream interface does not have one export shape across its versions - so the set is
this milestone's own decision and `version.rs` is written as one: 2 through 6 admitted, and four
refusals each for its own reason. Version 0's multi-export bootstrap is a different shape; version 1
has no negotiation function at all, so an ICD there exports ONE symbol and the two-export rule cannot
be satisfied by it; 7 and above may QUERY the negotiation and physical-device functions rather than
export them, so a conforming driver need not export what this substrate resolves - and 7 is therefore
negotiated DOWN to 6 rather than refused outright, which is upstream's own rule and what keeps two
exports sufficient. A driver requiring `vk_icdGetPhysicalDeviceProcAddr` is refused BY NAME whatever
version it offers, because that is a fact about the driver rather than the revision.

THE THIRD EXPORT IS MATCHED AND REFUSED rather than falling into the same arm as a misspelling. Both
answer NULL, but only one is a DECISION - and a reader following the lookup that failed should find
the reason rather than infer it from an absence. `refusal()` exists so the difference is assertable,
because C has nowhere to put it: `dlsym` returns a pointer or NULL.

THE EXACT-SURFACE GATE NOW COVERS BOTH CRATES AND BOTH DIRECTIONS. `foreign-abi` holds the
thirty-seven C translations, `foreign-discovery` the twenty-one discovery answers, and a symbol in
the wrong one fails: in the first it is a C library answering a discovery question, in the second it
is the discovery policy growing a general POSIX layer. All fifty-eight are provided and none beyond
them.

THE RULE CAUGHT ME IMMEDIATELY. I had written `loader_platform_dirname` and
`loader_platform_get_proc_address_error` because the header's common-unix section has them - and the
pinned configuration's objects never CALL either, so pass 1 does not name them and nothing may build
them. Both removed, with the reason recorded where they were: two functions nobody asked for, each
looking perfectly reasonable, is how a general POSIX layer arrives.

AND THE FIXTURES CAUGHT A REAL UNDEFINED BEHAVIOUR OF MINE. `fstat` and `dladdr` wrote their
structures through the caller's pointer with an ordinary assignment. The alignment of a C caller's
buffer is the caller's business and these functions have no way to check it; a real caller passes an
aligned `struct stat` and would never have failed, which is exactly how that class of bug survives.
Both now write unaligned.

VERIFICATION. Twelve host fixtures, all passing, and every one asserts a REFUSAL as well as an
answer: a record at the wrong version refused rather than interpreted; `getenv` empty for the five
variables that matter; the upstream search path's own directories not opening; a manifest outside the
record not opening; a write mode refused; a provider outside the closure not opening; exactly two
exports resolving and the third distinguishable from a misspelling; each refused version refused for
its own reason; a path refused rather than truncated; and "exists" meaning "is in this record".
`./check.sh --gate host-tests --gate foreign-facilities --gate foreign-pin --gate dependency-policy
--gate source-hygiene --gate verify-model --gate milestone-index` - all passed.

WHAT REMAINS OF THIS ITEM. ProcessService's selection slot: the v2 identity record gaining a declared
provider position with a kind and a closed set of digests, the resolution that makes a bound slot a
dependency node BEFORE the equality check runs, and the guest gate that launches a synthetic ICD
twice - at the lowest and highest admitted version - and asserts the four refusals. Until that
exists, this substrate can LOAD an ICD and nothing can SELECT one, which is half of what the item
claims.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T18:23:57Z):

THREE ITEMS MOVED IN THIS STRETCH: the profile sysroot (built and gated, not ticked), foreign-artifact
integration (ticked), and the authority half of the discovery model (built and gated; the item stays
open for the loader port). One item's checkbox changed. The milestone is five of eleven.

THE PROFILE SYSROOT. It is sized by the INVENTORY where the bootstrap one is sized by the option set,
and the inversion is the whole of it: forty-six declarations - the fifty-eight symbols pass 1 names,
less the twelve the loader's own patched headers declare for themselves. Measured against the
bootstrap headers, that dropped 43 declarations the pinned configuration includes a header for and
never calls: `open`, `read`, `close`, `exit`, `printf`, `qsort`, `strdup`, the whole `<ctype.h>`
classification family, most of `<math.h>`. A function it does not declare is one the substrate has no
symbol for, so a source that starts calling one fails to COMPILE rather than to link - which is the
decision being made deliberately rather than discovered by a diagnostic nobody attributed.
`arch/` carries one header per target - the triple, the data model, and static assertions over every
width, alignment and struct layout the declarations depend on - force-included by the build rather
than included by a source, so every translation unit carries the check. That found a wrong assumption
of this milestone's own on its first run: plain `char` is UNSIGNED on riscv64, not signed, so x86_64
is the odd target of the three. A comment would have carried that error indefinitely.
THE TRIM IS PROVED AND NOT ARGUED. The pinned configuration compiled against the profile sysroot
produces the archives the static-target pin froze - digest for digest, on all three targets. That is
the only form of proof that does not rest on having read the sources correctly.
ONE DESCRIPTION OF THE ABI. Adding the foreign path to the image build would have made a THIRD copy
of the triple and flags, after the cross files and the portable static target. It is now
`src/foreign/profile-abi.sh`, sourced by both builds and pinned in the lockfile, and the pin refuses
a drift in it (watched).
WHAT IS NOT DONE, AND WHY THE ITEM IS NOT TICKED: the item requires the sysroot bound into the
lockfile, and this file's own freeze order puts the profile sysroot digest in the DERIVED pin, which
is frozen after pass 2. Writing a derived pin now would be writing a part missing its own fields,
which the pin gate is built to refuse. It is bound into the inventory, the cache key and the image
identity today.

FOREIGN ARTIFACTS AS A KIND. The manifest gained a `producer` on sources and libraries. A Rust source
is a Cargo package whose closure Cargo answers; a foreign source is a directory of C sources with no
Cargo manifest, and what it is built from is the ORDERED OBJECT LIST on its library row - order
declared rather than sorted away, because link order decides which definition wins where two objects
offer one. The rules that made a foreign artifact expressible only by forging Rust fields are gone in
all three places that held them: the manifest's Cargo.toml requirement, the source-coverage walk that
equated "physical" with "has a Cargo.toml", and the build's own manifest check.
THE IDENTITY RECORD IS v2 IN TWO SECTIONS, image-wide and cold. A COMMON section every artifact has -
format, kind, artifact, package, source digest, target, profile - and a LANGUAGE section keyed by
producer. `rust` carries the compiler revision, the flags and the features; `foreign` carries each of
the three tools BY VERSION AND BY DIGEST, the flags, the sysroot digest, the configure-input digest,
the digest of the final objects, the patch-series digest and the licence. Each of those can change
what an artifact IS without changing a source byte, which is why they are in the record rather than
only in the build script.
IDENTITY COVERS THE WHOLE RECORD. The split governs who must UNDERSTAND which fields, not what the
digest is taken over: a consumer hashes the language section and does not parse it. The
hot-replacement rule therefore compares the common fields BY NAME and the language section AS TEXT,
line by line - strictly stronger than the three named Rust fields it replaced, because a producer
field that rule has never heard of still cannot move under a compatible verdict.
ALL FOUR READERS MOVED IN ONE CHANGE, which is what a cold transition means: `mkpackages`,
ProcessService, the shared compatibility path and device publication, plus `build-shared.sh`, the
staged-consistency check and the cache keys.
THE FIRST FOREIGN ARTIFACT is a synthetic ICD, compiled by the pinned C compiler against the profile
sysroot on all three targets, linked by the same linker into the same `.lslib` shape, and passing the
same relocation, W^X, export-owner and provider-closure audits every Rust library passes. No parallel
path, because a parallel audit is the thing that drifts.

THE SELECTION SLOT, WHICH IS THE DISCOVERY ITEM'S AUTHORITY HALF. A consumer built against a SET of
interchangeable providers names none of them. The manifest declares a slot as a KIND, the symbols
that kind admits, and the closed candidate set; the build requires every candidate to export EXACTLY
the kind's symbols and the consumer to import nothing else through the slot, and emits
`selection=KIND:NAME=DIGEST,...` into the record after the providers. ProcessService resolves each
slot into the dependency set and the recursive collection BEFORE the exact-equality check runs, so
that check keeps the meaning it always had.
THE KIND IS A CLOSED SET BECAUSE A FIXTURE PROVED IT HAD TO BE. A guest mutation corrupted
`vulkan-icd` into `0ulkan-icd` - still a well-formed kind - and the launch SUCCEEDED, because nothing
anywhere compared the kind against anything. It was documentation. It is now checked at the launch
and mirrored in the manifest, and a well-spelled kind nothing admits is a different refusal from a
malformed one.
THE RULES LIVE IN `service-logic` AND THE SERVICE DELEGATES TO THEM. `services` links `rt` and cannot
run a host test, and a launch-path rule with no test is a rule the next change does not know it broke.
The service now carries no copy of the parsing, binding or accounting.

VERIFICATION PERFORMED, AND WHAT EACH RUN ACTUALLY SHOWED:
- `./build.sh` on x86_64, aarch64 and riscv64: the foreign artifact compiles and links on all three,
  with the flags each target's single ABI description gives.
- `./test.sh --arch x86_64`: 388 passed. The suite grew by the new dynamic test.
- The new guest test binds the slot through the real ProcessService and then refuses fourteen
  substitutions: each of the twelve foreign producer fields corrupted in a real volume, the
  consumer's own slot line corrupted, and a candidate whose record no longer names the artifact it is
  staged as. The whole-file substitution is the same refusal at the same check and is not repeated -
  the helper needs the replacement to FIT the entry, and the synthetic ICD is the smallest artifact
  in the image.
- `check-icd-selection`: boots a guest, runs the consumer, and reads back that the admitted range
  holds at BOTH ends - 2 and 6 - with the four refusals asserted: an ICD offering 0, one offering
  only 1, one offering 7 brought down to 6, and the physical-device third export, which the build's
  exact-symbol rule refuses so it cannot be staged at all.
- `check-profile-sysroot`: declarations and inventory agree in both directions, and all three
  archives rebuild against the profile sysroot to the frozen digests. Watched to fail by adding one
  declaration the substrate does not provide.
- `check-foreign-identity`: each of the twelve producer fields moves the identity digest; the
  consumer edge carries exactly that digest; the cache key is the digest of a file the record is part
  of.
- `check-foreign-pin`: refuses a drift in the new ABI file (watched), and the two refrozen values -
  the bootstrap sysroot's README and the builder - are recorded with the reason.
- `build-shared.sh --verify-staged`: refuses a replaced selection candidate (watched), which the
  provider-chain check could not see at all because a candidate is recorded by an EXECUTABLE.
- `check.sh --gate` over dependency-policy, foreign-pin, foreign-facilities, profile-sysroot,
  foreign-identity, artifact-metadata, source-hygiene, dynamic-report, build-order,
  staged-consistency: all pass. The dynamic report was regenerated after all three targets were
  built, because the new artifact is in every image.
- Host tests: `bootproto` 87 passed including two new ones - the language section compared as text,
  and a producer field this rule cannot name still deciding the verdict; `service-logic` selection
  fixtures pass, now including the unknown-kind refusal.

NOT PERFORMED: `./test.sh` on aarch64 and riscv64, and the full `./check.sh`. The three targets were
BUILT and the foreign artifact was inspected on each; their guest suites were not run in this stretch.

A CORRECTION THIS STRETCH MADE TO ITS OWN EARLIER WORK: a mechanical v1-to-v2 version bump had
replaced the format string inside a compat fixture whose whole point was to carry a format the rule
does NOT understand. The test then asserted that the format the rule implements is one it cannot
read, and it had been failing since. It now names a version that does not exist.

WHAT REMAINS OF THIS MILESTONE: pass 2's converging link, and behind it the profile sysroot's lockfile
binding, the C++ ABI and TLS decisions, the no-threads gate on the FINALLY ADMITTED closure, and the
discovery item's loader port with its guest gate against the audit-linked loader.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T19:10:21Z):

PASS 2 CONVERGED, AND FOUR ITEMS CLOSED BEHIND IT. Nine of eleven. What was waiting on the audit link
was not one item but four, because three of this milestone's answers are MEASUREMENTS over a closure
that did not exist until something linked.

THE LINK. The ported archive is admitted WHOLE against the loader's own exports, so member selection
cannot narrow the closure below what the pinned configuration contains - a link rooted in whatever a
fixture happens to call would prove something about the fixture. It is strict in three ways at once:
nothing unresolved, nothing unresolved behind a shared library, and no default search path or default
library anywhere. It resolves 52 of 52, the substrate builds NOTHING it does not ask for, and the
eight references that remain are the Rust allocator shims and panic paths, taken from `lsrt.lslib`
exactly as every other library in this image takes them. The gate is the FIXED POINT and not the
first link: two consecutive links must resolve the same set, per target, because adjusting the
substrate after one link can change what the next one resolves.

THE PLATFORM PORT IS WHY THE SURFACE MOVED, and it is patch 0002 in the derived half. Two changes.
The layer scan's two entry points answer an EMPTY list, which is legal and true - there is no layer
directory on this system - and everything downstream then follows from upstream's own code:
enumeration reports zero, and an enabled layer is refused with `VK_ERROR_LAYER_NOT_PRESENT` by the
lookup that does not find it. Those two functions are the only entry points into the layer manifest
search, so replacing them removes the filesystem walk, the settings override and the
environment-steered paths in one place rather than in fifty. The environment reader takes upstream's
own third shape - the one its `#else` branch already provides for a platform without environment
variables - rather than the common-unix one LiberSystem joined for its FILE and THREAD facilities.
Taking the branch rather than stubbing the call is what removes the symbol: a stub that still called
`getenv` would leave it in the closure for the inventory to explain.
MEASURED EFFECT: six symbols gone, 58 to 52, on all three targets - `getenv`, the four identity
queries the elevation check needed, and the number parser only an environment value reached.
WHAT THE PORT COULD NOT REMOVE, STATED RATHER THAN GLOSSED: the provider-open and manifest-read
facilities the layer path SHARED with the ICD path. They are the same symbols, and the ICD path is
what this substrate exists to serve. What they reach here is a bounded package-owned record and not
an ambient directory, which is the discovery item's decision rather than an accident of this port.

THE PORTED TREE IS A COPY. The static-target part froze three archive digests for the pinned tree
before pass 1; editing that tree would move a value a part that PRECEDES this decision already holds.
So the port lives beside it and the builder is told which of the two to compile.

FOUR ANSWERS THAT ARE NOW MEASUREMENTS:
- TLS: Variant B, and the proof is over all three things that variant names - no `PT_TLS` segment, no
  `STT_TLS` symbol in any archive member or in the ELF, and no thread-local RELOCATION FORM anywhere.
  The last is the one a symbol scan cannot see: an object can carry an access to a variable defined
  elsewhere, and that is a relocation rather than a symbol.
- THREADS: no thread-creation symbol in the finally admitted set - every `ET_REL` member, the
  converged resolved set, and the audit-linked ELF - on any target. The stop condition is not hit.
- C++ ABI: exception tables, RTTI and `atexit` registration are FORBIDDEN, which under this file's
  own rule means the flags that stop them being emitted AND an artifact check. Static initialisation
  and `errno` are ADMITTED, because the closure names them.
- THE COMPILER-RUNTIME the link selected, which is two components and was declared by nobody: the
  substrate's own four memory functions for the C half, and `compiler_builtins` via build-std for the
  Rust half.

TWO DEFECTS THE NEW GATES FOUND, NEITHER OF WHICH A READING WOULD HAVE:
- The aarch64 audit-linked ELF carried `.eh_frame` after every C object had been compiled without
  exception tables. The Rust half emits unwind tables by default on that target. A forbidden
  mechanism needs the flag on EVERY producer that contributes to the closure, not on the one somebody
  thought of first; `-C force-unwind-tables=no` is now in the derived pin beside the C flags.
- Resizing the profile sysroot to pass 2 broke the compile, on a function the optimiser deletes. With
  the environment reader answering NULL in the same translation unit, everything below its first
  branch is dead - so the ARTIFACT never needed `strtoul`, and the SOURCE still called it. A sysroot
  that has to declare a function the substrate does not provide is the POSIX layer arriving through
  the include path, which is the one failure this sysroot exists to prevent, so the port removed that
  call too.

THE LOCKFILE IS COMPLETE. The derived part exists and holds what this file says it must: the ordered
platform-port series with a digest per patch and the part that owns each, the generated-source answer
(none, because the pinned upstream vendors them and codegen is off), the final profile sysroot
digest, and the compiler-runtime the converged link selected - plus the forbidden-mechanism decision
as both halves, flags and gate, because a decision with only one of them is a description. The pin
gate reads all of it, including the freeze order between the parts.

THE PROFILE SYSROOT IS NOW SIZED BY PASS 2, which is a correction the port forced: the candidate
surface over-states what a system must provide, and six of its symbols were things only the UNPORTED
loader asked for. Forty declarations. The proof moved with it and is stronger for the move - the
PORTED configuration is compiled against both sysroots and the two archives must be identical, on all
three targets, which is what says the dropped declarations were surface nobody required.

VERIFICATION PERFORMED:
- `foreign-audit-link`: converged on all three targets, twice each; the recorded inventory
  reproduces byte for byte; and its scan is proved able to SEE an injected thread-local definition, an
  injected thread-local ACCESS and an injected thread-creation reference before its zeroes are
  believed.
- `foreign-cxx-abi`: refuses a fixture carrying typeinfo, a vtable, an exception table and an
  `__cxa_atexit` registration, then scans the real artifacts on all three targets and finds none.
- `profile-sysroot`: declarations and the pass-2 inventory agree in both directions, and the ported
  archive is identical against both sysroots on all three targets.
- `foreign-pin`: all three parts intact, the freeze order holds, and drift in the new ABI file is
  refused (watched).
- `foreign-facilities`: 52 of 52 provided and none beyond them, now measured against pass 2 rather
  than pass 1 - which is what this file says the authority is.
- Host fixtures: `bootproto` 88 passed, including a new one asserting every thread-local relocation
  form on all three architectures is outside the allowlist the packager and the loader share.
- `./test.sh --arch x86_64`: 389 passed, including a new guest test that writes three thread-local
  relocation forms into a staged provider and requires each to refuse the launch.
- `check.sh --gate` over the ten gates this work touches: all pass.

NOT PERFORMED: `./test.sh` on aarch64 and riscv64. The three targets were BUILT and every pass-2
measurement was taken on each; their guest suites were not run in this stretch.

WHAT REMAINS, AND BOTH ARE THE SAME SHAPE - a mechanism the measurement ADMITTED rather than one it
refused. The C++ ABI item needs the supported-ABI answer for static initialisation: once-per-process
init and fini ordering across the provider DAG, what a constructor failing part way leaves behind,
normal exit versus crash - and the RUNNER that makes any of it observable, which this tree does not
have, because `liber_rt_start` calls the entry point directly. The discovery item needs the ported
loader ITSELF run in a guest against a synthetic ICD, reaching its entry points through the replaced
provider lookup; the selection slot half is built and gated.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-11T20:46:17Z):

TEN OF ELEVEN. This stretch built the one thing three items were waiting on - the init/fini runner -
and then ran the audit-linked artifact through the generic checks, which found two real defects.

THE RUNNER, BECAUSE THE MEASUREMENT ADMITTED THE MECHANISM. Pass 2 named exactly one constructor and
one destructor on all three targets, and under this file's own rule that makes static initialisation
ADMITTED and its runner a deliverable rather than an assumption. Nothing in this system ran them:
`liber_rt_start` performed the ABI check and called the entry point directly, so a constructor in a
loaded module was an initialisation that silently did not happen.
WHERE IT LIVES AND WHY. The kernel records each image's `.init_array` and `.fini_array` as it maps
it, biased to the address it was mapped at, in LOAD order - which is the provider order the loader
was given, so walking it forwards runs a provider's constructor before its consumer's and walking it
backwards does the same for destructors. A new syscall hands a process its own table, entry by entry;
it takes no handle because there is no other process it could sensibly name - the entries are
addresses in an address space, and an address from another one means nothing. Appending a syscall
needs no ABI bump, which this tree's own rule says in as many words.
ONE DEFECT ON THE WAY, and the fixture found it immediately: the main image was recorded at bias
zero, and a position-independent executable is mapped at `DYNAMIC_MAIN_BASE`. The first constructor
call faulted reading the array at exactly the address the section header names, which is what said
the bias had been left out rather than the table being wrong.

THE FOUR POSITIVE GATES, OBSERVED IN A GUEST. The fixture is a foreign provider that RECORDS - a
constructor runs before the console is adopted and cannot speak - and a consumer that reads the
record and prints it. The provider carries TWO constructors, the second of which returns without
reaching the step that would clear its marker, because a mechanism that only ever succeeds has no
observable failure. What the gate reads back: `sequence=PQC`, so both provider constructors ran in
priority order and before the consumer's; the completing one completed AND a partial initialisation
was left behind, which are two different questions and would be one flag if this were careless; on a
normal exit the consumer's destructor runs and then its provider's, in reverse of construction across
the DAG; and the same program crashing runs neither. `errno` is observed process-wide by two images
asking for it and getting one address.
THE PROVIDER PRINTS THROUGH A REPORTER THE CONSUMER INSTALLS, which is what makes the destruction
ORDER visible rather than merely recorded: both destructors say something, and the positions of their
two lines are the assertion.

THE AUDIT-LINKED ARTIFACT, THROUGH THE GENERIC CHECKS, AS A FILE. This milestone previously exempted
the one artifact whose surface it claims. It now carries a v2 foreign identity record - each tool by
version and digest, the sysroot, the configure inputs, the objects, the patch series, and the
UPSTREAM'S OWN LICENCE - and passes, per target: relocation forms inside the allowlist, no writable
executable segment, readable dynamic metadata with no `RPATH`/`RUNPATH`/`TEXTREL`, a record
accounting for exactly its `DT_NEEDED` set, and no export a staged provider already owns.
THE ALLOWLIST IS READ FROM THE ONE PLACE THAT DEFINES IT. A table of numbers in the gate would be a
second policy that agrees until it does not, so the gate parses `dynamic_relocation_kind` and fails
out loud on a shape it cannot read.

TWO REAL DEFECTS THE EXPORT-COLLISION CHECK FOUND, both of which would have been invisible until
something was staged:
- The lifecycle fixture had taken `__liber_errno_location` - the substrate's own name. They are never
  staged together today, but "never together today" is not a property anybody is holding fixed. The
  fixture's accessor is now its own name; what is being observed is the SHAPE, and the shape does not
  depend on the name.
- The substrate was defining `memcpy`, `memmove`, `memset` and `memcmp`, which `lsrt.lslib` already
  publishes so that every library in this image can import them - and the audit artifact links
  against it. The substrate no longer exports them, the converged link resolves them from the runtime
  like every other library does, and the derived pin's compiler-runtime answer was corrected: the C
  half's owner is the runtime, not the substrate. The measured split moved with it, 52 asked as 48
  from the substrate and 12 from the runtime, and three gates followed.

THE ONE ITEM THAT REMAINS, AND IT IS BLOCKED ON A MECHANISM RATHER THAN ON WORK. The discovery item's
last requirement is the audit-linked LOADER run in a guest against a synthetic ICD. The artifact
exists, is checked and is reproducible; what does not exist is a way to get it into a guest. Three
routes, each needing something this milestone forbids or has not defined:
- A MANIFEST ROW needs a source row, and the manifest requires a physical source directory. The
  pinned upstream is deliberately not in the tree, and naming it in a manifest is the production
  import this file defers by name.
- A BUILD-TIME INCLUDE into the test kernel would make the test build depend on untracked audit-only
  bytes: the suite would stop building for anyone who has not fetched the upstream, and a test kernel
  whose content varies with a directory outside `src/` is not reproducible.
- A RUNTIME CARRIER - a second disk or a ramdisk the harness supplies - needs no build coupling and
  is the shape that fits, but it is undefined here: what reads it in the guest, how the artifact is
  launched without a manifest path, and how the consumer that calls the loader is built and
  delivered.
I have not invented one. The item records the three routes and says the carrier is the next decision
it needs.

VERIFICATION PERFORMED:
- `./build.sh --arch x86_64` and `./test.sh --arch x86_64`: 389 passed, with the runner walking the
  lifecycle table on every launch in the suite.
- `check-lifecycle`: the four positive gates plus `errno`, in a booted guest.
- `check-foreign-audit-artifact`: all five generic checks on all three targets.
- `check-foreign-audit-link`, `check-foreign-facilities`, `check-profile-sysroot`,
  `check-foreign-cxx-abi`, `check-foreign-pin`, `check-foreign-identity`, `dependency-policy`,
  `artifact-metadata`, `source-hygiene`: all pass after the compiler-runtime correction.
- `check-icd-selection`: still passes; the slot binds and the admitted range holds at both ends.

NOT PERFORMED: `./test.sh` on aarch64 and riscv64, and the full `./check.sh`.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-12T02:42:48Z):

ALL ELEVEN ITEMS. What closed the last one was the QUARANTINE CARRIER, and the carrier turned out to
be one this system already had rather than one that needed inventing: the DEVELOPMENT configuration
is "the gate's own test-only image", which is what this file already called it.

HOW IT WORKS, AND WHY IT BREAKS NOTHING. A program row marked `producer = "audit"` is staged from the
audit link's output when that output exists and is absent otherwise, and is development-only. So: no
production manifest names the artifact; nothing in the ordinary build depends on audit-only bytes; a
tree that has never fetched the upstream builds and tests exactly as before, and the gates that need
the artifact say NOT PERFORMED; and a shipping build actively REMOVES a copy an earlier development
build left behind, because the staged tree is shared between the two. The consumer's own source is in
this tree - one more binary in the tools crate, compiled by the same script every other consumer uses
- and only its LINK is not.

THE PORTED LOADER RUNS. In a guest, against the synthetic ICD bound into its closure through a
selection slot, it answers zero layers, refuses an enabled layer by name both ways, answers out of
its own table, and REACHES THE DRIVER: three negotiations and three entry-point lookups through the
substrate's replaced provider lookup, counted by shims the consumer hands it because the ICD cannot
report it itself - its export surface is exactly two symbols by the kind's own rule, and a counter
would be a third. The instance ends as `VK_ERROR_INCOMPATIBLE_DRIVER`, which is the honest outcome:
the synthetic ICD implements no Vulkan entry point, because this milestone exports no Vulkan ABI.

THE DEFECT THAT GATE FOUND, AND NOTHING ELSE COULD. The loader's search path and the record's paths
never met. The ported build gave the loader `vol://system/share`, and the loader SPLITS a search list
on `:` - so it looked in `vol` and `//system/share`, found nothing, and reported SUCCESS from every
call. Every behavioural assertion passed while no driver was reached at all. The paths carry no
scheme now, and the substrate says which path it refused, so the next time the two do not meet it is
a line in the log rather than a silence. That is the whole argument for running the thing being
ported rather than a selector that stands in for it.

EVERY ADMITTED FACILITY, CALLED AND CHECKED. `abiprobe` calls all of them once and checks each
ANSWER - `strncpy` pads to the full count, `snprintf` returns what it WOULD have written, `fputs` to
anything but the diagnostic stream is refused rather than discarded, an `opendir` outside the record
answers nothing, an uncontended mutex taken twice does not deadlock a process with one thread - and
exits with the number that were wrong. Forty-six checks, zero failures.
THREE ARE RESOLVED AND DELIBERATELY NOT CALLED, said out loud rather than hidden: `abort` and
`__liber_assert_failed` diverge, so a probe that ended in one would report nothing about what it had
already checked, and `vsnprintf` needs a `va_list` Rust cannot construct. The first version of that
line called it with a null format to prove the symbol was there, which is not a test but undefined
behaviour, and the guest faulted on it immediately.

ON ALL THREE ARCHITECTURES, AND THE EVIDENCE IS SPLIT BECAUSE THE PORTS HAVE NO SHELL. aarch64 and
riscv64 boot under emulation and, in this tree, do not reach a shell at all: device bring-up
quarantines the network endpoint and the service graph never starts. That is outside this milestone,
so the LAUNCH is asserted by a kernel test instead, which needs no console - it launches the consumer
through ProcessService, requires the provider closure to verify, and reads the exit status the probe
reports its facility verdict in. The probe's own output is in all three guest logs.

TWO MORE DEFECTS FOUND ON THE WAY, both pre-existing and both invisible until something used the
configuration they were in:
- The DEVELOPMENT build did not compile. Fourteen `unnecessary unsafe` blocks and one genuinely
  missing one, left behind when the runtime's wrappers became safe: the shipping build never
  compiles those files, so nothing had noticed.
- The warm image snapshot did not treat the configuration as an input, so a development build after
  a shipping one found every input unchanged and skipped the phase that stages the quarantine
  artifact - producing a development image without it, silently.

VERIFICATION PERFORMED:
- `./test.sh --arch x86_64`: 389 passed in the shipping configuration.
- `LIBER_DEVELOPMENT=1 ./test.sh --tags dynamic` on x86_64, aarch64 and riscv64: 30 passed on each,
  including the foreign-consumer launch, which ran the probe on every one of them.
- `check-foreign-loader-guest`, `check-foreign-facilities-guest`, `check-icd-selection`,
  `check-lifecycle`: all pass on x86_64.
- The host gate set - dependency-policy, foreign-pin, foreign-facilities, profile-sysroot,
  foreign-identity, foreign-audit-link, foreign-cxx-abi, foreign-audit-artifact, artifact-metadata,
  source-hygiene, milestone-index, staged-consistency: all pass.

NOT PERFORMED: the full `./check.sh`, and `./test.sh` in full on the two ports - only the dynamic tag
ran there.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0135 (2026-09-12T16:27:10Z):

A GATE THIS MILESTONE LEFT RED, FOUND AND CLOSED.

`./src/tools/check-host-tests.sh` was failing on `abi`, and it was this milestone's doing: the
loader work added `SYS_PROCESS_LIFECYCLE = 82` and the `ModuleLifecycle` record to `src/abi`
without registering either in the two snapshots that exist to notice exactly that. Both guards
worked - one said the crate declares 82 syscalls and the snapshot names 81, the other said the crate
declares 15 `repr(C)` structs and 14 have layout assertions - and both had been red since the
commit that added them.

WHY IT MATTERS RATHER THAN BEING A LIST TO TOP UP. The syscall snapshot is what makes an added
syscall a deliberate decision; the layout assertions are what stop a marshalled struct changing shape
under a kernel and a userspace that must agree byte for byte. A snapshot that is behind is not a
weaker check, it is an absent one - and both of these guards were themselves rewritten earlier
BECAUSE a previous version of them compared a list against its own length and stayed green over a
syscall the kernel dispatches.

FIXED: the syscall is named in `SYSCALLS` with its number, and `ModuleLifecycle` has the same
size, alignment and per-field offset assertion every other marshalled struct has - 40 bytes, aligned
to 8, five `u64` fields at 0, 8, 16, 24 and 32.

VERIFIED: `cargo test -p abi` 28 passed, 0 failed. `./src/tools/check-host-tests.sh`: 90 suites,
all passing, exit 0 - where it was 1 of 90 failing before.

AUDITOR'S REVIEW ON P02M0135 (2026-10-15T04:33:46Z):

**Rating: 6/10.** The milestone's machinery is real and mostly sound: the dependency policy and its gate, the three-part lockfile with a digest gate, pass-1 and pass-2 inventories with an exact two-way substrate-surface check (`substrate_built_and_unrequired` empty, declared exports equal to the resolved set), the TLS and thread-creation scans over archive members, the resolved set and the linked ELF, the v2 identity record migrated through all four readers with field-by-field launch refusals in the guest suite, the selection slot resolved before the exact-equality check, the kernel lifecycle table plus the `rt` init/fini runner with guest-observed order, partial-initialisation and crash behaviour, and a ported loader that really reaches an ICD in a guest. Two things the milestone freezes as decisions are, however, not enforced where the code actually runs. The admitted Loader/Driver interface-version set (2..6, refuse 0, 1 and 7+) exists only as a host-tested Rust function and as the synthetic ICD's own self-clamping, while the ported loader still negotiates with upstream's version 7 and accepts anything down to 0. And the lockfile is verified against the tree but is not what the builds read: the option set, the toolchain identity and the generated-source digest are never consumed, the measured configuration is neither the pinned Debug nor the pinned Release build, and the compiled upstream trees under `.build/foreign` are hand-prepared rather than derived from the pinned archives and patch series. Smaller gaps: the build accepts any syntactically valid licence on a foreign manifest row, the audit and quarantine identity records carry constant placeholder digests, and two watched-fail self-tests prove less than the scopes they claim.

## Findings

1. **The admitted ICD interface-version set is enforced only by the synthetic ICD, not by the ported loader or the substrate (Medium, Defect).**
   The milestone freezes "ADMITTED 2, 3, 4, 5 and 6 ... REFUSED, 0 ... REFUSED, 1 ... REFUSED, 7 and above ... The substrate never negotiates above 6, which is what keeps two exports sufficient", requires four refusals to be asserted (an ICD offering 0, one offering only 1, one offering 7, one exporting the physical-device function), and requires the negotiated version to be recorded in the inventory. In the code:
   - The rule exists as `version::negotiate` / `is_admitted` (/data/yellow/libersystem/src/user/libs/foreign/discovery/src/version.rs:13, :51), and nothing outside the host tests (/data/yellow/libersystem/src/user/libs/foreign/discovery/src/tests.rs:186-210) calls it.
   - The substrate hands the loader the ICD's own negotiation entry unchanged: `resolve` returns `icd.negotiate` (/data/yellow/libersystem/src/user/libs/foreign/discovery/src/library.rs:48-50), reached from `loader_platform_get_proc_address` (:96).
   - The platform port (/data/yellow/libersystem/src/foreign/patches/0002-platform-port.patch) does not touch negotiation, so the ported loader keeps `CURRENT_LOADER_ICD_INTERFACE_VERSION 7` and `MIN_SUPPORTED_LOADER_ICD_INTERFACE_VERSION 0` (/data/yellow/libersystem/.build/foreign/src-headers/include/vulkan/vk_icd.h:38-39, offered at /data/yellow/libersystem/.build/foreign/src-loader-port/loader/loader.c:1844). An ICD whose negotiate function answers 7 is accepted at 7 - and the loader then fetches `vk_icdGetPhysicalDeviceProcAddr` through the ICD's own `vk_icdGetInstanceProcAddr` (loader.c:2251), bypassing the by-name refusal in `resolve` - and one that answers 1 is accepted at 1.
   - What keeps every observed run at 6 is the fixture: `icdprobe.c` clamps itself to `ICD_HIGHEST` 6 (/data/yellow/libersystem/src/user/libs/foreign/icdprobe/icdprobe.c:21-40).
   - The "four refusals" gate inverts the roles: `icdcheck` plays the loader and asks the fixture to speak 0, 1 and 7 (/data/yellow/libersystem/src/user/apps/tools/src/icdcheck.rs:64-96), and /data/yellow/libersystem/src/tools/check-icd-selection.sh:87-90 asserts the fixture's answers ("a caller offering 7 must be brought down to 6"). No ICD that offers 0, 1 or 7 exists, so no substrate or loader refusal is ever exercised.
   - The negotiated version is not recorded in /data/yellow/libersystem/src/foreign/INVENTORY-pass2.json (no such key).
   Why it matters: the closed two-export surface the discovery item rests on is a property of one fixture rather than of the port, and the first real driver that negotiates 7 widens it with every gate green. Fix within the port: under `__LiberSystem__` offer at most 6 and refuse answers below 2 (or have `resolve` return a substrate wrapper around `icd.negotiate` that does the same), add ICD fixtures whose negotiate function answers 0, 1 and 7 so the gate exercises the refusal side, and record the agreed version in the pass-2 inventory.

2. **The lockfile is checked against the tree but is not what the builds read, and the compiled upstream trees are not derived from the pinned inputs (Medium, Defect).**
   Done-when requires "the lockfile is committed and is the build's only source for the pinned values", and M1 says "A build that takes any pinned value from anywhere but the PART of the lockfile that owns it fails". Only the gates read the pins (`PIN-*.toml` is referenced by check-foreign-pin.sh and check-profile-sysroot.sh alone); the builders do not:
   - /data/yellow/libersystem/src/tools/build-foreign-static.sh:143-208 hard-codes the define set, the 14-source list and the compile flags. Nothing reads `[options]`, `[toolchain]` or `[generated_sources]` of /data/yellow/libersystem/src/foreign/PIN-bootstrap.toml:43-98, and /data/yellow/libersystem/src/tools/check-foreign-pin.sh never inspects those sections, so they can drift from what is built with no failure.
   - The pin says the inventory is measured on `Debug` and the authoritative closure is `Release` (PIN-bootstrap.toml:64-65). The builder compiles a single `-O2` configuration without `NDEBUG` (build-foreign-static.sh:195), so `assert` stays live and `__liber_assert_failed` is part of the measured 52-symbol surface - a symbol a Release (`NDEBUG`) build does not reference (/data/yellow/libersystem/src/foreign/profile-sysroot/include/assert.h). "Both are recorded, Release is authoritative" is not what was measured.
   - The pinned linker is `ld.lld` and the pinned compiler "Debian clang version 19.1.7" (PIN-bootstrap.toml:97-98); the audit link and the quarantine links use `rust-lld` from the rustc sysroot (/data/yellow/libersystem/src/tools/foreign-audit-link.py:180, :222, :450), and nothing compares the clang version line with the pin.
   - The generated-source digest (PIN-bootstrap.toml:88) is verified nowhere.
   - The trees actually compiled are `.build/foreign/src-loader` and `.build/foreign/src-loader-port` (build-foreign-static.sh:116-124), unpacked and patched "once, by hand"; no tool extracts them from the digest-checked archives or applies the pinned series, and the identity record's `patches-sha256` is computed from the patch files rather than from what was compiled (foreign-audit-link.py:175-176). An edit made directly in `src-loader-port` changes the evidence with every pin still green, and a change to 0002 changes no evidence until somebody re-applies it - which defeats "the derived evidence is regenerated against the frozen patch".
   Fix in scope: one script step that recreates both trees from the archives (digest-checked) plus the pinned series, and a pin check that compares the builder's effective defines and sources, the clang version line and the generated-source digest with the bootstrap part; then either build what the pin says (Debug and Release) or correct the pin to the one configuration actually measured.

3. **The build accepts any licence identifier on a foreign manifest artifact (Low, Defect).**
   Item 1 says "the build rejects an unreviewed licence", and the package gates list "an unreviewed licence" among the watched refusals. The reviewed permissive set lives only in /data/yellow/libersystem/src/tools/check-dependency-policy.sh:48 and is applied only to `third_party/INVENTORY.tsv`. For the foreign artifact kind this milestone created, /data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:1288-1293 (libraries) and :1363-1366 (programs) validate only the identifier's characters, and ProcessService only requires the field to be non-empty (/data/yellow/libersystem/src/user/services/core/src/process_service.rs:212-213). A `producer = "foreign"` row carrying `licence = "GPL-3.0-only"` builds, stages with that licence in its identity record, and launches. No current row does this, but the kind exists to admit future imports. Fix: check foreign and audit rows against the same allowed list the dependency gate uses (one list, shared).

4. **The audit-linked and quarantine identity records carry placeholder digests that bind nothing (Low, Defect).**
   The v2 language section exists to bind the configure inputs, the sysroot and the final objects. In /data/yellow/libersystem/src/tools/foreign-audit-link.py:197-199 the audit-linked ELF's `cflags` is prose and its `configure-sha256` is `sha256("whole-archive audit link " + arch)`; for the quarantine consumers `vkprobe`/`abiprobe` (:507-510) `sysroot-sha256`, `configure-sha256` and `objects-sha256` are hashes of constant strings, so a change to the consumer's object, its configure inputs or the profile sysroot leaves them unchanged. /data/yellow/libersystem/src/tools/check-foreign-audit-artifact.sh:85-87 accepts them because it checks only for 64 hex digits. The production foreign path (/data/yellow/libersystem/src/tools/build-shared.sh:1409-1480) computes real values, so the impact is confined to the development-only artifacts - but those are exactly the records the "generic checks as a file" gate is about. Fix: compute the digests the way build-shared.sh does (the real flags, the profile-sysroot tree digest, the object digest).

5. **Two watched-fail self-tests prove less than the scopes they claim (Low, Optional).**
   - The stop-condition self-test injects its TLS and thread-creation fixture into an archive member and a bare relocatable object only (/data/yellow/libersystem/src/tools/foreign-audit-link.py:365-394). The milestone requires the injection "into each of those three" scopes; the linked-ELF scope and the converged-resolved-set scope (a plain set intersection at :353) are never shown detecting anything.
   - The C++ ABI mutation is one combined fixture carrying exception tables, typeinfo and an `__cxa_atexit` registration at once (/data/yellow/libersystem/src/tools/check-foreign-cxx-abi.sh:72-96), so a regression that stopped one mechanism's pattern from matching would be masked by the other two; "every mechanism marked forbidden fails the build when reintroduced, watched" wants one fixture per mechanism.

## Verification performed

Static, read-only review; no build, test, cargo, QEMU or gate was run.
- Read /data/yellow/libersystem/docs/todo/P02M0135.md in full and the last implementer entries in this file.
- Lockfile: read the three PIN files and /data/yellow/libersystem/src/foreign/profile-abi.sh; read check-foreign-pin.sh, check-profile-sysroot.sh and build-foreign-static.sh; grepped every reader of `PIN-*.toml`, of the generated-source digest and of the pinned toolchain identity.
- Inventories: parsed INVENTORY-pass1.json and INVENTORY-pass2.json (52 symbols asked, 48 from the substrate, 12 from `lsrt`, no TLS, no thread creation, the same constructor and destructor on all three targets); read foreign-inventory.py, foreign-audit-link.py and check-foreign-audit-link.sh (two-way exact-surface check, scope-by-scope stop conditions).
- Substrate and port: read foreign/abi (allocator, sync, lib) and foreign/discovery (record, library, version, lib, tests) and both patches; read the pinned headers and the loader's negotiation code under /data/yellow/libersystem/.build/foreign (read-only).
- Discovery and selection: read service-logic selection.rs, icdprobe.c, icdcheck.rs, vkprobe.rs, check-icd-selection.sh, check-foreign-loader-guest.sh, and the dynamic kernel-suite tests for the slot (14 field mutations, malformed slot, renamed candidate), the TLS runtime refusal and the foreign-consumer launch.
- Lifecycle: read `liber_rt_start` / `run_lifecycle` / `exit_with` in rt, exe-start.rs, the kernel's `lifecycle_of` / `record_lifecycle` / `sys_process_lifecycle`, lifecycle.c and check-lifecycle.sh.
- Identity and licence: read the build-shared.sh identity writer, check-foreign-identity.sh, check-foreign-audit-artifact.sh, check-foreign-cxx-abi.sh, check-dependency-policy.sh, the system-manifest licence validation, the development and staged-image gates' quarantine handling, and gate registration in check.sh and verify-model/src/catalog.rs.

IMPLEMENTER'S RESPONSE ON P02M0135 (2026-10-17T01:44:22Z):

All five findings were verified and accepted.

**Finding 1 - the admitted ICD interface-version set is enforced only by the synthetic ICD (Medium): ACCEPTED.**
- `src/user/libs/foreign/discovery/src/library.rs`:
  - `resolve` no longer hands the loader the driver's own negotiation entry. It hands out the substrate's `bounded_negotiate::<SLOT>`, one per record slot.
  - That entry offers the driver at most 6. It refuses an answer of 0 or 1, of 7 or above, above what was offered, or an error, with `VK_ERROR_INCOMPATIBLE_DRIVER`, and leaves the loader's value untouched.
  - It reports the exchange as a diagnostic line.
- Because the answer is never null, the loader never falls back to asking the driver's own lookup, and nothing above 6 is ever agreed. The physical-device function stays unreachable.
- `version.rs` gains the exchange.
- `liber_foreign_install_icd` takes optional function pointers and refuses NULL.
- `src/foreign/INVENTORY-pass2.json` records the negotiated interface (`_icd_interface`), and `check-foreign-audit-link.sh` holds it to `version.rs`.
- `check-icd-selection.sh` no longer has the roles inverted.
- `check-foreign-loader-guest.sh` expects the loader's line: `icd interface: loader asked 7, substrate offered 6, driver answered 6 - agreed 6`.
- Nine new host tests in `discovery/src/tests.rs` (20 in all) cover 0, 1 and 7 refused, the physical-device function unreachable, and a NULL install refused. Watched failing: six of them fail on the old `resolve`.
- Not done, and recorded in the milestone: the loader's `VK_LUNARG_direct_driver_loading` path does not go through the substrate. Closing it needs a port patch and a guest run.

**Finding 2 - the lockfile is checked but not what the builds read; the compiled trees are not derived from the pinned inputs (Medium): ACCEPTED.**

`src/tools/build-foreign-static.sh`, on every run:
- checks both archives and every patch against their pinned digests;
- derives `src-headers`, `src-loader` (0001) and `src-loader-port` (the derived series, in order, no fuzz) under a lock, and replaces a tree that differs;
- refuses a compiler or archiver whose version line is not the pinned one;
- takes the sources from upstream's own `NORMAL_LOADER_SRCS`;
- before compiling, holds its options, defines, flags, `[build]` section, generated-source digest and cross ABI to the bootstrap part;
- writes its configuration file.

New modes: `--check-lockfile` and `--build-type debug|release`. `check-foreign-pin.sh` compares the tool lines and licences and runs `--check-lockfile`; ten mutations to the lockfile are each refused by name.

The pins now record what was actually built and measured:
- `PIN-bootstrap.toml`:
  - three options that do not exist in the pinned revision are recorded `absent`;
  - `[build]` pins the measured configuration (-O2, assertions live);
  - `[toolchain]` names `rust-lld` and the full clang line.
- Debug and Release are both measured against it: Debug gives 52 symbols on all three targets, Release 51 (without `__liber_assert_failed`).
- `PIN-static-target.toml` and `PIN-derived.toml` are refrozen.

Making Release authoritative would mean removing `__liber_assert_failed` from the substrate's exact surface, which is outside this fix. The pin was corrected to the configuration that is built and measured instead, as the finding allows.

**Finding 3 - any licence identifier is accepted on a foreign artifact (Low): ACCEPTED.**
- The reviewed set is one file, `src/tools/system-manifest/src/reviewed-licences.txt`. `check-dependency-policy.sh` reads it.
- `system-manifest` compiles it in (`include_str!`) and refuses any declared licence outside it, except `project`, on library and program rows.
- Two new `system-manifest` tests, watched failing: `GPL-3.0-only` and `BSD-4-Clause` are refused, `Apache-2.0` and `project` are accepted.

**Finding 4 - the identity records carry placeholder digests (Low): ACCEPTED.**
- `src/tools/foreign-audit-link.py` computes real cflags, linker, sysroot, configure, objects and source digests for the audit-linked ELF and for both quarantine consumers.
- It gains `--verify-records`, which `check-foreign-audit-artifact.sh` runs on the nine records. It also checks the sysroot against the pin.
- Watched failing: the old records are refused.

**Finding 5 - two watched-fail self-tests prove less than they claim (Low, Optional): ACCEPTED.**
- The stop-condition self-test injects into all three scopes, including a linked ELF carrying `PT_TLS`, with a clean control.
- `check-foreign-cxx-abi.sh` has one fixture per forbidden mechanism (five), with a clean control.
- Each mutation was watched being caught.

The milestone records all five in dated notes. The verification is below.

**Verification:**
- `host-tests` passed, the discovery library's 20 tests among them. `icd-selection` and `foreign-facilities-guest` passed.
- `foreign-loader-guest` passed, but only after one correction to the gate itself.
  - Its first run failed on the line finding 1 added. `guest-gate.sh` filters a probe's lines through `prefix: [a-zA-Z0-9 =_.,-]*`, which stops at a colon, so "vkprobe: foreign: icd interface: ..." could never be found whole.
  - `check-foreign-loader-guest.sh` now reads that line, and the refusal check, from the guest's whole log.
  - On the rerun the guest said "foreign: icd interface: loader asked 7, substrate offered 6, driver answered 6 - agreed 6", with 3 negotiations and 3 lookups.
- `kernel.dynamic.dynamic_foreign_consumer_launches_and_exercises_every_admitted_facility` ran on the ports in the development configuration, where `abiprobe` is staged:
  - riscv64 passed: 46 checks, 0 failures, 8 s.
  - aarch64's first run printed nothing past the test's name for 2400 s, and the harness stopped it.
  - Between that run and the next, `foreign-audit-link.py --check` relinked the audit-linked pass-2 artifacts on all three targets. The rerun on a fresh development build passed: 46 checks, 0 failures, 7 s.
  - So the first build most likely staged an `abiprobe` older than the substrate. That is not proven, and the hang did not recur.
  - In the shipping configuration the test says that `abiprobe` is not staged. It is development-only.
- The shipping builds for aarch64 and riscv64 passed, and so did their development builds.

AUDITOR'S RE-AUDIT ON P02M0135 (2026-10-18T17:31:27Z):

**Rating: 8/10.** All five findings of the 2026-10-15 review were accepted, and the fixes are in the tree.
- **Finding 1.** The substrate now hands the ported loader its own negotiation entry point, one per record slot. That entry point offers the driver at most 6. It refuses an answer of 0 or 1, of 7 or above, an answer above the offer, and any error. It reports each exchange, and the installer refuses a null entry point. The pass-2 inventory records the 2..6 range, and the audit-link gate checks that range against `version.rs`.
- **Finding 2.** The builder rebuilds both trees from the pinned archives and the pinned patch series, checking every digest. It refuses a compiler or archiver whose version line differs from the pin, and it reads `[build]` from the lockfile. Before compiling, it checks each option's effect, the generated-source digest and the cross ABI. The pin now describes the configuration that is actually built and measured (-O2, assertions live, no debug info), and `PIN-bootstrap.toml` [build] and the milestone (`docs/todo/P02M0135.md:380-387`) document this, as the owner decided. `rust-lld` and the full clang version line are pinned and enforced.
- **Finding 3.** The reviewed licence list is now one file, read by both the dependency gate and the manifest.
- **Finding 4.** The identity records are computed from the inputs of the link that produced them, and the artifact gate recomputes and compares every field (`--verify-records`).
- **Finding 5.** The stop-condition self-test now covers all three scopes, and the C++ ABI gate has one fixture per mechanism plus a clean control.

Every pinned digest I recomputed matches the tree. What remains is one gap the implementer wrote down as "NOT COVERED" while the milestone still says COMPLETE. The loader's `VK_LUNARG_direct_driver_loading` path is a second way to load a driver, and the substrate's version bound does not apply on it.

## Findings

1. **Medium, Incomplete fix - the admitted interface-version set and the third-export refusal can still be bypassed through the ported loader's `VK_LUNARG_direct_driver_loading` path, yet the milestone is marked COMPLETE.**
   - **What the plan requires.**
     - `/data/yellow/libersystem/docs/todo/P02M0135.md:1047-1051`: "REFUSED, 7 and above ... The substrate never negotiates above 6, which is what keeps two exports sufficient".
     - `:1052-1056`: a driver that requires `vk_icdGetPhysicalDeviceProcAddr` "is outside this profile and is refused by name".
     - `:839`: "Choose ONE loader/ICD discovery model".
     - `:473-475`: such paths "are removed by the port rather than left unreachable. A path nothing calls is still a path the closure admits".
   - **What the code does.**
     - The fix covers only the `loader_platform_get_proc_address` route (`/data/yellow/libersystem/src/user/libs/foreign/discovery/src/library.rs:58-74, 91-115`).
     - The ported loader still advertises the extension (`/data/yellow/libersystem/.build/foreign/src-loader-port/loader/loader.c:1653-1658`).
     - `vkCreateInstance` scans drivers (`trampoline.c:666`) before it validates extensions. That scan calls `loader_scan_for_direct_drivers` (`loader.c:4073`). In EXCLUSIVE mode it skips the scan the substrate's record answers (`:2076-2077`, `:4077`).
     - `loader_add_direct_driver` takes the negotiation function from the application-supplied `pfnGetInstanceProcAddr` (`:1916-1917`). It negotiates at 7 with no substrate in between (`:1928`, `:1844`), refuses any version below 7 (`:1938`), and fetches `vk_icdGetPhysicalDeviceProcAddr` through the driver's own lookup (`:1966-1967`).
     - `/data/yellow/libersystem/src/foreign/patches/0002-platform-port.patch` (unchanged since 2026-09-11) has no hunk for this path.
     - Both functions are linked into the quarantine consumer: `llvm-nm` on `/data/yellow/libersystem/.build/foreign/pass2/x86_64/vkprobe.lsexe` shows `loader_add_direct_driver` and `loader_scan_for_direct_drivers`.
     - The milestone says so itself at `/data/yellow/libersystem/docs/todo/P02M0135.md:878-880` ("NOT COVERED ... refusing it needs a port-patch hunk and a guest to prove it"), while `:3` says "COMPLETE - every item below is done" and the item is ticked at `:839`.
   - **Why it matters.** Any consumer that holds the slot-bound driver's `vk_icdGetInstanceProcAddr` can enable the extension and pass that lookup to the loader (vkprobe already imports it, `/data/yellow/libersystem/src/user/apps/tools/src/vkprobe.rs:38-39`). The loader then agrees version 7 with that driver and takes the physical-device function from its lookup. That is the widening finding 1 described, through a second entry, with every gate green. The response's claim "nothing above 6 is ever agreed" holds for only one of the loader's two driver-loading paths.
   - **Minimal fix.**
     - Add a hunk to 0002 under `__LiberSystem__` that makes `loader_scan_for_direct_drivers` refuse or ignore a `VkDirectDriverLoadingListLUNARG` before any driver is called, and stop advertising the extension. Dropping it from the advertised list alone is not enough, because the scan runs before extension validation.
     - Refreeze the derived pin.
     - Add one vkprobe step that enables the extension with the slot-bound lookup, and have `foreign-loader-guest` assert that it is refused.
     - Until then the item should not read as done.

## Verification performed

- **Documents read.**
  - `/data/yellow/libersystem/docs/todo/P02M0135.md` in full.
  - `/data/yellow/libersystem/AI/audit/code-audit-P02M0135.md:824-939`, with earlier sections for context.
- **Code read in the current tree.** HEAD f6925965. Fix commits 58fb476c and 7ee3e9bf.
  - The discovery crate (`library.rs`, `version.rs`, `record.rs`, `lib.rs`, `tests.rs`).
  - `vkprobe.rs`, `abiprobe.rs` (calls only), `icdprobe.c` and its README, the foreign-abi `fputs`/sink/stderr code.
  - The three PIN files and the two inventories.
  - `build-foreign-static.sh`, `check-foreign-pin.sh`, `foreign-audit-link.py`, `foreign-inventory.py`.
  - `check-foreign-audit-link.sh`, `check-foreign-audit-artifact.sh`, `check-foreign-cxx-abi.sh`, `check-icd-selection.sh`, `check-foreign-loader-guest.sh`, `guest-gate.sh`.
  - `system-manifest` `lib.rs`/`tests.rs`/`reviewed-licences.txt`, `check-dependency-policy.sh`.
  - The kernel test `dynamic_foreign_consumer_launches_and_exercises_every_admitted_facility`.
  - The derived loader tree's negotiation and direct-driver code (`loader.c`, `trampoline.c`) and upstream's `CMakeLists.txt`.
- **Claims checked.**
  - `resolve` hands the loader the bounded per-slot entry point, which offers at most 6, refuses 0/1, 7 and above, answers above the offer, and errors, leaves the loader's value untouched, and reports the exchange: confirmed (`library.rs:58-137`, `version.rs:120-173`).
  - "Never null, so no fallback; physical-device function unreachable": confirmed for `loader_scanned_icd_add` (`loader.c:2149-2161, 2251-2256`). Not confirmed for the direct-driver path (finding 1).
  - `liber_foreign_install_icd` refuses NULL: confirmed (`record.rs:129, 151-153`; `tests.rs:436-446`).
  - `_icd_interface` is in the inventory and is checked against `version.rs`: confirmed (`foreign-audit-link.py:511-538`, `check-foreign-audit-link.sh:107-120`).
  - icd-selection no longer claims the refusals: confirmed (`check-icd-selection.sh:27-34, 89-98`).
  - foreign-loader-guest asserts the line from the whole log: confirmed (`check-foreign-loader-guest.sh:104-114`). `GUEST_LOG` is private to each run (`guest-gate.sh:151`).
  - Nine fixture drivers, 20 tests in all: confirmed by count (`tests.rs:322-330, 358-493`). "Six fail on the old `resolve`": plausible, not checkable statically.
  - Builder digest-checks archives and patches, derives the trees under a lock and replaces any that differ: confirmed (`build-foreign-static.sh:191-251`).
  - Builder refuses another compiler or archiver: confirmed (`:185-188`). This machine's clang, llvm-ar and the root toolchain's `rust-lld` print the pinned lines.
  - Sources come from upstream's `NORMAL_LOADER_SRCS`: confirmed (`:370-371`, 14 sources).
  - Builder checks options, `[build]`, the generated-source digest and the cross ABI, and writes its configuration file: confirmed (`:373-534`). The three "absent" options really are absent from upstream's CMake.
  - `check-foreign-pin.sh` compares the tool lines and licences and runs `--check-lockfile`: confirmed (`:228-266, :300-311`).
  - "Ten lockfile mutations refused": not confirmable statically. There is no committed self-test.
  - Debug 52 / Release 51: recorded only as prose in `PIN-bootstrap.toml:117-121`; accepted per the owner's decision.
  - The corrected pin is implemented and documented: confirmed. The option-table row at `docs/todo/P02M0135.md:322-326` still reads "Release ... authoritative", but the dated note at `:380-387` supersedes it.
  - Refrozen digests (builder, ABI, cross files, both sysroots, generated sources, inventory, tool): all recomputed and equal to the pins.
  - Finding 3, licences: confirmed (`system-manifest/src/lib.rs:1305-1308, 1382-1383, 1879-1884`; `check-dependency-policy.sh`; `check-foreign-pin.sh:260-266`; two new tests).
  - Finding 4, real identity digests and `--verify-records` wired into the gate: confirmed (`foreign-audit-link.py:272-311, 669-777`; `check-foreign-audit-artifact.sh:97, 126`). I recomputed the source, objects, patches and sysroot digests of the three `audit-vulkan` records from their current inputs, and they match.
  - Finding 5: confirmed (`foreign-audit-link.py:541-614`; `check-foreign-cxx-abi.sh:104-179`).
  - Guest and port runs (foreign-loader-guest x86_64 PASS, icd-selection, foreign-facilities-guest, riscv64 and aarch64 kernel test): not confirmable statically. No logs are kept; the gate's work directory is removed on exit.
  - The aarch64 2400 s hang: the cause is unexplained, as the response itself says, and a static reading cannot settle it. abiprobe never goes through the new negotiation code.
- **Not done.** This was a static review only: nothing was built or run. I used read-only inspection only: tool version queries, `llvm-nm`/`llvm-readelf`/`llvm-objcopy` section dumps to /dev/null, and recomputing digests. No file was changed.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON P02M0135 (2026-10-19T22:00:37Z):

**Finding 1 - the admitted interface-version set and the third-export refusal can still be bypassed through the ported loader's `VK_LUNARG_direct_driver_loading` path, yet the milestone is marked COMPLETE (Medium): ACCEPTED.**
- Verified first: in the derived tree `vkCreateInstance` scans drivers before it validates extensions, the scan calls
  `loader_scan_for_direct_drivers`, and `loader_add_direct_driver` negotiates at 7 with the application's lookup and
  takes `vk_icdGetPhysicalDeviceProcAddr` from it - no substrate in between; in exclusive mode it skips the record's
  scan.
- `src/foreign/patches/0002-platform-port.patch` gains three `loader.c` hunks under `__LiberSystem__`:
  - `loader_scan_for_direct_drivers` calls nothing: a `VkDirectDriverLoadingListLUNARG` in the create info is ignored,
    exclusive mode stays off, and the record's scan runs as it always does;
  - `loader_add_direct_driver` is removed rather than left unreachable - a path nothing calls is still a path the
    closure admits;
  - the extension is no longer advertised, so with a driver that creates instances an instance that ENABLES it is
    refused with `VK_ERROR_EXTENSION_NOT_PRESENT` by upstream's own validation, which runs after the scan and before any
    driver is created (read from the code; the guest's driver stops the loader earlier - below).
  The patch's description says so; the external symbol surface does not change (the path called only the loader's own
  functions and the pointers it was handed).
- `src/foreign/PIN-derived.toml`: the 0002 digest, the pass-2 inventory digest and the three ELF sizes refrozen (x86_64
  618152 to 614720 bytes, aarch64 639992 to 636504, riscv64 957784 to 952816; the relink of all three targets left every
  other field of the inventory as it was - 52 symbols asked, 48 from the substrate, 12 runtime references, no TLS).
- `src/user/apps/tools/src/vkprobe.rs`: step 9 lists the instance extensions and creates an instance enabling the
  extension with a `VkDirectDriverLoadingListLUNARG` (exclusive mode) naming the slot-bound driver's own
  `vk_icdGetInstanceProcAddr` through a counting shim. It reports whether the extension was advertised, the result, and
  how often the lookup was called.
- `src/tools/check-foreign-loader-guest.sh` requires "direct driver loading not advertised", "direct driver lookups=0",
  "direct driver loading refused", and the creation's result to be the ordinary instance's in that guest, 4294967287
  (`VK_ERROR_INCOMPATIBLE_DRIVER`).
- Corrected on the final run: I had required 4294967289 (`VK_ERROR_EXTENSION_NOT_PRESENT`), from reading upstream's
  order
  - extensions are validated after the driver scan. The guest answered `VK_ERROR_INCOMPATIBLE_DRIVER`, as its ordinary
  instance does: its synthetic driver implements nothing, so the loader stops before the validation. The probe's verdict
  is now "not created, and the lookup never called"; the -7 for a driver that creates instances stays a reading of the
  code, not a run. With the port as it was, the same step showed the path live: the extension listed and the
  application's lookup called once (the same -9 result).
- `docs/todo/P02M0135.md`: the "NOT COVERED" note is closed by the Corrections of 2026-10-19.
- Also, under the owner's rule on milestone ids: the three PIN files, the bootstrap sysroot's README (whose tree digest
  is refrozen in `PIN-bootstrap.toml`, "for a README change and nothing else"), `check-foreign-pin.sh` and
  `docs/DEPENDENCY_POLICY.md` no longer name the milestone.

**Verification:**
- Host: the discovery and foreign crates' suites pass in `host-tests`.
- The relink of all three targets reproduces the recorded inventory; `foreign-pin`, `foreign-audit-link`,
  `foreign-audit-artifact` and `foreign-cxx-abi` PASS; `icd-selection` PASS (473 s).
- `foreign-loader-guest` PASS (477 s): "direct driver loading not advertised", "direct driver instance
  result=4294967287", "direct driver lookups=0", "direct driver loading refused". On the port as it was: advertised, the
  same result, "direct driver lookups=1", "NOT refused" - the path was live.

**Milestone status (verified after these changes):** COMPLETE. Every item is done and every finding is fixed, with
`foreign-loader-guest` showing the direct-driver path refused on the final tree.
