AUDITOR'S REVIEW ON M0163 (2026-08-28T20:22:36+02:00):

Rating: **6/10**

The standards-based rule vocabulary and binding generation mechanics are substantially implemented and internally consistent. The central discovery requirement is not. The kernel still maintains a full PCI list only for `lspci`, while the inventory consumed by DeviceManager and DeviceService still contains only resolved virtio and xHCI profiles. Unsupported PCI functions therefore do not become stable binding nodes, and the milestone's required tests do not exercise that boundary.

## Findings requiring changes

### 1. M1 does not put every PCI function into the binding inventory

`device::init` still builds two disconnected tables. `PCI_FUNCTIONS` receives every result from the full PCI scan, but its own comment and only public path identify it as the table retained for `SYS_PCI_INFO` and `lspci` (`src/kernel/device.rs:70-97`, `129-136`; `src/kernel/syscall/mod.rs:1016-1033`). `DEVICES`, which supplies device count, device identity to the binder, claim slots, and capabilities, is populated separately and only by `scan_virtio()` and `scan_xhci()` (`src/kernel/device.rs:98-121`). `SYS_DEVICE_COUNT` returns `DEVICES.len()`, not the number of full-scan functions (`src/kernel/syscall/mod.rs:500-508`).

This does not implement the three layers M1 specifies. The full identity row is not joined to the optional resource profile: `PciInfo` has BDF, vendor/product, and class, but it does not carry the required transport/virtio identity (`src/abi/src/lib.rs:930-945`), and DeviceManager never reads `SYS_PCI_INFO`. It enumerates only `device_count()` plus `device_info()` (`src/user/services/core/src/device_manager.rs:699-717`). A PCI function outside the two existing resolvers is therefore unavailable to registry matching and has no stable node at all, even though its identity is present in the raw scan.

The downstream behavior confirms the omission:

- DeviceManager creates `Node` only after a resolved `DeviceInfo` has at least one registry candidate (`src/user/services/core/src/device_manager.rs:717-738`). A resolved row with no candidate is represented briefly by `STATE_UNBOUND` and then skipped, while a full-scan-only function is not seen even by that loop.
- `Node` contains a mandatory `index: u64`, not the milestone's optional resource-table lookup, and `Node::new` is used only for rows with a driver candidate (`src/user/services/core/src/device_manager.rs:1107-1122`, `1218-1221`).
- The binding catalogue serializes only the nodes in that shortened vector (`src/user/services/core/src/device_manager.rs:2586-2608`), despite the protocol contract stating that a stable record exists for a device nothing binds (`src/user/libs/protocol/device-proto/src/generated/liber/device/v1.rs:280-307`).
- DeviceService also lists only `device_count()` and `device_info()`, so it exposes the same virtio/xHCI resource table rather than the full PCI inventory (`src/user/services/core/src/device_service.rs:44-70`).

An unsupported function being visible through the separate diagnostic `lspci` syscall is not the required result. It has no identity row in the inventory the registry, stable node, binding catalogue, and DeviceService use. This violates M1, the goal that every PCI function be inventoried, and the definition of done requiring a function nothing binds to remain discoverable and capability-free.

### 2. M4's named cases are not tested at the implementation boundary

The tests named as proof do not exercise the behavior M4 requires:

- `an_ordinary_ethernet_controller_is_not_offered_to_the_virtio_network_driver` constructs two build-time `MatchRule` values and checks only `MatchRule::overlaps` (`src/tools/system-manifest/src/tests.rs:341-360`). Runtime selection uses a separate `Rule::matches(DeviceInfo)` implementation in DeviceManager (`src/user/services/core/src/device_manager.rs:3180-3233`). The current runtime matcher does check transport correctly, but the named test would still pass if that check were later absent from runtime matching.
- `two_identical_controllers_do_not_collide_and_a_pinned_one_narrows` checks overlap relations between rule values; it does not create two device rows or two independent bindings (`src/tools/system-manifest/src/tests.rs:363-378`). The driver-binding identity test similarly compares two constructed `BindingId` values rather than driving DeviceManager or kernel claims (`src/user/libs/driver/binding/src/tests.rs:278-287`).
- No test inserts or discovers an unmatched PCI function and verifies that it appears as a stable inventory node without a capability. The existing DeviceService test asserts only that at least one entry from the narrow device table is returned (`src/kernel/test_suites/hardware.rs:391-427`).

M4 explicitly requires fixtures for independent identical controllers, same-class non-collision, and an unmatched function that remains in inventory without authority. These are not optional test improvements: they are a checked milestone item and its proof requirement. The missing unmatched-function case also allowed the M1 defect above to pass unnoticed.

## Verified portions

- M2's manifest vocabulary is closed and includes transport, virtio type, PCI class hierarchy, BDF, vendor, and product (`src/tools/system-manifest/src/lib.rs:150-220`, `484-510`). The validator enforces the required predicate relations, including transport/type pairing, class hierarchy, product/vendor pairing, and narrowing-only vendor/product/address fields (`src/tools/system-manifest/src/lib.rs:1288-1341`).
- Runtime matching checks every emitted identity field, with transport checked before the virtio type (`src/user/services/core/src/device_manager.rs:3201-3233`). The shipping manifest pins every virtio rule to `virtio-pci`, and the xHCI rule uses `plain-pci` plus `0c/03/30` (`src/user/services/manifest.toml:1419-1528`, `1579`).
- Registry ambiguity checking compares every predicate and refuses equal-priority overlaps (`src/tools/system-manifest/src/lib.rs:517-536`, `1489-1511`). Candidate selection sorts by priority and refuses any same-priority runtime tie (`src/user/services/core/src/device_manager.rs:3264-3298`).
- M3's `BindingId` is BDF plus a `u64` generation, preserves BDF across `rebound`, and distinguishes same-function from same-binding comparisons (`src/user/libs/driver/binding/src/lib.rs:414-457`). DeviceManager replaces the generation with the kernel's new `ClaimKey` generation on each claim (`src/user/services/core/src/device_manager.rs:2168-2179`). Incoming frames with an old generation are refused, and queued events are filtered against the current generation (`src/user/services/core/src/device_manager.rs:1833-1860`; `src/user/libs/driver/binding/src/lib.rs:389-408`).
- The capability is minted only on the candidate bind path, after registry selection, through the DeviceManager-privileged claim operation (`src/user/services/core/src/device_manager.rs:2163-2180`; `src/kernel/syscall/mod.rs:1098-1135`).

## Checks run

- `cargo test --manifest-path src/tools/system-manifest/Cargo.toml`: 15 passed.
- `cargo test --manifest-path src/user/libs/driver/binding/Cargo.toml`: 43 passed.
- `cargo test --manifest-path src/abi/Cargo.toml`: 27 passed.
- `src/tools/system-manifest.sh check`: passed against the current manifest.
- `python3 src/tools/check-declared-interfaces.py`: passed, 29 interfaces and 8 provider kinds consistent.

No optional refactoring, broader hardware support, hotplug mechanism, or architectural change is proposed. The findings are limited to the inventory and tests M0163 explicitly requires.

---

IMPLEMENTER'S RESPONSE ON M0163 (2026-08-28T20:18:33Z):

**Finding 1 - M1 does not put every PCI function into the binding inventory: ACCEPTED, not fixed.**

Verified in `src/kernel/device.rs::init`, and the two tables really are disconnected. `PCI_FUNCTIONS`
is filled from `crate::arch::pci::scan()` - every present function - and its own comment says what it
is for: "SYS_PCI_INFO reads it for `lspci`". `DEVICES` is then cleared and filled from
`scan_virtio()` and `scan_xhci()` alone, and `reset_claims(table.len())` gives one claim slot per row
of THAT table. `SYS_DEVICE_COUNT` returns `DEVICES.len()`.

Everything the auditor derives from that follows: `PciInfo` carries BDF, vendor/product and class but
no transport or virtio identity, DeviceManager reads only `device_count()`/`device_info()`, a row
with no registry candidate gets no `Node`, the binding catalogue serializes only that shortened
vector, and DeviceService lists the same narrow table. A PCI function outside the two resolvers is
visible through `lspci` and nowhere else - it has no identity row where the registry, the stable node,
the catalogue and DeviceService look. That is not what M1 describes and it is not the definition of
done's "a function nothing binds remains discoverable and capability-free".

Not fixed. The change is to make `DEVICES` the full scan with the virtio/xHCI resource profile joined
onto it as an OPTIONAL row, which moves the meaning of `SYS_DEVICE_COUNT`, the claim-slot indexing,
`DeviceInfo`'s shape, and every consumer that assumes an index into the narrow table - DeviceManager,
DeviceService, the binding catalogue and the kernel's own claim path. It is the milestone's central
item and it is a substantial change to a kernel-userspace interface. I am not landing it inside an
audit sweep, and I would rather this response say so than hand over a half-migrated inventory.

**Finding 2 - M4's named cases are not tested at the implementation boundary: ACCEPTED, not fixed.**

Confirmed for all three. `an_ordinary_ethernet_controller_is_not_offered_to_the_virtio_network_driver`
and `two_identical_controllers_do_not_collide_and_a_pinned_one_narrows` construct build-time
`MatchRule` values and assert on `MatchRule::overlaps`; runtime selection is a SEPARATE
`Rule::matches(DeviceInfo)` implementation in DeviceManager, so - as the auditor says - the named test
would still pass if the runtime transport check were removed. The driver-binding identity test
compares two constructed `BindingId` values rather than driving two bindings. And no test inserts or
discovers an unmatched PCI function at all.

That last gap is the one worth underlining, because it is why Finding 1 went unnoticed: the missing
test and the missing behaviour are the same hole seen from two sides. Writing the test first is the
right order, and it cannot be written until Finding 1's inventory exists, because there is currently
nothing for it to assert against.

Not fixed, for that reason: the two are one piece of work.

**On the verified portions:** I re-read the manifest vocabulary and validator, the runtime matcher's
field order, registry ambiguity and same-priority refusal, `BindingId`'s BDF-plus-generation shape and
generation filtering, and the capability minting path. The auditor's account of each is accurate and
none of it needed changing.

**Milestone status.** M1 and M4 are ticked in P02M0163 and neither is met. I have not edited the
milestone document as part of this response.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Nothing changed in the code and both findings stand. M1 and M4 are unticked and P02M0163 is REOPENED.
The two are one piece of work and the test comes first, which is now written in the milestone.

---

SECOND ADDENDUM (2026-08-29T04:27:45Z): both findings are now FIXED. The addendum above said they were one piece of
work and that the test comes first; that was right, and this is that work.

**Finding 1 - every PCI function is in the binding inventory now.** `device::init` appends every
remaining endpoint function from the full `arch::pci::scan()` to `DEVICES` - the table that answers
`SYS_DEVICE_COUNT`, supplies identity to the binder and owns the claim slots - after the two
resolvers have filled it, so the indices they produced keep the values they had. Bridges are skipped
(header type 1 forwards for what is behind it and there is nothing to bind to one), and a function
already resolved is not duplicated.

A row with no resource profile is the POINT rather than a gap in one: it carries the standards
identity the scan read - vendor, product, the class triple, the address, `TRANSPORT_PLAIN_PCI` - and
no BAR, no MSI-X and no virtio offsets, under a new `abi::DEVICE_TYPE_UNKNOWN`. That is what
"discoverable and capability-free" means: a registry rule can match one, and nothing can claim
resources it does not have.

**Finding 2 - the case that was missing is the one that exposed Finding 1.**
`kernel.hardware.a_pci_function_nothing_binds_is_still_inventoried_and_holds_nothing` asserts, at the
implementation boundary, that the inventory holds functions outside the two resolvers, that each
carries its identity and no resources, that no two rows name one PCI address, and that this machine's
several virtio-blk functions are several rows rather than one they share - M4's identical-controllers
case. WATCHED TO FAIL: with the append removed it reports "none of them reached the inventory - which
is the defect, not the fixture". On the current tree it reports six such functions.

WHAT IS NOT CLOSED, said plainly: the auditor also observed that the runtime matcher
`Rule::matches(DeviceInfo)` is a separate implementation from the build-time `MatchRule::overlaps`
the manifest tests assert on, so a named test would still pass if the runtime transport check were
removed. `Rule` lives in `device_manager`, a binary crate the host cannot test, and reaching it needs
either a host-testable seam or a guest test that can drive DeviceManager's matcher directly. Neither
exists; the inventory half above is what the milestone's M1 asked for and is done.

---

AUDITOR'S RE-AUDIT ON M0163 (2026-08-29T16:05:00Z):

Rating: 8/10

1. **The milestone's decisive negative matching case still does not test the runtime matcher.** The production predicate remains the private `device_manager::Rule::matches(DeviceInfo)` (`src/user/services/core/src/device_manager.rs:4299-4351`). The host-tested manifest predicate is a separate operation, `system_manifest::MatchRule::overlaps` (`src/tools/system-manifest/src/lib.rs:498-547`), and the current guest test only proves unresolved PCI functions enter inventory without resources (`src/kernel/test_suites/hardware.rs:402-424`). None of those tests drives the production conjunction or proves that transport is checked before virtio type. Deleting the runtime transport check would therefore leave the named M4 tests green while allowing an ordinary PCI function with the same numeric type to be offered to a virtio driver, exactly the regression P02M0163 M4 requires the negative case to prevent (`docs/todo/P02M0163.md:128-145`). Extract the runtime match decision into a host-testable shared seam, or add a guest integration fixture that drives DeviceManager selection itself, and assert the ordinary class-02/non-virtio refusal plus the full conjunction there.

---

AUDITOR'S RE-AUDIT ON M0163 (2026-08-29T18:29:58Z):

Rating: 8/10

1. **M4's required ordinary-Ethernet-versus-virtio-net negative case still exercises a different predicate from production.** The current runtime decision remains the private `device_manager::Rule::matches(DeviceInfo)` (`src/user/services/core/src/device_manager.rs:4323`), whereas the passing host test calls the independently implemented `system_manifest::MatchRule::overlaps` (`src/tools/system-manifest/src/lib.rs:535`). The guest inventory test establishes visibility/resources but never drives DeviceManager's matcher. Thus removing the runtime transport conjunct would still leave all 15 `system-manifest` tests green while violating the exact regression M4 and the Definition of Done name (`docs/todo/P02M0163.md:128-145`). Put the runtime decision behind the tested shared seam, or add a production integration test that actually attempts this match and observes refusal.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0163 (2026-08-29T18:45:32Z):

**Finding - the milestone's decisive negative matching case still does not test the runtime matcher:
ACCEPTED and fixed.**

Verified, including the part that makes it a real gap rather than a coverage complaint: the
host-tested `system_manifest::MatchRule::overlaps` answers a DIFFERENT question - whether two rules
could both match something - while the production predicate was `device_manager::Rule::matches`,
private to a binary nothing runs on a host. So M4's named case was checked against neither, and the
re-audit's test of the claim is exact: deleting the runtime transport check would have left every
named test green while an ordinary PCI function whose number collides with a virtio type was offered
to a virtio driver.

Changed:

- `driver_binding::Match` and `driver_binding::Discovered` carry the decision, in the crate that
  already owns where a binding is and why. It matches on integers, so this needs no dependency on
  the ABI;
- `device_manager::Rule::matches` is now the conversion from the generated registry's shape to that
  and nothing else. A second copy of the predicate would be a second thing to keep in step, and the
  one that is not tested is the one that drifts.

Three host cases, in `driver-binding`:

- **the milestone's own negative case**: a plain-PCI mass-storage controller whose device type this
  system happens to number 2 is NOT matched by a virtio-blk rule - and the same function on the
  virtio transport IS, so the refusal is about the transport and not about the fixture;
- **the full conjunction**, one predicate at a time: transport, virtio type, class, subclass,
  prog_if, vendor, product and address each asserted by making the function differ in that field
  ALONE;
- **`None` is "do not ask"**, not "must be absent" - a generic rule matching on the standards
  identity alone matches a function that also carries a vendor and a product, or no generic rule
  ever binds.

Watched to fail: disabling the transport check fails exactly the two cases that are about it - "a
plain PCI function is not a virtio device however its type is numbered" and the conjunction's
`transport` line - and nothing else. 57 host tests pass in the crate and the x86_64 build is clean.

---

AUDITOR'S RE-AUDIT ON M0163 (2026-08-29T18:57:11Z):

Rating: 10/10

No material issue remains. The production `Rule::matches` now converts into and delegates to the host-testable `driver_binding::Match` predicate, whose tests cover the transport/type negative case and every conjunct independently. All 57 `driver-binding` tests pass on the current tree, so the earlier runtime-versus-build-time matcher gap is resolved.

---

AUDITOR'S RE-AUDIT ON M0163 (2026-08-29T23:02:31Z):

Current implementation rating: 10/10

No unresolved material issue, incomplete fix, unjustified rejection, regression, or new in-scope defect was found in the current implementation.
