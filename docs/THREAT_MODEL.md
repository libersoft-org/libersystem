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

**The first of those is enforced for the CPU and not for the device, and that is a
stated limit rather than an oversight.** There is no IOMMU. A DMA capability hands
the driver the buffer's raw physical addresses so it can program the device's
descriptor rings, and nothing between the device and RAM checks what those rings
contain. A driver that programs its device to read or write outside its own buffer
succeeds. The kernel's own mappings bound where the DRIVER may reach; they do not
bound where its DEVICE may reach.

So a driver holding a DMA-capable device is, for memory-safety purposes, inside the
TCB, and the properties above hold against a driver that is buggy or hostile in
software but not against one that turns its device into an arbitrary read/write
primitive. Three things narrow the window and none of them closes it: bus mastering
stays disabled until ownership is established, reclaim requires a quiescence claim
from the holder of the device's own DeviceMemory capability, and a faulted driver's
DMA frames are quarantined rather than reused immediately.

Closing it means per-device IOMMU domains with only live DMA capabilities mapped in.
That is tracked as a non-goal below, with the same standing as the others: known,
priced, and not yet done. `AI/audit/drivers.md` reports this boundary from the driver
side as DRV-015 and it is the same boundary.

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
- a signed/immutable system image, verified boot, and a package trust chain. Said precisely,
  because a reader may otherwise assume UEFI Secure Boot covers the boot chain: Secure Boot verifies
  the LOADER BINARY and stops there. Nothing verifies the kernel, the bootstrap list, the early
  modules, or the system volume they come from, and nothing binds them to the loader that read them.
  An attacker who can modify the boot or system volume replaces the kernel or early userspace and
  owns the machine before a single capability exists. Closing it means a signed boot manifest rooted
  in a key the loader authenticates, binding the system UUID, kernel, bootstrap list, every module
  and the image by length and strong digest, verified BEFORE any fixed-address placement, with
  revocation and a monotonic rollback version whose storage is explicit;
- fine-grained portals (mic/camera/screenshot) and network policy granularity;
- side-channel and timing attacks (Spectre-class microarchitectural leaks);
- physical attacks (cold boot, bus probing) and encrypted user volumes;
- denial of service via resource exhaustion beyond the Domain limits already
  enforced (a full ResourceManager policy is deferred);
- a formally verified kernel (the TCB is trusted, not proven);
- IOMMU isolation of DMA-capable devices. Until per-device IOMMU domains exist,
  a driver holding such a device can reach any physical memory through it, so
  DMA-capable drivers count as TCB members for memory safety - see 2.2, which
  states this rather than implying it.
```
