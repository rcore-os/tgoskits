pub mod arm64;

#[allow(clippy::module_inception)]
pub mod crc32c;

// Re-export commonly used functions from crc32c module
pub use crc32c::{
    crc32c, crc32c_append, crc32c_finalize, crc32c_init, ext4_crc32c_seed_from_superblock,
    ext4_superblock_has_metadata_csum,
};
