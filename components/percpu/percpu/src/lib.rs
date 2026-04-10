#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate ax_percpu_macros;

cfg_if::cfg_if! {
    if #[cfg(feature = "sp-naive")] {
        #[path = "naive.rs"]
        mod imp;
    } else if #[cfg(feature = "custom-base")] {
        #[path = "custom/mod.rs"]
        mod imp;
    }else{
        mod imp;
    }
}

pub use ax_percpu_macros::def_percpu;

pub use self::imp::*;

#[doc(hidden)]
pub mod __priv {
    #[cfg(feature = "preempt")]
    pub use ax_kernel_guard::NoPreempt as NoPreemptGuard;
}

cfg_if::cfg_if! {
    if #[cfg(doc)] {
        /// Example per-CPU data for documentation only.
        #[cfg_attr(docsrs, doc(cfg(doc)))]
        #[def_percpu]
        pub static EXAMPLE_PERCPU_DATA: usize = 0;
    }
}
