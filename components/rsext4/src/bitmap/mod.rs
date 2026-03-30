//! Bitmap primitives for block and inode allocation tracking.

pub mod bitmap_utils;
mod block;
mod error;
mod inode;

pub use block::BlockBitmap;
pub use error::BitmapError;
pub use inode::InodeBitmap;
