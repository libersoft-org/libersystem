# icdprobe

A SYNTHETIC ICD, AND THE FIRST FOREIGN ARTIFACT IN THIS IMAGE. It exists for two jobs that turn out
to be one artifact:

  the PACKAGING job    something has to enter the manifest, the build, the cache and the identity
                         graph as a foreign artifact rather than as an exception, and then pass the
                         same relocation, W^X, export-owner and provider-closure audits every Rust
                         library passes. A path nothing travels is a path nobody has checked.
  the DISCOVERY job    the selection slot binds a provider by kind, and `vulkan-icd` is the kind it
                         was designed for. Proving a slot binds needs an ICD to bind, and the real
                         one is a separately authorised milestone away.

WHAT IT IS NOT. It implements no Vulkan entry point and returns no function pointer for any of them.
It is the Loader/Driver INTERFACE - the two exports an ICD must have and the version negotiation
between them - and nothing behind it.

THE TWO EXPORTS ARE THE WHOLE SURFACE, which is the substrate's decision and not a simplification:
`vk_icdNegotiateLoaderICDInterfaceVersion` and `vk_icdGetInstanceProcAddr`, with versions 2 through 6
admitted. Version 1 has no negotiation function, version 0 has a different bootstrap shape, version 7
lets a driver stop exporting what this substrate resolves, and version 4's
`vk_icdGetPhysicalDeviceProcAddr` would be a third export. Each of those is refused, and the
refusals need ICDs shaped to be refused - which is why the neighbouring files exist.

NEGOTIATION IS A NEGOTIATION, so one artifact covers both ends of the admitted range: the caller
writes what it wants, this writes back what it will do, and the answer is the lower of the two. That
is what lets the gate exercise the LOWEST and the HIGHEST admitted version without two drivers whose
difference is a constant.
