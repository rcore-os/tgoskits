//! The I/O Prelude.
//!
//! The purpose of this module is to alleviate imports of many common I/O traits
//! by adding a glob import to the top of I/O heavy modules:
//!
//! ```
//! # #![allow(unused_imports)]
//! use axio::prelude::*;
//! ```

pub use crate::{BufRead, IoBuf, IoBufExt, IoBufMut, IoBufMutExt, Read, Seek, Write};
