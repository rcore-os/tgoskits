macro_rules! opaque_handle {
    ($(#[$meta:meta])* $name:ident, $namespace:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[repr(transparent)]
        pub struct $name(usize);

        impl $name {
            /// Sentinel returned before the corresponding runtime object exists.
            pub const NONE: Self = Self(0);

            /// Creates a handle from the runtime-owned opaque value.
            ///
            /// # Safety
            ///
            /// A non-zero `raw` value must identify a live runtime-owned object
            /// or resource of this exact handle type. The caller must uphold all
            /// provenance, lifetime, pinning, aliasing, and ownership invariants
            /// required by operations that consume or dereference the handle.
            /// Use [`Self::NONE`] for the absent-handle sentinel.
            #[doc = concat!(
                "\n```compile_fail\n",
                "use ax_task::", $namespace, "::", stringify!($name), ";\n",
                "let _handle = ", stringify!($name), "::from_raw(1);\n",
                "```"
            )]
            pub const unsafe fn from_raw(raw: usize) -> Self {
                Self(raw)
            }

            /// Returns the runtime-owned opaque value.
            pub const fn into_raw(self) -> usize {
                self.0
            }

            /// Returns whether this is the absent-handle sentinel.
            pub const fn is_none(self) -> bool {
                self.0 == 0
            }
        }
    };
}
pub(super) use opaque_handle;
