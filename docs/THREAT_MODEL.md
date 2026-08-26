# Threat model

This document records what LiberSystem defends against, where the security
boundaries are, and how each boundary is turned into an executable check. It is
the written threat model called for by the Concept's *Open questions* (item 12,
"an explicit threat model"), backed by the testing strategy of item 11 ("unit,
integration on QEMU, syscall fuzzing, property tests of capabilities").

The system's security rests on one structural property: **no ambient authority**.
A component can name a thing it cannot reach; authority lives only in a
**capability** (a handle carrying rights), never in a name, a path, or the
identity of the caller. Every boundary below is a consequence of that property,
and every check below exists to prove the property holds.

- Status: established during the security-hardening work (app sandbox,
  permission manifests, PermissionManager).
- Enforced by: the kernel capability mechanism, the userspace PermissionManager
  policy, and the WASI/native component sandbox.
- Verified by: capability property tests, syscall fuzzing, and the permission
  scenario in the kernel test suite.

---

## Table of contents

- [1. Scope and trust model](#1-scope-and-trust-model)
- [2. Adversaries](#2-adversaries)
- [3. Enforced boundaries](#3-enforced-boundaries)
- [4. Turning the model into executable checks](#4-turning-the-model-into-executable-checks)
- [5. Non-goals (for now)](#5-non-goals-for-now)

---

## 1. Scope and trust model

LiberSystem is a capability microkernel: a small message core in ring 0, with
drivers, services, and applications as isolated userspace processes that hold
only the capabilities they were handed.

```text
TRUSTED (the TCB - a fault here can break the property):
- the kernel: objects, the handle table, rights checks, address spaces,
  the Domain hierarchy, the syscall boundary, the scheduler;
- the boot chain that wires the initial capabilities:
  SystemManager -> ServiceManager -> the core services;
- the PermissionManager, which decides which capabilities a launched
  component is granted.

UNTRUSTED (assumed potentially malicious or buggy):
- applications (native or Wasm components);
- drivers (isolated userspace processes, even though they touch hardware);
- any data that crosses a boundary: syscall arguments, channel messages,
  file and device contents.
```

The trust boundary is the syscall edge and the channel. Everything an untrusted
process can do, it does by invoking a syscall on a handle it holds, or by sending
a message on a channel it holds. It can hold a handle only if some trusted party
explicitly created or transferred it.

## 2. Adversaries

### 2.1 A malicious application

A native or Wasm application that actively tries to reach resources it was not
granted: to read files it does not own, open the network, talk to a service it
has no client for, forge or widen a capability, or escape its sandbox into
another process.

```text
Assumed capable of:
- calling any syscall with any arguments, in any order;
- passing arbitrary, hostile, or malformed handle and pointer arguments;
- sending arbitrary bytes (and capabilities it does hold) on its channels;
- attempting to duplicate a handle with more rights than it carries.

Must NOT be able to:
- perform any operation a held handle's rights do not allow;
- obtain authority it was not explicitly granted (no ambient authority);
- widen a capability by duplicating it (attenuation only narrows);
- read or write another process's memory, or the kernel's;
- crash or corrupt the kernel through the syscall boundary.
```

### 2.2 A compromised driver

A driver is untrusted even though it drives real hardware: it may be exploited
through a malicious device or a bug. A compromised driver is a malicious
application that additionally holds a few hardware capabilities (an interrupt, a
DMA buffer, an MMIO region) - but no more than DeviceManager handed it.

```text
Assumed capable of:
- everything a malicious application is, plus
- driving its bound IRQ, its DMA buffer, and its MMIO region maliciously.

Must NOT be able to:
- reach memory outside its own DMA buffer / MMIO region;
- access devices, services, or files it was not granted;
- survive its own crash with its capabilities intact - on fault the kernel
  stops the process, reclaims its capabilities, detaches its IRQ, and revokes
  its DMA access, and only the supervisor decides whether to restart it.
```

**Whether the first of those is enforced for the DEVICE depends on the machine, and
which machine this is is reported on every boot rather than assumed.** There are two
cases and the system says which one it is in.

**Under an enforcing profile it holds.** On an x86_64 machine carrying a
`virtio-iommu`, the kernel brings the controller up, takes it out of boot bypass and
reads its own bypass byte back before any driver is allowed to master the bus. Each
bus-mastering endpoint is then attached to a domain of its own, and a DMA buffer is
mapped into that domain before its address is given to anybody - so what a driver
receives is an address that means something only inside its device's domain and
nothing at all outside it. Equal numeric addresses in two domains resolve to
different memory. A device told to read or write past its mapping is refused by the
hardware, and the refusal is recorded as a fault rather than inferred. Closing a
mapping unmaps and invalidates it and only then returns the frames; a step that does
not confirm quarantines the frames instead of recycling them. The boot prints
`dma: every bus-mastering device is translated` when this is the state, and a driver
that declared it needs translation does not bind on a machine that has a controller
which failed to come up.

**Without one it does not, and the system says so.** On a machine with no
`virtio-iommu`, a DMA capability hands the driver the buffer's raw physical addresses
and nothing between the device and RAM checks what its descriptor rings contain. A
driver that programs its device to read or write outside its own buffer succeeds. The
kernel's own mappings bound where the DRIVER may reach; they do not bound where its
DEVICE may reach. That state is not silent: every device running that way is listed
by name in the boot report under `dma: DEGRADED ISOLATION`.

So a driver holding a DMA-capable device is inside the memory-safety TCB exactly
when its device is untranslated. Under an enforcing profile it leaves that TCB, and
what remains trusted is named: the `virtio-iommu` device model, this kernel's backend
for it, and the hypervisor. Nothing here is a claim about a native VT-d, AMD-Vi,
SMMU or RISC-V IOMMU backend, about aarch64 or riscv64, or about physical hardware -
those need separately qualified backends and separately measured evidence.

Three things narrow the window on a machine that has no controller, and none of them
closes it: bus mastering stays disabled until ownership is established, reclaim
requires a quiescence claim from the holder of the device's own DeviceMemory
capability, and a faulted driver's DMA frames are quarantined rather than reused
immediately. `AI/audit/drivers.md` reports this boundary from the driver side as
DRV-015 and it is the same boundary.

## 3. Enforced boundaries

The boundaries below are mechanisms in the kernel (TCB) plus one policy layer in
userspace (the PermissionManager). Each is structural - it holds for every
process, not by the cooperation of the process it constrains.

1. **Capability handles.** A process never sees a raw object; it holds an opaque,
   per-process handle that the kernel resolves to a capability. A handle from one
   process is meaningless in another. A stale handle (to a closed, possibly reused
   slot) is rejected by generation.

2. **Rights gate every operation.** Every capability carries a rights bitset, and
   every syscall checks the right it needs before acting. A handle grants no
   operation beyond the rights it carries - a read-only handle cannot map for
   write, a non-`DUPLICATE` handle cannot be duplicated, and so on.

3. **Attenuation only narrows.** A derived capability (a duplicate, or a handle
   transferred onward) can only carry a subset of the original's rights. There is
   no operation that widens a capability. Least privilege is therefore monotone:
   authority can only shrink as it propagates.

4. **No ambient authority.** A process begins with an empty handle table and the
   single bootstrap capability it was launched with. It can reach a resource only
   by holding a capability to it; there is no global filesystem, device list, or
   service registry reachable "by default". This is the property the whole model
   protects.

5. **Address-space isolation.** Each process has its own address space; one
   process cannot name another's memory. A DMA buffer or MMIO region maps only the
   physical frames the capability covers, so even a driver doing DMA is confined.

6. **Fault isolation and cleanup.** A faulting process is terminated by the kernel,
   which reclaims its handles, IRQ binding, and DMA access. A crash cannot corrupt
   the kernel or another process, and a crashed driver loses its hardware
   authority. The Domain hierarchy bounds aggregate resource use and lets a whole
   subtree be killed at once.

7. **The strict app sandbox (PermissionManager + manifest).** A component is
   launched under a typed permission `Manifest` - a set of `Capability` grants,
   the typed source of truth for what it may be given, never a text or JSON file.
   The PermissionManager grants the launching component exactly its manifest's
   capabilities and nothing else, withholding even capabilities it holds itself
   when the manifest does not list them, and records every decision (grant or
   denial) in an audit trail. The launched component thus starts with only its
   manifest's capabilities and can reach nothing it was not granted - the
   WASI-world property ("a component gets only the imports we pass it") extended to
   every launched component.

```text
Boundary  -> Mechanism (where it lives)
-------------------------------------------------------------------
handles   -> per-process handle table with generations   (kernel)
rights    -> rights check on every syscall               (kernel)
narrowing -> duplicate/transfer subset-only              (kernel)
no AA     -> empty initial table + explicit grants        (kernel + boot)
memory    -> per-process address space, scoped DMA/MMIO   (kernel)
fault     -> terminate + reclaim + revoke; Domain limits  (kernel)
sandbox   -> typed Manifest, granted per policy + audited (PermissionManager)
```

## 4. Turning the model into executable checks

The threat model is only as good as the tests that hold it. Each property above
is backed by a check in the kernel test suite (run with `TEST=1 cargo test` in
`src/kernel`), so a regression that softens a boundary fails the build.

```text
Property                          Executable check
-------------------------------------------------------------------------------
a handle grants no operation      capability_grants_no_operation_beyond_rights
  beyond its rights                 (property test, randomized rights)
attenuation only narrows          capability_attenuation_only_narrows
                                    (property test: duplicate is subset-only,
                                     never widens)
no ambient authority              no_ambient_authority_fresh_table_empty
                                    (a fresh handle table resolves nothing)
the syscall boundary is robust    syscall_fuzz_rejects_invalid_calls
                                    (fuzz: random syscall numbers and bogus
                                     handle arguments are rejected, never crash
                                     the kernel or grant authority)
the strict sandbox holds          permission_manager_sandboxes_a_component
                                    (a component launched under a manifest reads
                                     only its one granted file and is denied the
                                     capability its manifest withholds; the audit
                                     records grant/grant/deny)
rights are enforced; duplication  handle_rights_enforced,
  attenuates; revocation is total   handle_duplicate_attenuates,
                                    handle_revocation_invalidates
fault isolation and cleanup       fault_isolation_kills_only_process,
                                    driver_crash_is_cleaned_up_and_notified,
                                    driver_survives_crash_and_restart
```

The property tests use a fixed-seed PRNG, so a run is deterministic and a failure
is reproducible. The syscall fuzz runs from a ring-0 thread whose handle table is
empty: every random handle resolves to nothing, so the boundary is exercised
without a real capability ever being put at risk.

## 5. Non-goals (for now)

The following are out of scope here and are tracked in the Concept's
*Security model: current decisions* and *Open questions*. They do not weaken the
"no ambient authority" property; they extend policy and coverage on top of it.

```text
- freshness, measured boot, physical-firmware qualification and production key operations.
  AUTHENTICITY ITSELF IS ENFORCED and is not on this list: the loader carries a compiled-in public
  key, and every kernel and early module it executes or hands to the kernel is covered by a signed
  manifest - a canonical record over each artifact's kind, path, length and SHA-256 digest, verified
  with Ed25519 BEFORE the bytes are parsed as an ELF, before a destination is derived from them and
  before anything is placed. A source whose signed manifest is present and damaged stops the boot
  rather than falling back. On x86_64 the loader itself is authenticated first, by an OVMF firmware
  with a platform key enrolled, and the gate proves the negative: an unsigned or bit-modified loader
  does not run at all. What is NOT covered: anti-rollback (a validly signed OLD image is accepted),
  TPM measurement and attestation, physical firmware enrollment and NVRAM behaviour, and production
  key custody, rotation and revocation - the format has a key-id boundary, the operations behind it
  are not this repository's to prove;
- fine-grained portals (mic/camera/screenshot) and network policy granularity;
- side-channel and timing attacks (Spectre-class microarchitectural leaks);
- physical attacks (cold boot, bus probing) and encrypted user volumes;
- denial of service via resource exhaustion beyond the Domain limits already
  enforced (a full ResourceManager policy is deferred);
- a formally verified kernel (the TCB is trusted, not proven);
- IOMMU isolation on every machine and every architecture. The x86_64
  `virtio-iommu` profile enforces it and 2.2 says what that buys; a machine with
  no controller does not, and its DMA-capable drivers remain TCB members for
  memory safety. Native VT-d, AMD-Vi, SMMU and RISC-V IOMMU backends, and the
  aarch64 and riscv64 runtime profiles, are not implemented.
```
