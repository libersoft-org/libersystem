# LiberSystem

## Table of contents

- [**About**](#about)
- [**Documentation**](#documentation)
- [**Installation**](#installation)
- [**License**](#license)
- [**Contribution**](#contribution)
- [**Donations**](#donations)
- [**Star history**](#star-history)

## About

**LiberSystem** is a modern operating system written from scratch in Rust. It is built around a small, memory-safe microkernel and a typed object / capability model - every resource has a clear type and is reached through an explicit, unforgeable capability that carries its own rights.

The kernel is a small, safe arbiter; system services and drivers run as isolated, restartable components that talk to each other over stable, typed contracts. Security is capability-based and least-privilege by construction, the system is SMP-aware from the ground up, and memory safety comes from the Rust language itself rather than from discipline.

This is an early-stage project under active development. It is not yet a general-purpose OS release.

## Documentation

- [**Concept**](./docs/CONCEPT_EN.md) - the **LiberSystem design document**: object and capability model, IPC, services, and the roadmap ([česká verze](./docs/CONCEPT_CZ.md)).
- [**Threat model**](./docs/THREAT_MODEL.md) - what the system defends against, where the **security boundaries** are, and how each boundary is turned into an executable check.
- [**LSIDL**](./docs/LSIDL.md) - the **LiberSystem Interface Definition Language**: the language services are described in, from which the wire codec, clients, servers, and docs are generated.
- [**System interfaces**](./docs/gen/system.md) - the generated reference for the **`liber:system` package**: every record, enum and service interface, as defined in the LSIDL sources.
- [**LiberFS**](./docs/LIBERFS.md) - the **native filesystem**: the copy-on-write, extent-mapped, checksummed and snapshotting on-disk format, and how it compares with FAT, NTFS, ext4, XFS, Btrfs and ZFS.
- [**Debugging**](./docs/DEBUG.md) - the **debugging toolbox**: the lab harness that drives a live instance, timing and tracing, GDB, packet capture, and the test suite.
- [**Performance notes**](./docs/PERF.md) - **measured before/after numbers** for the milestones whose goal was a performance improvement.

## Installation

- For build and installation instructions follow [**this document**](./INSTALL.md).

## License

- This software is developed under the license called [**Unlicense**](./LICENSE).

## Contribution

If you are interested in contributing to the development of this project, we would love to hear from you! Developers can reach out to us through one of the contact methods listed on [**our contacts page**](https://libersoft.org/contacts). We prefer communication through our Telegram chat group, but feel free to use any method that suits you.
In addition to direct communication, you are welcome to contribute by submitting issues or pull requests on our project repository. Your insights and contributions are valuable to us. We look forward to collaborating with you!

## Donations

Donations are important to support the ongoing development and maintenance of our open source projects. Your contributions help us cover costs and support our team in improving our software. We appreciate any support you can offer.

To find out how to donate our projects, please navigate here:

[![Donate](https://raw.githubusercontent.com/libersoft-org/documents/main/donate.png)](https://libersoft.org/donations)

Thank you for being a part of our projects' success!

## Star history

If you support our open source software, consider starring this repository. Thank you!

[![Star History Chart](https://api.star-history.com/svg?repos=libersoft-org/libersystem&type=Date)](https://star-history.com/#libersoft-org/libersystem&Date)
