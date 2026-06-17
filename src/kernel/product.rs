// Product metadata, sourced at build time from product.conf (the single source
// of truth) by build.rs and injected as compile-time environment variables.
#![allow(dead_code)]

pub const NAME: &str = env!("PRODUCT_NAME");
pub const VERSION: &str = env!("PRODUCT_VERSION");
pub const WEBSITE: &str = env!("PRODUCT_WEBSITE");
pub const GITHUB: &str = env!("PRODUCT_GITHUB");
