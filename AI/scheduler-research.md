# Scheduler Reference

## Current Model
- Each CPU owns a run queue, current thread, zombie slot and idle stack pointer.
- Threads stay on their assigned CPU; cross-core balancing is not implemented.
- Scheduling combines explicit yield/block/exit with timer-driven preemption.
- Timer preemption is disabled until per-CPU state and scheduler initialization complete.
- Remote enqueue sends a wake IPI to a halted target CPU.

## Context Switching
- Architecture assembly saves callee state and switches kernel stacks.
- Address-space changes restore the target page-table root.
- User entry stacks are per thread so preemption from user mode is safe.
- The idle/bootstrap context runs when no thread is ready.
- Exited threads are reaped after switching away from their own stack/address space.

## Blocking And Deadlines
- Object waits are split across 64 koid buckets.
- Timed waits also enter a separate short deadline list.
- Wake uses a blocked-to-ready compare-exchange so concurrent wake sources enqueue once.
- A resumed multi-object waiter removes stale bucket entries.
- Periodic housekeeping deadlines do not prevent `run_until_idle()` from settling.

## Main Operations
- Spawn on current CPU or a selected CPU.
- Create a suspended user thread and start it once.
- Yield and requeue the current thread.
- Block on one or more object koids with an optional deadline.
- Wake waiters for an object or expired deadline.
- Exit, retire and reap a thread.

## Invariants
- A runnable thread is on at most one run queue.
- A blocked thread is off all run queues.
- Only one wake transition may claim a blocked thread.
- Queue operations use the owning CPU scheduler slot.
- Kernel address-space state is restored before reclaiming a dead process.
- Interrupt-safe locks prevent timer preemption inside scheduler critical sections.

## Validation
- CPU-bound user threads must be preempted without making syscalls.
- Blocking tests cover object wake, timeout and multi-object races.
- SMP tests cover remote spawn and wake IPI behavior.
- `run_until_idle()` must terminate despite periodic service waits.

## Remaining Risks
- No automatic load balancing or thread migration.
- The timed-wait list can become expensive if most waits acquire deadlines.
- Fairness depends on timer cadence and cooperative kernel paths.
- Real-time priorities and affinity policy are not implemented.
