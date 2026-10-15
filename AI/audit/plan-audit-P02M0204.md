AUDITOR'S REVIEW OF PLAN P02M0204 (2026-10-15T02:53:13Z):

**Rating: 3/10.** The plan reads the current restart mechanics correctly, and the owner's tool is a reasonable shape: a few parameters, defaults for services and drivers, overrides by name, `--json`. But five things are missing or wrong:
- It excludes the services that the failed-start gap is mostly about, and nothing in it can re-run a start or start the services waiting on one.
- Its settings live in a store that anyone with a configuration client can write, that cannot delete them, and that tells nobody when they change.
- It retries starts that block the supervisor's loop.
- Its `then reboot` names a path that does not exist.
- Its permanent/transient rule has no carrier on the service side.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0204.md), and [the gap in P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:234) it was created to close, against the tree at HEAD 26faf58d. In the tree I read:
- ServiceManager's bring-up loop, crash restart, relaunch paths and policy read;
- the bootstrap code's `start_service`;
- ConfigService, its IDL and the `config` tool;
- the observability IDL and `lssvc`;
- WatchdogService's liveness contract;
- the kernel's recovery ladder;
- `rt::fail_bootstrap` and `rt::exit_with`;
- DeviceManager's incident model and the binding library.

1. **High - Excluding `escalate` services leaves the failed-start gap open for 24 of the 42 services, and nothing in the plan can re-run a start or start the services that wait on one.**

   [The rule](/data/yellow/libersystem/docs/todo/P02M0204.md:35) gives an `escalate` service no policy at all. The manifest declares 24 of 42 services `escalate`, among them StorageService ([row](/data/yellow/libersystem/src/user/services/manifest.toml:4962)), PermissionManager ([row](/data/yellow/libersystem/src/user/services/manifest.toml:5107)), ProcessService, NetworkService, DisplayService, InputService, ConsoleService and the shell. The gap P02M0099 names is "a failed START is recorded and nothing asks again", and it is not limited to the 18 `transparent` services.

   The plan's own reason for `escalate` does not apply to a start that failed. That reason is: clients hold the channels they were handed, and a replacement would leave those dead. But a service whose start never completed handed nothing to anybody.

   What does stop a retry today is code the plan does not touch:
   - **One-shot inputs.** `start_service` consumes inputs it cannot get again:
     - the block probes, moved out with `mem::take` ([line](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:631));
     - the role disks ([line](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:637));
     - kept roots taken rather than read ([example](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:1042)).
   - **Only some bootstraps can be re-run.** The relaunch path re-runs three hand-written bootstraps and the plan-relaunchable list, and nothing else: "A service whose bootstrap this ladder cannot re-run" ([relaunch](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1871), [list](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1978)).
   - **Dependents are never started later.** Only the bring-up loop starts a service ([`next_startable`](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:946)); the standing loop never starts an `Absent` one. A service that comes up on its second attempt leaves every service that depends on it "never started". The oracle's "a service that fails its first N starts and then comes up" would pass while its dependents stay down.
   - **The boot window.** The kernel's recovery ladder ends the round if the shell does not attach in time ([ladder](/data/yellow/libersystem/src/kernel/main.rs:1021)). Retries during bring-up therefore have to fit inside the boot window, as the plan already says for drivers. The plan says nothing of the kind for services.

   The plan also contradicts itself on `then reboot`. Two of its three default candidates, StorageService and PermissionManager ([candidates](/data/yellow/libersystem/docs/todo/P02M0204.md:50)), are `escalate`, so by its own rule they take no policy.

   **Correct the rules and the ServiceManager item.**
   - Separate a failed start from a death after `Ready`. A failed start is retryable for every service whose bootstrap inputs can be produced again; a death after `Ready` stays `transparent`-only.
   - Name, per service, which starts can be re-run, and what the others need (inputs re-minted, or kept until the start succeeds).
   - Add an item that starts `Absent` dependents from the standing loop once their dependencies are `Ready`.
   - Bound bring-up retries by the boot window.
   - Either drop the `escalate` candidates for `then reboot`, or define `then` for a failed start separately from a crash.

2. **High - The settings' store does not support the plan's authority, `reset`, or live application.**

   [The tool](/data/yellow/libersystem/docs/todo/P02M0204.md:62) stores settings in ConfigService, applies them at once, and requires an operator grant to change them. ConfigService as it is breaks all three:
   - **Anyone with a configuration client can write them.** Every key outside `device.policy.` is writable by any configuration client ([`set`](/data/yellow/libersystem/src/user/services/core/src/config_service.rs:224)). Seven services hold one, the shell among them, and the shipping `config` tool writes any key with `config set <key> <value>` ([tool](/data/yellow/libersystem/src/user/apps/tools/src/config.rs:1)). A setting in an ordinary key therefore bypasses the `restart-policy` grant entirely.
   - **`reset` cannot work.** It removes a key, and `remove` refuses every key outside `device.policy.` and every caller but that namespace's owner ([IDL](/data/yellow/libersystem/src/idl/config.lsidl:23), [service](/data/yellow/libersystem/src/user/services/core/src/config_service.rs:272)).
   - **Nobody is told about a change.** The interface has no notification, and the tree's convention is that "the owning service reads the new value at its next documented read point" ([tool](/data/yellow/libersystem/src/user/apps/tools/src/config.rs:5)). "Re-read on change" ([item](/data/yellow/libersystem/docs/todo/P02M0204.md:70)) has no trigger, and a `config set` would never be noticed.
   - **Drivers have no write path.** DeviceManager's ConfigService connection is the owner of `device.policy.` ([hand-off](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1365)), and ServiceManager cannot write there. The plan gives drivers only "a read view" ([item](/data/yellow/libersystem/docs/todo/P02M0204.md:72)).
   - **The old key stays behind.** `service.restart-budget` is seeded as a default ([seed](/data/yellow/libersystem/src/user/services/core/src/config_service.rs:91)) and read once per boot ([read](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:337)). The plan neither retires nor migrates it.

   **Correct the tool and wire items.**
   - Reserve owned namespaces: one for services, owned by ServiceManager through a seeded owner pair as `POLICYOWNER` is for DeviceManager; the drivers' settings under DeviceManager's existing one.
   - Widen `remove` to owned prefixes.
   - Route `restartctl set` and `reset` through the owners. Because the owners do the writing, they can apply a change at once without any notification.
   - Replace `service.restart-budget`.

3. **Medium - Restarts and retried starts block the supervisor's loop, so with retries one hanging service resets the machine.**

   The plan moves only the back-off into the event loop ([rule](/data/yellow/libersystem/docs/todo/P02M0204.md:33)). The start itself still blocks:
   - Both relaunch paths wait for the replacement's report with `recv_blocking` and no deadline ([relaunch](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1928), [planned](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:2021)).
   - So does `start_service` ([line](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:1047)).
   - `NoReport` exists only for a peer that closed ([line](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:1185)).

   A replacement that hangs before reporting therefore blocks the standing loop indefinitely. WatchdogService pets the hardware only for an `alive` answered by that loop ([contract](/data/yellow/libersystem/src/user/services/core/src/watchdog_service.rs:3)), so the machine resets. With `unlimited`, every boot repeats it.

   The rule's "a missed handshake" ([rule](/data/yellow/libersystem/docs/todo/P02M0204.md:31)) is not observable either, because there is no handshake deadline.

   **Correct the ServiceManager item.**
   - Make the start itself event-driven: launch, then await the report as a deadline in the loop, ending in `NoReport` (or a handshake timeout) after it.
   - Add to the oracle a fixture that never reports, with a watchdog armed.

4. **Medium - `then reboot` names a path that does not exist, and nothing stops a reboot loop.**

   "Through the existing recovery path" ([item](/data/yellow/libersystem/docs/todo/P02M0204.md:70)) has no referent. The kernel's ladder supervises SystemManager "only until the system comes up" ([comment](/data/yellow/libersystem/src/kernel/main.rs:689)), and no userspace path reboots because a service ran out of attempts.

   What exists to build on is ServiceManager's power capability ([held](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:688)) and its orderly shutdown, which already prepares the watchdogs.

   Nothing bounds a critical service that fails at every boot. It would turn an unattended machine into an endless reboot loop, which is worse than a degraded system that says what is wrong.

   **Correct the `then` setting and the ServiceManager item.**
   - Name the mechanism: an orderly reboot through ServiceManager's shutdown sequence.
   - Add a persisted count of consecutive policy reboots. After a bound, the system stays up degraded and reports the service.
   - Add that case to the oracle.

5. **Medium - The permanent/transient rule has no carrier for services, and its "input changed" trigger has no source.**

   [The rule](/data/yellow/libersystem/docs/todo/P02M0204.md:27) makes "a configuration the service rejects" permanent. A service has no way to say so today:
   - A failed start arrives as free text through `rt::fail_bootstrap` ([function](/data/yellow/libersystem/src/user/runtime/rt/src/lib.rs:2029)), which has 68 call sites and exits with status 0.
   - ServiceManager's reasons are `BootstrapRefused`, `NoReport` and `Faulted` ([enum](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:215)).
   - Transient causes travel the same way, for example StorageService's "classification probes: channel closed before announced count" ([call](/data/yellow/libersystem/src/user/services/storage/src/service.rs:4157)).
   - An exit status exists ([`exit_with`](/data/yellow/libersystem/src/user/runtime/rt/src/lib.rs:425), latched by the kernel and readable through process stats), but ServiceManager does not read it.

   "Tried again when its input changes - a new package generation, a configuration change" has no source: nothing tells ServiceManager about either.

   Drivers are different: their causes already carry retryability ([`FailureCause`](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:429)), and the plan should say it reuses them. It should also say that a quarantined binding stays outside automatic retry ([`decide_retry`](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:1023)). The rules are silent on quarantine.

   **Correct the rules and the policy-core item.**
   - Define the carrier: a permanence marker in `BOOTSTRAP_FAILURE`, or a reserved exit status.
   - List which existing `fail_bootstrap` call sites become permanent.
   - Reduce the retry trigger to an operator `start` or a reboot, unless a change notification is added under finding 2.

6. **Low - The plan adds a second status surface instead of extending the one that exists.**

   The `supervisor` interface already reports each component's state, restart count, last reason and last failure ([record](/data/yellow/libersystem/src/idl/observability.lsidl:165), [interface](/data/yellow/libersystem/src/idl/observability.lsidl:191)). `lssvc` renders it, drivers included ([tool](/data/yellow/libersystem/src/user/apps/tools/src/lssvc.rs:1)), and SystemGraphService merges it into the graph.

   The plan adds "a read view over driver bindings from DeviceManager" and a read operation on a new interface ([item](/data/yellow/libersystem/docs/todo/P02M0204.md:72)). That gives two views that will diverge.

   **Correct the wire item** by extending `supervisor-stat` with:
   - the policy in force;
   - the attempts spent;
   - when the next attempt is due;
   - a state word for "waiting to restart" (its documentation lists six).

   `restartctl show` then reads the same data, and the new interface only sets.

7. **Low - The drivers' half is left undefined where it meets DeviceManager's incident model.**

   [The DeviceManager item](/data/yellow/libersystem/docs/todo/P02M0204.md:71) does not say:
   - **What `then reboot` means for a driver.**
   - **How `delay`, `backoff` and `max-delay` combine with what exists.** DeviceManager already has per-attempt deadlines that double ([`attempt_deadline`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:229)), a fixed back-off table ([table](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:153)) and an incident time budget.
   - **What an attempt is when a node advances to its next candidate driver.**
   - **When overrides take effect.** They cannot be read before `POLICYCFG`, which arrives at the end of bring-up. Compiled defaults therefore govern every bring-up bind.

   **Correct the item** by stating each of these, and use the drivers' oracle case to check the bring-up and after-bring-up behaviour separately.

What holds:
- The diagnosis in [WHAT IS WRONG TODAY](/data/yellow/libersystem/docs/todo/P02M0204.md:12) matches the tree:
  - the budget never resets ([check](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1805));
  - the back-off is a sleep ([line](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1827));
  - the policy is read once ([read](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:324));
  - drivers get three attempts ([constant](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:61)).
- The parameter set is the size the owner asked for and not more.
- `--json` matches the tree: the shell turns `--json` into the `json` sub-form the other tools read ([shell](/data/yellow/libersystem/src/user/services/logic/src/shell_language.rs:31)).

Validation: read-only inspection of the plan and of the sources linked above. No plan or source file was modified, and nothing was built or booted.


PLANNER'S RESPONSE ON P02M0204 (2026-10-15T03:09:02Z):

Every finding was checked against the tree at 26faf58d before it was acted on. The plan was rewritten whole, with these new sections:
- what the tree already has;
- the rules, renumbered;
- where the settings live;
- the items, now eight.

Checking also found one inaccuracy the audit did not name: the plan's description of today's driver budget. It is corrected under finding 7.

1. **ACCEPTED, with a narrower remedy than proposed - excluding `escalate` services left the failed-start gap open.**

   Confirmed:
   - 24 of the 42 services are `escalate`.
   - `next_startable` runs only in the bring-up loop (`service_manager.rs:946`), and `supervise` never starts an `Absent` service.
   - A start can consume inputs the supervisor cannot produce twice:
     - the system StorageService's probes (`bootstrap.rs:631`), and every storage instance's role disk (`:637`);
     - the font catalogue's admin root, taken for PermissionManager (`:1042`);
     - the exclusive roles of DisplayService and the shell (manifest).
   - Privileges are duplicated, so they can be delivered again.
   - The kernel's ladder ends the round if the shell has not attached.

   Plan changes:
   - **Rules 1-3:**
     - A start that never reached `Ready` is a different case from a death after `Ready`.
     - A failed start is retried under the policy for every service whose start can run again in place, `escalate` ones included. The audit's point holds: nothing was handed out yet.
     - The exceptions are named from the code. The retryable set is declared beside `plan_relaunchable` and enforced by `check-bootstrap-plan`.
     - A death after `Ready` is restarted for `transparent` services, and `then` applies at once for `escalate` ones.
   - **Rule 6:** `Absent` dependents are started from the standing loop, with the bring-up start path's context moved there. `online` is sent when the set settles.
   - **Rule 7:** the boot window stays the outer bound through the kernel's ladder. The policy never extends it.
   - **The `then reboot` contradiction is resolved, not dropped.** Rules 2 and 3 now give StorageService and PermissionManager a `then` to apply, on a death and on a start that cannot run again.
   - **Item 3** carries all of it, and **the oracle** gains `escalate` and `transparent` fixtures whose dependents start after a late success.

   Not adopted: making the one-shot inputs reproducible, so those starts could be retried in place. That means re-minting disks and re-issuing an admin root the catalogue identifies its operator by, which is new cross-service protocol. For those services a reboot is the retry, and `then` offers it. Doing more would be over-engineering for this milestone.

2. **ACCEPTED - ConfigService's write authority, `remove`, and the absence of notifications.**

   Confirmed:
   - `set` checks only the `device.policy.` prefix (`config_service.rs:224-240`).
   - Seven services hold a configuration client, and the shipping `config` tool writes any key.
   - `remove` serves only `device.policy.`, and only to its owner.
   - The interface has no notification.
   - `service.restart-budget` is seeded at `config_service.rs:91`.

   Plan changes (the new section "WHERE THE SETTINGS LIVE", and item 4):
   - A second reserved namespace, `service.restart.`, owned by ServiceManager through a seeded `RESTARTOWNER` pair on the `POLICYOWNER` pattern. ServiceManager takes it again at every ConfigService start.
   - `remove` is allowed under each reserved namespace, to its owner.
   - `restartctl` changes settings only through ServiceManager. ServiceManager validates them, writes them through and applies them, so no notification is needed.
   - `service.restart-budget` and its seed are removed.

   One departure from the audit's remedy. The drivers' settings are not stored under DeviceManager's `device.policy.` namespace. All restart settings have one owner, and ServiceManager hands DeviceManager its effective policy on the control channel, at ConfigService's start and after every change. That leaves one write path, and DeviceManager reads nothing new. The oracle gains an ordinary client refused under `service.restart.`, and `reset` returning to the defaults.

3. **ACCEPTED - starts and relaunches block the supervisor's loop.**

   Confirmed:
   - `recv_blocking` with no deadline at `service_manager.rs:1928` and `:2021`, and at `bootstrap.rs:1047`.
   - `NoReport` only on a peer close.
   - WatchdogService pets the hardware only on an answered `alive`.

   The watchdog's defaults are a timeout of 60 s, a period of 15 s and a deadline of 5 s. A bounded blocking wait would survive them, but not a shorter timeout an operator may configure. So in the standing loop the report is awaited as an event.

   Plan changes:
   - **Rule 5:** the start is split into launch-and-deliver and awaiting the report.
     - In the standing loop the wait is an event in the wait set. During bring-up, before the watchdog's first answer, it is a bounded wait.
     - The deadline derives from the boot window, as DeviceManager's bind deadline does.
     - No report by the deadline is `NoReport`, which is transient: the instance is killed and counted.
   - **Item 3**, and **the oracle**, which gains a service that never reports with a watchdog armed, and the machine not reset.

4. **ACCEPTED - `then reboot` named a path that does not exist, and had no loop guard.**

   Confirmed:
   - The ladder watches SystemManager only until the system is up (`kernel/main.rs:689`).
   - The admin channel's `!reboot` is ServiceManager's orderly sequence, and it prepares the watchdogs first.
   - Nothing counts boots.

   Plan changes:
   - **`then reboot` is that orderly sequence.**
   - **The guard.** As soon as ConfigService is up, ServiceManager records the boot as not yet healthy: the count of consecutive such boots, this one included. A policy reboot is taken only if this boot's record was written and the count is at most three; otherwise it acts as `stop`, and the console and journal say why. A boot that stays up for `reset-after` with every `then reboot` service `Ready` clears the count.
   - **The oracle** gains the reboot, the bound and the clearing.

   One departure from the audit: the count is written at the start of each boot, not before each reboot. A write at reboot time would fail exactly when StorageService is the service that died, which would re-open the loop the guard exists to close.

5. **ACCEPTED, and the plan's own example corrected - the permanent/transient rule had no carrier.**

   Confirmed: 68 `fail_bootstrap` call sites, free text and exit status 0, and the three reasons. The classes the call sites actually report are:
   - a role or capability not delivered or not granted;
   - a missing or malformed init package;
   - absent hardware ("no framebuffer available");
   - the kernel refusing to attach an instance;
   - allocation and domain refusals;
   - peers that closed.

   None of them is "a configuration the service rejects", so the plan's example was wrong.

   Plan changes:
   - **Rule 4:** the first four classes are permanent and the rest transient, as are a crash and `NoReport`. The carrier is a class argument to `rt::fail_bootstrap`, sent as a marker in the report. Every call site is classified in the same change (item 2).
   - **The retry trigger is reduced to an operator `start` or the next boot.** Nothing tells ServiceManager about a new package generation or a configuration change, and adding that is not needed.
   - **Drivers:** their `FailureCause` retryability is reused. A quarantined device stays outside automatic retry, and a rebind after a sleep stays free.

6. **ACCEPTED, corrected - a second status surface beside the existing one.**

   Checking showed the driver rows of `supervisor-stat` are a snapshot taken after bring-up (`service_manager.rs:1110-1117`), not live state. The live per-device surface is DeviceManager's `binding-record`, which `lsdev` and the system graph read through `device.bindings`.

   Plan changes (item 6 and the tool section):
   - `supervisor-stat` is extended for services, and `binding-record` for drivers. Both gain the policy in force, the attempts spent, the next attempt due, and a "waiting to retry" state word.
   - `restartctl show` reads those two surfaces, and the new `restart-policy` interface only sets and resets.
   - No third view is added.

7. **ACCEPTED - the drivers' half was undefined.**

   Checking also found that the plan misdescribed today's driver behaviour. It said "three attempts per incident". The tree says otherwise:
   - `admit_attempt` counts "across this node's candidates and incidents in the boot", and "a fresh deadline does not reset spent";
   - DeviceManager: "Success ends the incident window, but automatic spending lasts for the boot".

   Drivers have the same never-resets problem as services.

   Plan changes:
   - **"WHAT IS WRONG TODAY"** is corrected.
   - **The drivers' defaults** say that `reset-after` is the change, while the attempts and the back-off are today's.
   - **Item 5 now states:**
     - `then stop` only for drivers, and the reason;
     - the policy replaces `MAX_AUTOMATIC_ATTEMPTS` and `BACKOFF_TICKS`, with the count reset after `reset-after`;
     - the per-attempt bind deadline, which doubles for patience, is unchanged;
     - attempts are counted as `admit_attempt` counts them, across candidates, and a missing artifact or a parked dependency spends nothing;
     - the compiled defaults govern until ServiceManager hands DeviceManager the stored policy, which covers phase one and the start of phase two;
     - quarantine, the sleep rebind and the volume's device are handled as rule 4 says.

Re-check after the changes. The plan now covers both supervisors under one model:
- failed starts and deaths are separated;
- one-shot starts are named and enforced by a check;
- dependents start late;
- no wait happens inside the supervisor, and the hardware watchdog is respected;
- the boot window stays the outer bound;
- `then reboot` is a real, bounded path;
- the classification has a carrier;
- the settings have a single owner that cannot be bypassed;
- the existing status surfaces are reused.

It agrees with P02M0203: the volume's device takes no policy. The owner's agreed defaults are kept, and the one behavioural change they imply for drivers is stated. Every rule has an oracle case that runs in QEMU. It is ready for implementation once the owner says so. The one choice left to the owner, which services take `then reboot` by default, is named for the start of the item and can be changed with `restartctl` at any time.
