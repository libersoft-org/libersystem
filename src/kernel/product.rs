// Product metadata, sourced at build time from product.conf (the single source
// of truth) by build.rs and injected as compile-time environment variables.

// THE NAME ONLY. The kernel prints one banner line and names three boot artifacts; the version,
// the website, the repository and the vendor are the userspace shell's banner, which reads them
// from its own crate. Six constants were declared here and one was used.
pub const NAME: &str = env!("PRODUCT_NAME");

// Boot artifact (boot module) filenames, also from product.conf. The kernel
// locates the modules the bootloader handed it by matching these on the path.
pub const INIT_PACKAGE: &str = env!("INIT_PACKAGE");
pub const VOLUME_PACKAGE: &str = env!("VOLUME_PACKAGE");
// The live medium's system volume image, handed over as a module by the loader.
pub const SYSTEM_VOLUME: &str = env!("SYSTEM_VOLUME");
