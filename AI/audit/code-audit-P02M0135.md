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
