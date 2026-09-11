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
