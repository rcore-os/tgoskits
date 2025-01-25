#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate percpu_macros;

#[cfg_attr(feature = "sp-naive", path = "naive.rs")]
mod imp;

pub use self::imp::*;
pub use percpu_macros::def_percpu;

#[doc(hidden)]
pub mod __priv {
    #[cfg(feature = "preempt")]
    pub use kernel_guard::NoPreempt as NoPreemptGuard;
}

cfg_if::cfg_if! {
    if #[cfg(doc)] {
        /// Example per-CPU data for documentation only.
        #[cfg_attr(docsrs, doc(cfg(doc)))]
        #[def_percpu]
        pub static EXAMPLE_PERCPU_DATA: usize = 0;
    }
}
