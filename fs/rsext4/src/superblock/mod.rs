//! Ext4 superblock definitions, helpers, and disk encoding.

mod constants;
mod default;
mod disk;
mod features;
mod geometry;
#[cfg(test)]
mod tests;
mod types;

pub use types::{Ext4Superblock, UUID};
