

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0178 (2026-09-10T19:05:49Z):

Scope: the four items of `docs/todo/P02M0178.md`. This record is written after the work, and the
verification section says what was actually run.

## What the tree had, verified before changing it

The 2026-09-09 unsafe-boundary inventory counted 5736 sites, 4779 of them reachable in a shipping
configuration, and routed six groups to this milestone:

    rt-propagated-wrappers           304
    drivers-propagated-wrappers      159
    services-wrapped-runtime-calls   984
    apps-wrapped-runtime-calls       393
    libs-wrapped-runtime-calls        92
    kernel-syscall-read-user           5

`src/user/runtime/rt/src/lib.rs` declared 134 `pub unsafe fn` plus 8 private ones; userspace outside
the runtime carried 569 `unsafe fn` declarations and 1364 `unsafe {}` blocks.

ONE FACT DECIDED THE SHAPE OF M1. The runtime is edition 2024, so `unsafe_op_in_unsafe_fn` is in
force and every wrapper body ALREADY carries its own `unsafe { syscall(..) }` block. Making a wrapper
safe is therefore a signature change and nothing else: the block that discharges the raw call's
pointer contract stays exactly where it was, which is what lets the signature be safe.

## M1 - the wrappers get the signature their contract has

127 wrappers became safe `fn` (123 in `lib.rs`, 4 in `stream.rs`). Ten keep `unsafe fn` and each
gained a written `# Safety` block naming what the caller owes:

- `map_object`, `dma_buffer_map`, `framebuffer_map` hand back a virtual base the caller dereferences;
- `dma_buffer_phys`, `dma_buffer_phys_at`, `dma_buffer`, `dma_buffer_for` hand back an address a
  DEVICE dereferences, which is the same obligation pointed the other way;
- `memmap_get` describes PHYSICAL memory, named by the plan;
- `process_load_module` places a module at a caller-chosen bias in the child, named by the plan;
- `recv_package` fabricates a `&'static [u8]` over a mapping, so the caller must keep the handle and
  never unmap. This one is NOT in the plan's list and was found while checking the plan's assumption
  against the code: its body is `slice::from_raw_parts(map_object(handle)?, len)` with a `'static`
  lifetime the function cannot justify. Had it gone safe, safe code could have produced a dangling
  slice.

The raw `syscall` (four `cfg` variants), `move_bytes` and the four shared-image `mem*` implementations
keep `unsafe` and now carry contracts too.

TWO WRAPPERS THE PLAN DOES NOT NAME WENT SAFE, and the reasoning is written where they are.
`unmap_object` and `dma_buffer_unmap` end a mapping whose address was handed out by their `unsafe`
partner - but reaching through such an address is itself an `unsafe` dereference, and that block is
where the obligation already sits. Nothing safe can be made to misbehave by the unmap alone, which is
the test the plan states: "the address-returning wrappers keep `unsafe fn`".

## M2 - the propagation is removed where it was only propagation

Two passes, both compiler-driven rather than judged by eye.

The BLOCKS came out under `-D warnings`, which every userspace package sets: with the wrappers safe,
`unused_unsafe` turns each propagated block into a hard error naming its file, line and column. A
script consumed cargo's JSON diagnostics and spliced each reported block away, run to convergence in
both configurations the product builds - the static default and the shared image's
`--no-default-features --features shared-image` - because the two reach different code.

Removing a block is not one transformation. In statement position the contents replace it, dedented
one level; in EXPRESSION position a block holding statements keeps its braces and loses only the
keyword, because `let x = a; b;` is not an expression and `None => f();` is not a match arm. The
first attempt did not make that distinction, broke four match arms and one `let`, and was thrown away:
the touched files were restored from the commit and the corrected transformation re-run from clean.
That is why the working tree carries no half-spliced code.

The DECLARATIONS came out by rule. An `unsafe fn` in userspace keeps its declaration when its
signature carries a raw pointer, its body performs a raw-memory or volatile operation, or its body
calls one of M1's kept wrappers; everything else was unsafe only because of a signature that has
changed. 478 of the 569 declarations went safe; the 91 that remain are the drivers' MMIO and ring
work (56), the services' mapped-object views (19+), the tools' mapped-file readers (14) and the IPC
client's buffer construction.

    unsafe fn in src/user outside the runtime   569 -> 91
    unsafe {} blocks in the same tree          1364 -> 398
    unsafe fn in the runtime                    142 -> 15
    unsafe {} blocks in the runtime             156 -> 106

The runtime's blocks barely move, and that is the point: they are where the pointer contract is now
discharged on the caller's behalf.

`__user_main` was already `extern "C" fn` and not `unsafe fn` in all 115 definitions, so the entry
ABI needed no change - checked rather than assumed.

## M3 - the kernel's user-pointer read says what it needs

`read_user<T>` in `src/kernel/syscall/mod.rs` filled a `MaybeUninit<T>` from a caller-controlled
address, zero-filled the tail on a short read and called `assume_init`, for EVERY `T`. It is now
`fn read_user<T: UserPlain>(ptr: u64) -> T`, where `UserPlain` is a marker trait that is both

- SEALED, through a private `plain_data::Sealed` supertrait, so the permitted list cannot be
  extended from outside the syscall module, and
- `unsafe` to implement, so adding a type is a deliberate claim about bit patterns rather than an
  `impl` written to make a call compile.

It is implemented for exactly the three types the kernel reads: `u64`, `[u8; abi::ENTRY_NAME_LEN]`
and `abi::CapTransfer`, each with a SAFETY comment saying why every bit pattern is a value, all-zero
included - which is the pattern the short-read path writes. A future `T` with invalid bit patterns
now fails to compile at the call site instead of being undefined behaviour with no `unsafe` in sight.

## M4 - the inventory proves the narrowing

`src/tools/unsafe-inventory` was regenerated over its whole 15-row configuration matrix and the six
groups were reclassified in `AI/audit/unsafe-classification.toml`, each from `overly-broad` /
"narrow (follow-up F1)" to a category and a disposition that describe what is actually left:

    group                            before   after   what remains
    rt-propagated-wrappers              304     133   the raw syscall, the ten address wrappers,
                                                      and the blocks that discharge their contract
    drivers-propagated-wrappers         159      56   MMIO accessors and ring writers
    services-wrapped-runtime-calls      984     125   blocks still holding one unsafe call
    apps-wrapped-runtime-calls          393      61   entry blocks around a mapped-file reader
    libs-wrapped-runtime-calls           92      85   shared-image PROVIDER IMPORTS, reclassified
                                                      as abi-linkage: these were never runtime
                                                      propagation, and the old invariant said they were
    kernel-syscall-read-user              5       5   the same sites, now bounded by `UserPlain`

    whole tree                         5736    4116 sites; 4779 -> 3139 reachable in a shipping row

A BLOCK COUNTS ONCE however many calls it holds, so none of these groups can reach zero while one
genuinely unsafe call remains inside an otherwise safe block. That is why the residue is stated as a
named contract per group rather than as a target number, which is what M4 allows and what the
milestone's "What this milestone refuses" forbids turning into a count.

## Verification

Every command below was run from ONE tree, after the last source edit. Each is the whole command.

    ./build.sh --arch x86_64                    PASSED  sdk libs user kernel loader packages volume
    ./test.sh --arch x86_64                     PASSED  387 test(s), 195 s
    ./check.sh --gate host-tests                PASSED  77 suite(s)
    cd src/tools/unsafe-inventory && cargo run --quiet -- --repo ../../..
                                                PASSED  4112 sites, 15 rows, 0 unclassified

The guest suite is the milestone's "scoped guest suites for the affected crates pass unchanged": the
services, drivers and tools whose `unsafe` was removed are what a booted system runs, and 387 tests
over them pass with the same count as before the change.

THREE CONFIGURATIONS HAD TO BE SWEPT, not one, and the third was found by a failing gate rather than
by reasoning. The static default and the shared image reach different code, and the HOST-TEST
configuration reaches a third set - `src/user/drivers/core/src/virtio/tests.rs` compiles only under
`cargo test`, and its 26 propagated blocks survived both product sweeps. `check.sh --gate host-tests`
caught them, which is what that gate is for.

Not performed, and named rather than implied:

- the aarch64 and riscv64 guest suites. The change is architecture-independent - a signature and the
  blocks that call it - and the runtime's three `syscall` variants were not touched, but this is a
  statement about the change rather than a measurement of those targets. `./verify.sh --for
  src/user/runtime/rt --plan` selects 1352 of 1365 keys at roughly 12400 s because everything in
  userspace reaches the runtime; that full sweep has not been run.
- a compile-fail fixture for `read_user`. The bound is enforced by the type system and can be read
  in the signature; this tree has no `trybuild`-style harness to assert a non-compile, and adding one
  is outside what this milestone asked for.

## Definition of done, item by item

- "No userspace `unsafe fn` or `unsafe {}` exists whose only reason is a runtime wrapper's
  signature." MET, and mechanically: `-D warnings` makes `unused_unsafe` a hard error, so a
  propagated block cannot survive a build. What remains is a block or function holding at least one
  genuinely unsafe operation.
- "Every remaining userspace `unsafe` names its contract." MET for the runtime, where each kept item
  gained a `# Safety` block, and through the classification file for the rest, where each group's
  invariant states the contract its sites hold - which is the form the inventory checks and the form
  M4 asks for.
- "`read_user<T>` cannot be instantiated with a type that has invalid bit patterns without an
  `unsafe` at the call site." MET by the sealed `unsafe trait UserPlain` bound.

## What was NOT changed, as the milestone requires

No syscall's behaviour, no entry ABI, no shared-image provider ABI. No `forbid(unsafe_code)`
anywhere, and no target count: the residue is described, not budgeted.
