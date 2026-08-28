# Architectures

One kernel, three machine prologues. This document says what each port is proven on today - not how
much code it shares, and not a claim about hardware nobody has run it on.

## What each port runs on

| | **`x86_64`** | **`aarch64`** | **`riscv64`** |
| --- | --- | --- | --- |
| Entry path | UEFI loader | UEFI loader, and direct `-kernel` | UEFI loader, and direct `-kernel` |
| Machine description | ACPI (RSDP, MADT, SRAT/SLIT) | device tree, or a named no-DT profile | device tree |
| Interrupt controller | APIC / IO-APIC, MSI-X | GICv2, GICv2m, GICv3, GICv3+ITS | AIA/IMSIC. The PLIC is READ FROM THE TREE AND DISCARDED - `boot.rs` prints its address and then drops it, and the only external-interrupt handler dispatches IMSIC EIDs. A machine with no IMSIC has no external interrupts here. |
| Timer | local APIC, TSC-calibrated | CNTP (generic timer) | SBI timer |
| SMP bring-up | ACPI MADT + INIT-SIPI-SIPI | PSCI `CPU_ON` | SBI HSM |
| DMA isolation | virtio-iommu, on by default | none yet | none yet |
| QEMU profiles | `q35` with and without a controller | `virt` at GICv2, GICv3, GICv3/ITS | `virt` with `aia=aplic-imsic` |
| Physical hardware | not qualified | not qualified | not qualified |

**No line of this table is a claim about a physical machine.** Every profile named here is an
emulated one, and passing on it is evidence that the discovery path works on the machine QEMU
describes - not that any real board has been booted.

## The rule the table exists to state

**A port reads its machine or refuses it by name.** It does not guess.

That sentence is what the last row costs. A kernel that falls back to a plausible address when
firmware says something it does not understand appears to work on the one machine those addresses
belong to, and writes into nothing on every other. So each port answers three questions from what the
firmware actually published, and each has a named refusal for the case where it cannot:

- **Where the machine description is.** A pointer that was handed over and does not carry an FDT
  header is a firmware error, not an absence: the port says so and stops rather than going looking
  for a tree somewhere else. A machine that publishes none at all gets the static descriptor only
  where a named profile authorises it - the QEMU no-DT regression profile - and a refusal otherwise.
- **Which interrupt controller it has.** A device tree that describes no controller this kernel can
  drive is fatal at parse time rather than at the first MMIO write, because without a controller
  there is no timer interrupt and a kernel without one cannot claim a working scheduler.
- **Which harts or cores exist, and where.** A layout the port cannot address - guest-indexed or
  group-indexed IMSIC files, interrupt files whose index is not their hart id, a topology whose
  distance table contradicts itself - is refused by name, and the MSI path is taken out of service
  rather than pointed at a computed address inside a layout nobody confirmed.

## What each port has to provide

The portable kernel sits on a small arch contract: context switch, per-CPU state, page tables, a
timer, an interrupt controller, a way to wake another core, and a way to reach PCI configuration
space. Everything above it - the scheduler, the object and capability model, IPC, the drivers - is
the same code on all three.

Three things are deliberately NOT portable, and each is a machine prologue of its own:

- **The boot hand-off.** x86_64 arrives through UEFI with a memory map and an RSDP; the other two may
  arrive that way or through a direct `-kernel` boot with a device-tree pointer in a register.
- **Bring-up of secondary cores.** ACPI plus INIT-SIPI-SIPI, PSCI `CPU_ON`, and SBI HSM are three
  different conversations with three different agents, and none of them abstracts into the others.
- **The interrupt controller.** An APIC, a GIC and a PLIC/IMSIC differ in what a vector IS, not only
  in how one is programmed - an SPI, an LPI and an EID are not the same object.

## How the table is held to

Each row is checked rather than described. `check-qemu-arch-profiles.sh` boots every named profile at
one core and at four, and requires by name the tests that drive what the profile claims: the
controller it discovered, at least five timer ticks, every declared core online, an MSI acquired and
delivered and released on whichever MSI controller the machine has, and - at four cores - a remote
wake IPI, a real TLB shootdown acknowledgement and a thread scheduled on a secondary core.

A profile with no MSI backend at all - a GICv3 with its ITS turned off - says outright that it makes
no MSI claim, because asking it an MSI question would prove nothing. That is the shape of every row
here: a claim, or an explicit statement that none is being made.
