AUDITOR'S REVIEW OF PLAN P02M0190 (2026-09-25T22:49:59Z):

**Rating: 5/10.** The library underneath is real and the operation set is suitably narrow. But the plan never says how an application reaches the TPM or how its three grants are minted, and it leaves open which PCRs applications may use, what hierarchy authorization is assumed and how a restarted driver recovers.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0190.md) and the `src/tpm` library it builds on. Also reviewed the kernel's TPM hardware test, PermissionManager's grant minting, the provider vocabulary and service wiring, the firmware-node identity plan [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:27) and the originating [P02M0099 TPM item](/data/yellow/libersystem/docs/todo/P02M0099.md:3786), at commit `07371c44af82a11c1d275b0cbd832899f712e62a`, together with the QEMU device models on this machine. The plan, P02M0196 and P02M0099 were read as they stand in the working tree, which carries uncommitted edits to all three. The findings concern decisions the implementation needs, not the expected absence of the driver, contract and tool.

1. **High - The plan names a contract and three grants, but no process that serves `liber:tpm@1` to applications and no path by which PermissionManager mints a grant.**

   The [provider item](/data/yellow/libersystem/docs/todo/P02M0190.md:29) makes the driver "the ONE owner of the device", and the [authority item](/data/yellow/libersystem/docs/todo/P02M0190.md:40) says "PermissionManager grants each part separately". Nothing says who serves the contract, how a connection that carries only `tpm`, `tpm-measure` or `tpm-seal` is created, or how that connection is tied to the launched component.

   In this tree a driver does not serve applications. It publishes a provider of a [closed kind](/data/yellow/libersystem/src/idl/device.lsidl:154). Each device class has exactly one consuming service ("A SMART-CARD READER, with SmartcardService its one consumer", [system-manifest](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:193)). ServiceManager mints catalogue connections only for declared services ([device.lsidl](/data/yellow/libersystem/src/idl/device.lsidl:392)). Every device-backed application grant is minted in [`grant_for_task`](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:749). The mint goes through a service's ADMIN endpoint, which PermissionManager resolves over the broker, and it is made for a policy row naming the component, with the task as owner (see [smartcard](/data/yellow/libersystem/src/user/services/core/src/permission_manager.rs:806); its [service wiring](/data/yellow/libersystem/src/user/services/manifest.toml:4684) says "every application connection is minted there"). A driver launched by DeviceManager cannot be reached that way. The plan adds no `tpm` provider kind, no mint operation and no policy row.

   The operation-level split is where the plan's authority argument lives. It can be enforced only if one server knows which operations each minted connection carries. A driver that is "restartable like every driver here" also needs a stated lifetime for grants: after a restart, application connections are dead and must be minted again, not silently redirected.

   **Correct the provider, contract and authority items** to choose how the contract is served. The smallest arrangement that fits the tree has two parts:
   - The driver publishes a new `tpm` provider kind.
   - One small TPM service consumes that kind, serializes whole operations and serves an ADMIN mint that takes the operation subset and the task.

   If the driver is instead meant to serve applications directly, say how PermissionManager reaches it and why this departs from the pattern. Add the kind to the three closed mappings: the [IDL](/data/yellow/libersystem/src/idl/device.lsidl:154), the [manifest names](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:171) and the [DeviceManager wire mapping](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:6066). Add the default policy rows (the demonstration tool and nothing else). State what a client sees when the driver or the service restarts.

2. **Medium - Every hierarchy is assumed to be enabled and to have an empty authorization, so seal, unseal and quote fail on a TPM that another OS or the firmware has provisioned, and the plan does not decide what happens then.**

   The library authorizes "every hierarchy and object here" with the empty password ([command.rs](/data/yellow/libersystem/src/tpm/src/command.rs:81)). Seal, unseal and quote each start with `CreatePrimary` under the owner hierarchy ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:258)). `swtpm` starts with an empty owner authorization, so the [gate](/data/yellow/libersystem/docs/todo/P02M0190.md:53) passes. A machine whose owner hierarchy has an authorization value, or whose firmware disabled that hierarchy, answers with an authorization or hierarchy error. A third-party developer then gets an unexplained TPM response code. Only `random`, `pcr` and `extend` would still work on such a machine.

   **Correct the provider item and the developer page.** The driver detects the condition at bind from the TPM's permanent and startup-clear properties (through the `GetCapability` that finding 4 asks for) and reports a named refusal. State whether an operator path to supply the owner authorization exists (it would be an AdminService action) or whether such machines are unsupported. Add a host-suite or gate case with the owner authorization set on `swtpm`. Detection, a named error and a documented limit are enough.

3. **Medium - The plan leaves undefined which PCRs an application may extend, seal to and quote, and the grants do not separate one application from another.**

   The contract allows "the debug PCR 16 and the application range the policy names - never the firmware's" ([contract item](/data/yellow/libersystem/docs/todo/P02M0190.md:34)), but no policy anywhere names a range. The library lets locality 0 extend PCRs 0-16 and 23 ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:24)). That set includes the firmware's PCRs 0-7 and PCRs 8-15, which operating-system loaders and kernels conventionally use. The [originating item](/data/yellow/libersystem/docs/todo/P02M0099.md:3791) requires the TPM to integrate with a future measured boot, which will need 8-15.

   PCRs are shared by the whole machine. Any `tpm-measure` holder can move a PCR that another application seals to or quotes, and so break that application's secrets until reboot.

   A sealed object is not bound to the application that made it. Every seal uses the same fixed storage primary, which needs no authorization ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:53)), and the PCR policy is the object's only authorization ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:80)). Any `tpm-seal` holder that can read the file `tpm seal` wrote can unseal it.

   **Correct the contract, authority and documentation items:**
   - List the PCRs each operation accepts. For example, allow extend only on 16 and 23, and allow seal and quote over any readable PCR.
   - Reserve 8-15 for this system's own future measured boot.
   - State in the contract and on the developer page that PCRs are shared, and that a sealed object is bound to PCR state, not to an application.
   - If per-application binding is wanted, choose the mechanism now (for example, an authorization value on the sealed object). The library does not have one.

4. **Medium - A restarted driver inherits the objects and sessions its predecessor left loaded, and the library has no operation to find or flush them. `tpm info` needs the same missing operation.**

   The library flushes what it loads, on its failure paths as well as its successes ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:5)). A driver killed between `CreatePrimary`, `Load` or `StartAuthSession` and the matching `FlushContext` leaves those objects or sessions in the TPM. A PC Client TPM need hold only three transient objects and three loaded sessions. An unseal needs two objects and a session ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:410)). One kill at the wrong moment can therefore make every later unseal fail with an object-memory error until reboot, even though the plan calls the driver restartable.

   The library has no `TPM2_GetCapability` (see its [command codes](/data/yellow/libersystem/src/tpm/src/lib.rs:47)). Without it the leftover handles cannot be listed. Nor can `tpm info` report the manufacturer and firmware version the [tool item](/data/yellow/libersystem/docs/todo/P02M0190.md:49) promises, which contradicts a contract "carrying exactly the library's operations".

   **Correct the provider, contract and verification items:**
   - Add a bounded, typed `GetCapability` to the library, for fixed properties and loaded handles.
   - Add a start-up step in which the driver flushes transient objects and sessions it did not create.
   - Add `info` to the contract.
   - Add a gate step that kills the driver during an unseal and then unseals successfully.

5. **Medium - The plan says riscv64 has no TPM model, but this machine's QEMU has one, so a target that can be verified would be recorded as unsupported.**

   The [verification item](/data/yellow/libersystem/docs/todo/P02M0190.md:63) says "riscv64 has no TPM model in this QEMU and says so rather than passing". On this machine (QEMU 10.0.11), `qemu-system-riscv64 -device help` lists `tpm-tis-device, bus System`, the same sysbus model aarch64 uses. QEMU's riscv `virt` machine accepts that device as a dynamic sysbus device and emits its platform-bus device-tree node ([hw/riscv/virt.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/riscv/virt.c)).

   **Correct the verification item** to treat riscv64 like aarch64: bind through P02M0196a's device-tree half and verify on `virt` with `tpm-tis-device` over `swtpm` once that half exists. A one-line change suffices.

6. **Low - The plan misstates what the static-table step provides. The `TPM2` table names no FIFO region, some start methods need AML, and a later namespace node will describe the same TPM a second time.**

   The provider "maps the CRB or FIFO region the `TPM2` table names" ([provider item](/data/yellow/libersystem/docs/todo/P02M0190.md:29)), and "ONLY ITS FIRST STEP IS NEEDED" ([plan](/data/yellow/libersystem/docs/todo/P02M0190.md:22)). Four facts qualify both statements:
   - For the FIFO interface, QEMU's `build_tpm2` writes start method 6 with a zero control-area address ([hw/acpi/aml-build.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/acpi/aml-build.c)). The library substitutes the PC Client fixed base ([table.rs](/data/yellow/libersystem/src/tpm/src/table.rs:1), [table.rs](/data/yellow/libersystem/src/tpm/src/table.rs:52)).
   - That substitution is correct only on a PC. QEMU's aarch64 `virt` also emits a `TPM2` table with start method 6, while its TIS sits at a platform-bus address ([hw/arm/virt-acpi-build.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/arm/virt-acpi-build.c)).
   - TPMs that use the ACPI start methods (2 and 8) are refused by name because they need AML ([table.rs](/data/yellow/libersystem/src/tpm/src/table.rs:5)). On those machines step 1 does not make the resource exist.
   - QEMU also describes both front-ends as an `MSFT0101` namespace device with its own `_CRS` ([hw/i386/acpi-build.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/i386/acpi-build.c), [hw/tpm/tpm_tis_isa.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/tpm/tpm_tis_isa.c)). When [P02M0196b](/data/yellow/libersystem/docs/todo/P02M0196.md:48) enumerates `_CRS`, the same registers gain a second identity. "The ONE owner" then holds only if the two identities are one claimable device.

   **Correct the "what is missing" paragraph, the provider item and the developer page.** State the resource rule the identity step must implement: the FIFO fixed base on x86 only, the CRB control-area page, and no interrupt, because the transport polls. Name the unsupported start methods. Require that the table node and the namespace node resolve to one device.

Validation was plan and source inspection, plus the QEMU v10.0.0 sources cited above and `qemu-system-{x86_64,aarch64,riscv64} -device help` on this machine (QEMU 10.0.11). No build, test or guest was run, and no repository file was modified.

PLANNER'S RESPONSE ON P02M0190 (2026-09-26T02:42:25Z):

Verified all six findings against the plan as it stands in the working tree, `src/tpm` (`lib.rs`, `ops.rs`, `command.rs`, `table.rs`, `crb.rs`, `fifo.rs`), the kernel's TPM hardware test, `permission_manager.rs` (`grant_for_task`, the smart-card and modem arms, the component rows), `service_manager.rs` and `capability_names.rs`, the SmartcardService wiring in `manifest.toml`, the `provider-kind` enum in `device.lsidl`, `ProviderKindName` in `system-manifest`, DeviceManager's wire mapping and its disable path, the capability enum in `security.lsidl`, the aarch64 and riscv64 loaders (both hand the kernel `rsdp: 0` and a device tree), P02M0196 and P02M0099's TPM item. QEMU was queried read-only with `-device help` on all three targets (QEMU 10.0.11): x86_64 offers `tpm-crb` and `tpm-tis`, aarch64 and riscv64 both offer `tpm-tis-device` on the system bus. All six findings are correct in substance; two are taken with a narrower change than recommended, as stated. No source file was changed.

1. **ACCEPTED - No process serves the contract and no mint path exists.** Confirmed: `provider-kind` is closed at `admin-executor = 20`, every device-backed application grant in `grant_for_task` is a connection minted over a service's ADMIN root resolved through the broker with the task duplicated `RIGHT_WAIT | RIGHT_TRANSFER`, and a DeviceManager-launched driver has no broker-resolvable root. The plan now takes the arrangement the tree already uses for smart cards and modems: the driver publishes one provider of a new `tpm` kind, appended to the IDL enum, `ProviderKindName` and DeviceManager's wire mapping with its `driver_protocol` constant; a new TpmService consumes that kind alone through a CATALOGUE role and serves one ADMIN root, `liber:tpm@1/tpm-admin`, whose `mint(kind, component, owner)` returns a connection carrying one grant's operations for the life of the owner task. The three capabilities are appended to `security.lsidl`, minted by one `grant_for_task` arm through a new `TPMADMIN` name (in `capability_names.rs` and ServiceManager's three resolution rows). Because there is one TPM and no alias, the component row is the policy (no `*_policy` function). Default rows: the shipping `tpm` tool holds all three plus `volumes` (the one shipping row, as `btctl` is for the Bluetooth operator); a development-only `tpmprobe` holds `tpm`, `tpm-seal` and `volumes`. Decisions go to `service_logic::tpm`. The contract is now three interfaces (`tpm`, `tpm-admin`, `tpm-device`) with named refusals. The restart semantics are stated: after a DRIVER restart application connections stay (the TPM's state is in the chip), the operation in flight answers `interrupted` and is never replayed (a replayed extend would extend twice), and calls while no provider is present answer `unavailable`; after a SERVICE restart every connection dies and grants are minted at the next launch, as SmartcardService's are. The queue is bounded (one call per connection, 16 in all, `busy` past that). The client-library item now says how it routes calls across the three connections.

2. **ACCEPTED - Hierarchy authorization is assumed empty.** Confirmed: `password_authorization` is the empty password for everything, and seal, unseal and quote each start with `CreatePrimary(RH_OWNER)`. The plan adds a library item reading `ownerAuthSet` (`TPM_PT_PERMANENT`) and `shEnable` (`TPM_PT_STARTUP_CLEAR`) through the new `GetCapability`. The driver reads this state at start-up and logs it. TpmService answers `seal`, `unseal` and `quote` with `owner-hierarchy-unavailable` without sending anything, and `info` reports it; random, PCR read and extend keep working. Supplying an owner authorization is not part of this milestone. It is in EXCLUDES and documented as a limit whose remedy is a TPM clear, which also invalidates earlier sealed objects. The host test sets an owner authorization on `swtpm` with a `HierarchyChangeAuth` built in the test, and disables the owner hierarchy with `HierarchyControl` under the platform's empty authorization.

3. **ACCEPTED - PCR sets undefined and sealed objects not bound to an application.** Confirmed: `extendable` admits 0-16 and 23, no policy names a range, and `sealed_template` makes the PCR policy the object's only authorization under a storage primary that needs none. The contract now fixes the sets: `pcr-read`, `seal`, `unseal` and `quote` take any PCR 0-23; `pcr-extend` takes 16 and 23 only; 8-15 are reserved for this system's own measured boot (and EXCLUDES says so). The library's `extendable` stays as it is for that future consumer, and the application range is TpmService's rule. Per-application binding is decided with the smallest mechanism that needs no library change: TpmService prefixes every sealed secret with the SHA-256 of the sealing component's name and refuses a mismatching `unseal` with `other-component`, which reduces the application's secret to 96 bytes. It holds because TpmService is the only process reaching the TPM's `Unseal`. The developer page now states that PCRs are shared, what a sealed object is bound to (this TPM, the PCR's value, the component) and what a `tpm-measure` holder can do to a secret sealed to 16 or 23.

4. **ACCEPTED, with the in-guest kill replaced by a host-test oracle - leftovers after a restart and no `GetCapability`.** Confirmed: `lib.rs` has no `CC_GET_CAPABILITY`, `unseal` holds a primary, the loaded object and a policy session at once, and `Startup` cannot clear anything once the TPM has started. The plan adds a bounded, typed `GetCapability` (fixed properties, the two attribute words, transient and loaded-session handles; 16 properties and 64 handles per answer) and `flush_leftovers`. The driver runs `flush_leftovers` at every start, before it publishes, and logs the count. `info` is in the contract and the tool. DECLINED: the gate step that kills the driver during an unseal. DeviceManager's `--disable` is a planned stop that withdraws the provider, sends STOP and forces a teardown only past the deadline (`apply_policy`), and a `swtpm` operation completes in milliseconds, so no guest step can place a kill inside one operation without a fault-injection seam this tree does not have. The same recovery is instead proven deterministically in `host.tpm` against `swtpm`. Objects and sessions are loaded and never flushed, an unseal fails with the object-memory warning, and after `flush_leftovers` the same unseal succeeds. The guest gate adds a driver restart through `lsdev --disable`/`--enable` (answering `unavailable` while down, the start line logged again, the sealed file still unsealing) and a TpmService stop/start.

5. **ACCEPTED - riscv64 has a TPM model.** Confirmed: `qemu-system-riscv64 -device help` lists `tpm-tis-device, bus System`, as aarch64 does. The QEMU source claim about `virt` allowing it as a dynamic sysbus device was not re-read; it is consistent with the device being listed and with the aarch64 case. The verification item now treats aarch64 and riscv64 alike, bound through P02M0196a's device-tree half, and notes that the node sits under QEMU's platform bus so its `reg` is read through that bus's `ranges`. The "what is missing" paragraph names the `tcg,tpm-tis-mmio` node.

6. **ACCEPTED - What the `TPM2` table provides.** Confirmed in `table.rs`: method 6 is given the fixed `0xFED40000`, method 7 derives the base from the control area at offset 0x40, and 2, 8, 11 and 12 are refused by number. The aarch64 point is narrower in this tree than stated: both non-x86 loaders pass `rsdp: 0`, so the table is never read there. The plan now says the table path is x86_64's alone and that aarch64 and riscv64 come from the device tree. The plan now states the resource rule: one MMIO range, the locality-0 register page (4 KiB at the base: fixed on x86_64 for method 6, control area less 0x40 for method 7, `reg` on the device tree). `fifo.rs` touches nothing past 0xF00 and QEMU's CRB buffers end within the page; localities above 0 are excluded, and a CRB naming buffers outside the page is refused by the transport's `Buffer`. The rule also states no interrupt and no DMA, and that only methods 6 and 7 make a device, with the others logged by number. EXCLUDES and the developer page name the unsupported methods. On identity, the `MSFT0101` node resolves to the table's device under P02M0196a's reconciliation rule when its `_CRS` range starts at the same base and contains the page, and the claim stays the page. QEMU's CRB node names one page and its TIS node 0x5000. An `MSFT0101` node without a `TPM2` device matches no driver.

Re-check of the whole plan: every item is now an implementable decision with its files named. The library additions feed the driver's start. The driver serves only `tpm-device` to its one consumer, and the service holds every policy decision in one host-tested module. The capability, kind and resolution names are appended, never inserted, and nothing is version-bumped. The tool's commands match the contract, now naming the PCR for `seal` and `quote`. The gate's order keeps PCR 16 unchanged until the step that is meant to break the seal. The verification items cover every new behaviour, with the one guest limit stated and covered by a host oracle. The plan stays within D5 (it refers to P02M0196a for the claim, the identity forms and the reconciliation) and D17 (only the driver's binding publishes; applications reach the TPM through minted service connections). It is complete, correct, feasible and internally consistent. No source, test or script file was changed.


AUDITOR'S RE-AUDIT OF PLAN P02M0190 (2026-09-26T04:01:12Z):

**Rating: 7/10.** The central arrangement now fits the tree and can be built. But the device-tree half of the resource rule is not what P02M0196a provides, and the gate cannot observe TpmService's own enforcement.

I read the complete history: the original review, the planner's response, the plan as audited (`git show 07371c44:docs/todo/P02M0190.md`) and the planner's edits in commit 0dd5da07 to P02M0190, P02M0099 and `TODO.md`. I checked the plan against the tree:
- `src/tpm` (`lib.rs`, `table.rs`, `crb.rs`, `fifo.rs`, `ops.rs`, `command.rs`, `marshal.rs`, `tests/swtpm.rs`) and the kernel's TPM hardware test.
- The provider kinds in `device.lsidl`, `ProviderKindName`, DeviceManager's wire mapping and the `driver_protocol` constant.
- The capability enum in `security.lsidl`.
- PermissionManager's `grant_for_task`, its component rows and the `cardread` probe.
- `capability_names.rs` and ServiceManager's broker rows.
- SmartcardService's manifest wiring and the policy verbs of `lsdev`.
- The aarch64 and riscv64 loaders, the TPM attachment in `qemu-run.sh`, `check.sh`, `lab.sh` and the scenario runner.

I also read P02M0196 in full, P02M0196's audit and P02M0099's TPM item. For QEMU 10.0 I used `-device help` on all three targets and read the v10.0.0 sources of the TIS, CRB and platform-bus device-tree code. The corrections for findings 1 to 5 hold. The declined in-guest kill is adequately replaced by the `host.tpm` test. The correction for finding 6 holds on x86_64 but not on the device tree.

1. **High - The plan says P02M0196a limits the device-tree TPM's claim to one 4 KiB page, but P02M0196a publishes the node's whole `reg`, so on aarch64 and riscv64 the claim covers the four localities the plan excludes.**

   **What P02M0190 expects.** The plan says the identity step implements a resource rule for this driver ([resource rule](/data/yellow/libersystem/docs/todo/P02M0190.md:67)). The rule allows one 4 KiB page for locality 0 ([one page](/data/yellow/libersystem/docs/todo/P02M0190.md:69)). On the device tree that page sits at the node's `reg`, and "Localities above 0 are excluded" ([device tree](/data/yellow/libersystem/docs/todo/P02M0190.md:72)). The rule also says there is no interrupt ([no interrupt](/data/yellow/libersystem/docs/todo/P02M0190.md:75)). The runtime checks on aarch64 and riscv64 depend on this device-tree binding ([runtime](/data/yellow/libersystem/docs/todo/P02M0190.md:208)).

   **What P02M0196a provides.** P02M0196a applies the 4 KiB rule only to the `TPM2` table ([TPM2 bullet](/data/yellow/libersystem/docs/todo/P02M0196.md:99)), and it marks that rule "x86_64 only" ([x86_64 only](/data/yellow/libersystem/docs/todo/P02M0196.md:101)). Its device-tree bullet publishes every node with its `reg` translated through `ranges`, and with its interrupts. It has no rule for a TPM node ([device tree](/data/yellow/libersystem/docs/todo/P02M0196.md:118)). The claim then mints "a `DeviceMemory` per MMIO range" ([claim](/data/yellow/libersystem/docs/todo/P02M0196.md:68)). P02M0196's own check on aarch64 and riscv64 asks only that the node be "published on both" ([P02M0196 check](/data/yellow/libersystem/docs/todo/P02M0196.md:336)).

   **What QEMU emits.** QEMU 10.0 writes the node's `reg` size as 0x5000 ([sysbus-fdt.c, line 467](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/core/sysbus-fdt.c#L450-L472)). That is the whole five-locality region the device maps ([tpm_tis_sysbus.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/tpm/tpm_tis_sysbus.c#L103-L105)). The library itself calls this region `REGION_LEN = 0x5000` ([table.rs](/data/yellow/libersystem/src/tpm/src/table.rs:22)).

   **The consequence.** On aarch64 and riscv64 the driver's claim would carry a 0x5000 `DeviceMemory`. That is exactly the window over localities 1 to 4 that the plan calls "an authority this driver never uses". A board whose node names `interrupts` would also add an `Interrupt`, although the plan says there is none. Neither plan owns cutting the range down. This continues original finding 6: its correction extended the resource rule to the device tree, but P02M0196a has no matching rule.

   **Correct the resource rule** in one of two ways:
   - Make it a requirement on P02M0196a's device-tree bullet: a `tcg,tpm-tis-mmio` node publishes only the first 4 KiB of its translated `reg`, and no interrupt. P02M0196 then states this beside its `TPM2` rule.
   - Or state that on aarch64 and riscv64 the claim is the node's whole `reg` (0x5000 on QEMU) and the driver maps only its first page. Then limit "Localities above 0 are excluded" and "NO INTERRUPT" to x86_64.

2. **Medium - The gate cannot fail if TpmService itself does not enforce grants: the `not-granted` it checks comes from the client library inside `tpmprobe`, and no application connection is kept open across the driver restart.**

   **The `not-granted` check never reaches TpmService.** The client library answers `not-granted` itself for any call that none of the held grants carries ([client library](/data/yellow/libersystem/docs/todo/P02M0190.md:170)). `tpmprobe` holds `tpm` and `tpm-seal` but not `tpm-measure` ([rows](/data/yellow/libersystem/docs/todo/P02M0190.md:164)). So it has no connection that carries `pcr-extend`. The gate's "`tpmprobe` is refused `not-granted` on an extend" ([gate](/data/yellow/libersystem/docs/todo/P02M0190.md:185)) is therefore answered inside the probe's own process.

   That step fails only if PermissionManager wrongly mints `tpm-measure` for the probe. It cannot catch a TpmService that serves `pcr-extend` on a `tpm` connection. TpmService's check is the real boundary for a third-party program, which need not link `tpm-client`. The host tests cover only the decision function in `service_logic::tpm` ([verification](/data/yellow/libersystem/docs/todo/P02M0190.md:195)). They do not cover how the service applies that decision to a live connection.

   The smart-card gate, whose shape the plan copies, does check this in the guest. `cardread` calls `acquire` on its read-only connection through the generated client, and SmartcardService refuses it ([cardread.rs](/data/yellow/libersystem/src/user/services/core/src/cardread.rs:29)).

   **The restart check never keeps a connection open.** The plan promises that application connections STAY across a driver restart ([restart item](/data/yellow/libersystem/docs/todo/P02M0190.md:115)). But each `tpm` command is a new launch with newly minted grants ([per launch](/data/yellow/libersystem/docs/todo/P02M0190.md:189)). The restart step uses a new `tpm random` while the driver is down and a new launch to unseal after it ([restart step](/data/yellow/libersystem/docs/todo/P02M0190.md:187)). A TpmService that closed every application connection when the provider was withdrawn would still pass.

   **Correct the gate item.**
   - Have `tpmprobe` send `pcr-extend` on its `tpm` connection through the generated client, not through `tpm-client`, and require TpmService's `not-granted`.
   - Have `tpmprobe` hold one connection open across `lsdev --disable` and `--enable`. On that same connection, `random` answers a value before the restart, `unavailable` during it and a value after it.

Validation: I inspected the plans, the audit history and the source with read-only git (`show`, `diff`, `log`), `grep`, `sed` and `awk`. QEMU was queried only with `-device help` on the three targets. I fetched the QEMU v10.0.0 sources read-only from gitlab.com into the scratchpad. No plan, source or audit file was modified, and nothing was built, tested or booted.


PLANNER'S RESPONSE ON P02M0190 (2026-09-26T05:34:16Z):

I verified both findings against the plan as it stands in the working tree and against the following sources:
- P02M0196: the claim item, the `TPM2` bullet, the device-tree item and the aarch64/riscv64 check. I also read P02M0196's own latest re-audit, whose finding 4 raises the same point from its side.
- P02M0197: the driver wire's `SUSPEND`/`RESUME` contract with `suspend-deadline`, and part e.
- `src/tpm`: in `table.rs`, `REGION_LEN = 0x5000`, `FIFO_BASE` and the offset-0x40 control-area check; in `fifo.rs`, no register past `DID_VID` at 0xF00; in `crb.rs`, `inside` and `Buffer`; in `lib.rs`, no `CC_SHUTDOWN`; in `ops.rs`, `startup` accepting `RC_INITIALIZE`.
- The QEMU v10.0.0 sources:
  - `hw/core/sysbus-fdt.c` `add_tpm_tis_fdt_node` writes a `reg` size of 0x5000, and its comment says "Optional interrupt for command completion is not exposed".
  - `hw/tpm/tpm_tis_sysbus.c` maps `TPM_TIS_NUM_LOCALITIES << TPM_TIS_LOCALITY_SHIFT`.
  - `hw/tpm/tpm_tis_isa.c` has its `_CRS` interrupt commented out.
  - The riscv and arm `virt` machines both admit `TYPE_TPM_TIS_SYSBUS` and give the platform bus `ranges`.
- `qemu-system-{x86_64,aarch64,riscv64} -device help` on this machine (QEMU 10.0.11).
- The smart-card precedent:
  - `cardread.rs`, which calls `acquire` through the generated `smartcard::Client` and requires `Denied`.
  - `check-smartcard-service.sh`, whose background `cardhold hold 4 &` and `cardhold slow &` hold connections while later lines run.
  - PermissionManager's smart-card arm of `grant_for_task` and `cardread`'s read-only row.
- The client crates (`bluetooth-client` and its provider: named trampolines over the generated clients).
- `lsdev`'s `--disable N`/`--enable N`, the shell's `&` jobs, and `scenario.py`'s `key`, `expect` and `prompt` steps with the `dfu-tool` scenario.
- `rt::connect_or_resolve`.
- The manifest's catalogue-client demand, computed read-only: 27 of 32 today.

Two findings accepted, none rejected.

1. **ACCEPTED - The device-tree claim would be the node's whole `reg`, with its interrupt.** Confirmed. P02M0196a cuts only its `TPM2` row to the page and marks that rule x86_64-only. Its device-tree item publishes every node's translated `reg` and its interrupts, with no rule for a TPM node, and its claim mints one `DeviceMemory` per range. QEMU's `tcg,tpm-tis-mmio` node names 0x5000, which is all five localities (the library's own `REGION_LEN`). QEMU exposes no interrupt, but the binding allows one on a board. So on aarch64 and riscv64 the claim would carry the window this plan withholds, and neither plan cut it.

   I took the first of the two corrections, agreed with P02M0196's planner: P02M0196a's device-tree item gives a `tcg,tpm-tis-mmio` node one range, the 4 KiB page at its translated `reg` base, and no interrupt, as its `TPM2` row does. I declined the second correction, which would have kept a 0x5000 `DeviceMemory` in the claim. That is exactly the authority the rule exists to withhold, and a driver that merely declines to map it does not remove it.

   Plan changes:
   - **The driver item's lead** now says the resource rule holds on all three targets. P02M0196a implements it on both paths: in its `TPM2` row, and in the row its device-tree item gives a `tcg,tpm-tis-mmio` node, cut to the page rather than carrying the translated `reg` whole.
   - **ONE MMIO RANGE:** on the tree the base is that of the node's `reg`, translated through its parents' `ranges`. The page is all of that `reg` the row carries, although QEMU's node, and possibly a board's, names 0x5000. "Localities above 0 are excluded" therefore holds everywhere.
   - **NO INTERRUPT:** the row carries none even where a board's tree node, or an `MSFT0101` node's `_CRS`, names one.
   - **ONE DEVICE, NOT TWO:** the claim stays the page, with no interrupt.
   - **The gate:** a new first step, in which `lsdev` lists the TPM as one platform row whose resources are one 4 KiB MMIO range at `0xFED40000` and no interrupt.
   - **Runtime (aarch64 and riscv64):** now runs the `tpm-tis` run's scenario, bound through the row P02M0196a's device-tree item gives a `tcg,tpm-tis-mmio` node. Its first step shows that row as one 4 KiB range at the node's translated base with no interrupt, although the node's `reg` names 0x5000.

2. **ACCEPTED - The gate cannot see TpmService's own enforcement, or a connection surviving a driver restart.** Confirmed:
   - `tpm-client` answers `not-granted` itself for a call no held grant carries, and `tpmprobe` holds no `tpm-measure`. The gate's refused extend therefore never left the probe, and it catches only a PermissionManager row that wrongly mints `tpm-measure`.
   - The host tests cover `service_logic::tpm` as a function, not the live service applying it.
   - Every `tpm` command is a new launch with freshly minted grants. A TpmService that closed every application connection on the provider's withdrawal would still pass the restart step.
   - The smart-card gate does make this check: `cardread` sends `acquire` through the generated client and requires the service's `Denied`.

   Plan changes:
   - **The refusal check:** `tpmprobe` is now refused `not-granted` twice on an extend of PCR 23, and PCR 23 stays unchanged.
     - The first refusal comes through `tpm-client`, answered in the probe's own process because PermissionManager minted it no `tpm-measure` grant.
     - The second comes through the contract's generated client on the probe's `tpm` connection. As with `cardread`, only TpmService can answer that call.
     - I kept both because each catches a different defect.
     - PCR 23 is a PCR that `pcr-extend` accepts, so the missing grant is the only possible refusal. It also leaves PCR 16 untouched for the later steps, which depend on it.
   - **The restart check:** `tpmprobe hold`, started in the background before `lsdev --disable`, holds ONE `tpm` connection across the restart.
     - On that connection, `random` answers a value before the disable, `unavailable` while the driver is down and a value after the enable.
     - An `interrupted` is admitted for the one call the withdrawal overtook, because the plan's own restart rule answers `interrupted` for the operation in flight.
     - The probe prints each change of answer, and the scenario waits for each one before its next step, as the scenario runner's `expect` allows.
     - The existing checks around it stay: a newly launched `tpm random` while the driver is down, the start line logged again, and the sealed file still unsealing.
   - **The client-library item** now says its local `not-granted` is a convenience, not the boundary: TpmService itself answers `not-granted` to any operation sent on a connection whose grant does not carry it, which is what a program that does not link `tpm-client` meets.

Coordinated changes: the driver item gains one sentence for the TPM's sleep step, which P02M0197 names:
- The driver implements P02M0197's `SUSPEND`/`RESUME` exchange, with a `suspend-deadline` in its registry entry. The work is carried by whichever of P02M0197 and this milestone lands second.
- Its `SUSPEND` finishes the operation in hand and reads no other until its `RESUME`. For S3 it sends `TPM2_Shutdown(TPM_SU_STATE)`. The library has no `CC_SHUTDOWN`, so the sentence says this is a typed operation `src/tpm` gains with it.
- Its `RESUME` sends no `Startup` when the firmware has already started the TPM with the saved state.
- The developer-page item now also says that a resume from S3 resets PCRs 16 and 23 - the PC Client profile saves only PCRs 0 to 15 across `Shutdown(STATE)`, and libtpms, the engine `swtpm` runs, resets PCRs 16 to 23 at every `Startup` - so a secret sealed to either stays shut after a sleep too, while one sealed to PCRs 0 to 15 survives it. P02M0197's S3 case asserts both halves.

Re-check of the whole plan:
- **Resource rule:** it now states one rule for all three targets, with P02M0196a named as implementing it on both paths. This agrees with P02M0196a's `TPM2` bullet, its merge rule (the first description's resources are kept, so an `MSFT0101` `_CRS` adds no interrupt) and the device-tree row its planner is adding. The transports it relies on stay inside the page: every FIFO register is below 0xF04, and the CRB's buffers are checked by `Buffer`.
- **Gate:** its order still keeps PCR 16 intact until the step that is meant to break the seal. The held connection ends before TpmService is stopped. Every probe and grant it uses is in the default rows (`tpmprobe hold` needs only `tpm`).
- **Sleep sentence:** consistent with P02M0197a's contract and with part e, which resumes the TPM driver for its image key.
- **Catalogue slots:** TpmService's transparent catalogue role raises the manifest's catalogue-client demand from 27 to 29 of 32, so this milestone alone needs no change to the bound.
- **EXCLUDES:** one completeness fix from the re-read. The I2C exclusion now also names a device tree's `tcg,tpm-tis-i2c` node, since aarch64's QEMU offers `tpm-tis-i2c` and the driver matches only `tcg,tpm-tis-mmio`.
- **One inconsistency outside this plan:** P02M0197e still says riscv64's QEMU has no TPM model and that riscv64 reports hibernation "not set up". `qemu-system-riscv64 -device help` lists `tpm-tis-device`, and riscv `virt` admits it, so that text is P02M0197's to correct.

The plan is complete, correct, feasible, internally consistent, versions nothing, and is ready for implementation.

Only `docs/todo/P02M0190.md` was edited and this response appended; no source, test, script or other plan was changed, and nothing was built or booted.


AUDITOR'S RE-AUDIT OF PLAN P02M0190 (2026-09-26T15:50:59Z):

**Rating: 10/10.** No unresolved material findings. Both findings of the last re-audit are corrected in the plan, P02M0196a's own text now carries the device-tree half, and the planner's additions agree with P02M0197, the source tree and QEMU.

I read the complete history: the original review, both planner responses and the re-audit of 2026-09-26T04:01:12Z. I read the plan as it stands in the working tree and the planner's latest edits (`git diff` against `eca3cbe7`). I checked it against:
- the sibling plans as they stand in the working tree: P02M0196 (the merge rule, the claim, the `TPM2` row, the device-tree item and its checks), P02M0197 (the suspend transaction, the driver contract, the TPM driver's step, the S3 case and part e), P02M0099's TPM item and `TODO.md`. No other sibling mentions the TPM.
- `src/tpm`: `table.rs`, `crb.rs`, `fifo.rs`, `lib.rs` and `ops.rs`.
- The smart-card precedent (`cardread.rs`, `check-smartcard-service.sh`).
- The scenario runner's `expect` step and teardown, the `dfu-tool` scenario and its gate, and `lsdev`'s policy verbs.
- The TPM attachment in `qemu-run.sh` and the manifest's catalogue-client bound.
- `qemu-system-{x86_64,aarch64,riscv64} -device help` on this machine (QEMU 10.0.11): `tpm-crb` and `tpm-tis` on x86_64, `tpm-tis-device` on aarch64 and riscv64, and `tpm-tis-i2c` on aarch64. No CRB sysbus model is offered.

Both findings of the last re-audit are resolved:
- **Finding 1 (the device-tree claim): corrected.**
  - The plan now states one resource rule for all three targets ([driver item](/data/yellow/libersystem/docs/todo/P02M0190.md:67), [the page on the tree](/data/yellow/libersystem/docs/todo/P02M0190.md:74), [no interrupt](/data/yellow/libersystem/docs/todo/P02M0190.md:79)).
  - P02M0196a now implements it on the tree ([device-tree item](/data/yellow/libersystem/docs/todo/P02M0196.md:129)) and tests it in its [host suites](/data/yellow/libersystem/docs/todo/P02M0196.md:361) and its [aarch64/riscv64 check](/data/yellow/libersystem/docs/todo/P02M0196.md:409).
  - The merge rule [keeps the first description's resources](/data/yellow/libersystem/docs/todo/P02M0196.md:54), so an `MSFT0101` `_CRS` adds no range and no interrupt.
  - The new first gate step and the aarch64/riscv64 runtime step fail if the row carries 0x5000 or an interrupt ([gate](/data/yellow/libersystem/docs/todo/P02M0190.md:195), [runtime](/data/yellow/libersystem/docs/todo/P02M0190.md:231)).
  - Declining the second option was right.
- **Finding 2 (the gate could not see TpmService's own checks): corrected.**
  - The refused extend of PCR 23 now also goes through the generated client on the probe's `tpm` connection ([gate](/data/yellow/libersystem/docs/todo/P02M0190.md:199)). As with [`cardread`](/data/yellow/libersystem/src/user/services/core/src/cardread.rs:29), only TpmService can answer that call. PCR 23 is in the extend set, so the missing grant is the only possible refusal.
  - `tpmprobe hold` keeps one connection open across the driver restart ([restart step](/data/yellow/libersystem/docs/todo/P02M0190.md:205)). A TpmService that closed it could not answer a value on it after the enable, so the step can fail.
  - The mechanisms exist: [shell background jobs](/data/yellow/libersystem/src/harness/scenarios/dfu-tool.toml:21), [ordered `expect` steps](/data/yellow/libersystem/src/harness/scenario.py:767), and driver lines on the cold serial log, which the [DFU gate already reads](/data/yellow/libersystem/src/tools/check-dfu-tool.sh:88).

The planner's other changes hold:
- **The sleep sentence** ([driver item](/data/yellow/libersystem/docs/todo/P02M0190.md:90)) says what P02M0197a's [TPM driver step](/data/yellow/libersystem/docs/todo/P02M0197.md:157) and its [`suspend-deadline` contract](/data/yellow/libersystem/docs/todo/P02M0197.md:132) say. The library has no `CC_SHUTDOWN` ([lib.rs](/data/yellow/libersystem/src/tpm/src/lib.rs:48)). Its `startup` sends CLEAR and accepts `RC_INITIALIZE` ([ops.rs](/data/yellow/libersystem/src/tpm/src/ops.rs:181)). P02M0197's [S3 case](/data/yellow/libersystem/docs/todo/P02M0197.md:329) reads PCRs and unseals a secret, which only this milestone's service can do. So "whichever lands second" is in practice P02M0197, and that case covers the step.
- **The developer page's S3 note** ([docs item](/data/yellow/libersystem/docs/todo/P02M0190.md:238)) agrees with the same S3 case and with libtpms, the engine `swtpm` runs. Its `PCR.c` marks only PCRs 0 to 15 as saved, and on a resume it resets 16 and 23 to zero.
- **The catalogue arithmetic holds.** Today's demand is 27 of 32: 17 minting roles, 9 of them transparent, plus DeviceManager's own. One more transparent role adds two ([lib.rs](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:1499)).
- **The planner's note about P02M0197e is out of date.** Part e now binds riscv64 through the device tree ([part e](/data/yellow/libersystem/docs/todo/P02M0197.md:384), [its verification](/data/yellow/libersystem/docs/todo/P02M0197.md:436)).
- **When `tpmprobe hold` ends.** The response says the held connection ends before TpmService is stopped, but the plan does not say so. This is harmless:
  - A cold run starts its own list of scenarios already run ([lab.py](/data/yellow/libersystem/src/harness/lab.py:2959)), so the strict frame-loss check does not apply to it ([scenario.py](/data/yellow/libersystem/src/harness/scenario.py:676)).
  - The teardown counts only registry artifacts and agent launches as held ([lab.py](/data/yellow/libersystem/src/harness/lab.py:2799)).
- **Repository rules:** no milestone id is placed outside `docs/todo`, no audit is referenced, and nothing is versioned.

No incomplete or incorrect correction, unjustified rejection, contradiction or newly discovered material defect remains to report.

Validation: I inspected the plan, the sibling plans, the audit history and the source with read-only git (`diff`, `show`), `grep`, `sed` and a read-only Python parse of `manifest.toml` for the catalogue count. QEMU was queried only with `-device help` on the three targets. libtpms's `src/tpm2/PCR.c` (branch `stable-0.9`) was fetched read-only from GitHub. No plan, source or audit file was modified, and nothing was built, tested, benchmarked or booted.
