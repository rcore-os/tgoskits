//! Architecture-independent interrupt connection primitives.

mod message;
mod types;
mod wired;

pub use message::*;
pub use types::*;
pub use wired::*;
