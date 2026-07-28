#![doc = include_str!("../README.md")]
#![no_std]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(all(axtest, feature = "axtest"))]
/// Coverage tests for scoped local storage.
pub mod axtest;

mod boxed;
mod item;
mod scope;

pub use item::{Item, LocalItem, ScopeItem, ScopeItemMut};
pub use scope::{
    ActiveScope, Scope, ScopeCell, ScopeCellBusy, ScopeCellReadGuard, ScopeCellWriteGuard,
};
