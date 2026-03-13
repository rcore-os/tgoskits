#![doc = include_str!("../README.md")]
#![no_std]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

mod boxed;
mod item;
mod scope;

pub use item::{Item, LocalItem, ScopeItem, ScopeItemMut};
pub use scope::{ActiveScope, Scope};
