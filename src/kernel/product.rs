// Product metadata, sourced at build time from product.conf (the single source
// of truth) by build.rs and injected as compile-time environment variables.
#![allow(dead_code)]

pub const NAME: &str = env!("PRODUCT_NAME");
pub const VERSION: &str = env!("PRODUCT_VERSION");
pub const WEBSITE: &str = env!("PRODUCT_WEBSITE");
pub const GITHUB: &str = env!("PRODUCT_GITHUB");
pub const VENDOR: &str = env!("PRODUCT_VENDOR");
pub const VENDOR_URL: &str = env!("PRODUCT_VENDOR_URL");

// Boot artifact (boot module) filenames, also from product.conf. The kernel
// locates the modules the bootloader handed it by matching these on the path.
pub const INIT_PACKAGE: &str = env!("INIT_PACKAGE");
pub const VOLUME_PACKAGE: &str = env!("VOLUME_PACKAGE");
