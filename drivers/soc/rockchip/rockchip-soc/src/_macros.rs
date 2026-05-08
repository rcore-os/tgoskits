macro_rules! def_id {
    ($n:ident, $t:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $n($t);

        impl From<$t> for $n {
            fn from(value: $t) -> Self {
                Self(value)
            }
        }

        impl From<usize> for $n {
            fn from(value: usize) -> Self {
                Self(value as _)
            }
        }

        impl From<$n> for $t {
            fn from(id: $n) -> Self {
                id.0
            }
        }

        impl core::fmt::Display for $n {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!(stringify!($n), "({})"), self.0)
            }
        }

        impl $n {
            pub const fn value(&self) -> $t {
                self.0
            }

            pub const fn new(value: $t) -> Self {
                Self(value)
            }
        }

        impl core::ops::RangeBounds<$n> for $n {
            fn start_bound(&self) -> core::ops::Bound<&$n> {
                core::ops::Bound::Included(self)
            }

            fn end_bound(&self) -> core::ops::Bound<&$n> {
                core::ops::Bound::Included(self)
            }
        }
    };
}
