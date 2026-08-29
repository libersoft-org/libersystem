# Kernel Object And Capability Model

## Object Model
- Every kernel object has a stable koid and generation.
- Objects are held through `Arc<dyn KernelObject>` and typed downcasts after kind checks.
- Incrementing an object's generation revokes all capabilities that captured the old generation.

## Processes And Threads
- A Process owns an AddressSpace, HandleTable, resource Domain, fault state and thread set.
- Process termination closes handles and wakes process waiters.
- A Thread owns its kernel stack, architecture context, state and Process reference.
- Thread states cover ready, running, blocked and exited lifecycles.
- New threads can be created suspended and started once.

## Handles And Rights
- Handles are process-local slot identifiers with slot generations.
- A capability stores the object, rights, badge and object-generation snapshot.
- Lookup validates slot generation, object generation, object kind and required rights.
- Duplication only attenuates rights.
- Transfer is explicit through IPC and preserves least authority.
- Closing a handle refunds its Domain accounting charge.

## Core Objects
- Channel endpoints carry bounded messages and optional capability transfers.
- Events and interrupts wake waiters through object koids.
- Timers use absolute deadlines and integrate with scheduler timed waits.
- MemoryObjects represent controlled shared memory.
- AddressSpace objects own user mappings while retaining shared kernel mappings.
- Domains bound resources and provide cleanup scope.

## IPC And Service Policy
- Typed LSIDL bindings define wire formats and compatibility.
- PermissionManager remains the ordinary program-launch authority.
- ProcessService resolves and loads manifest-owned executables/providers.
- Service restart and dependency resolution must not create ambient authority.

## Security Invariants
- User pointers are validated before access.
- User stacks are non-executable; writable executable mappings are rejected.
- Dynamic imports must resolve to exactly one declared provider.
- Undeclared artifacts, changed grants and incompatible identities are rejected.
- Running processes retain immutable mappings across later artifact generations.

## Active Risks
- Keep handle, waiter and generation transitions race-safe under SMP and preemption.
- Avoid mixed provider generations within one process launch.
- Keep resource cleanup deterministic after timeout, fault or disconnect.
- Preserve bounded queues, message sizes, dependency depth and object counts.
