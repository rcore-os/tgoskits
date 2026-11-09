//! Target bindings generated from the shared suite declaration.

macro_rules! register_tests {
    ($($(#[$condition:meta])* ($id:literal, $name:literal, $runner:ident, $body:block);)*) => {
        $(
            $(#[$condition])*
            #[cfg(feature = "ax-std")]
            fn $runner() -> crate::TestResult $body
        )*
        pub(crate) const SELECTED_TESTS: &[crate::TestCase] = &[
            $(
                $(#[$condition])*
                #[cfg(feature = "ax-std")]
                crate::TestCase::new($id, $name, $runner),
            )*
        ];
    };
}

// Only registration data is generated; implementations remain normal crate modules.
include!(concat!(env!("OUT_DIR"), "/suite_bindings.rs"));
