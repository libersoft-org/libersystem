# The capability transfer model

An executable specification of the handle and channel state machines, and the configurations TLC
Its claim is deliberately finite: for each published
configuration, every reachable state satisfies the listed invariants. That is stronger than a design
argument and weaker than a proof of the kernel - TLC explores a bounded abstraction of the parts
`MODEL_MAP.md` names, and nothing here says anything about unmodeled code, unsafe Rust, assembly,
the compiler, hardware, or executions outside the stated bounds.

## The files

| | |
| --- | --- |
| `MODEL_MAP.md` | the traceability map: model state and actions against the Rust items they abstract, the lock that makes each action atomic, where a syscall can be interrupted, and the three ledgers |
| `Capability.tla` | the reusable half - what a slot, a capability, a message and a charge ARE. No variables, no actions |
| `Transfer.tla` | the composed specification: the transitions, and the invariants they are checked against |
| `spike.cfg` | the smallest configuration that can show a transfer racing a close |
| `handles.cfg` | the same, with duplication reachable and a second object type |
| `revoke-test-only.cfg` | the only configuration where the TEST-ONLY revocation helper exists |
| `MEASUREMENTS.md` | what each configuration costs and covered, with the digests it was measured against |

## Running it

TLC is a pinned artifact rather than something this repository builds. Fetch it once:

    ./bootstrap.sh tla2tools

Then, from the repository root, one configuration at a time:

    java -cp .build/tools/tla2tools.jar tlc2.TLC \
        -workers 1 -config docs/spec/capability/spike.cfg docs/spec/capability/Transfer.tla

WHICH CONFIGURATION IS PART OF THE RESULT. `revoke-test-only.cfg` models a helper that exists only
in tests, so its result describes that helper and not the production authority model; the other two
remove the action entirely rather than disabling it behind a guard.

The verification gate runs the same command with no network available: `toolchain.lock` pins the
JAR by SHA-256, `bootstrap.sh` is the one command that fetches and verifies it, and a gate that
cannot find it prints that command rather than reaching for a mirror.

## Reading a counterexample

TLC prints the shortest behaviour that reaches a violating state, one action per step, naming the
action and the line it is defined on. Three states is `Init` plus two actions - which is what the
first run of `spike.cfg` produced, and what a defect this small looks like when a model finds it
instead of a machine.
