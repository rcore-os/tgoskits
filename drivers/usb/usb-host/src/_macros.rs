macro_rules! define_int_type {
    ($name:ident, $id:ty) => {
        #[repr(transparent)]
        #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub $id);

        impl $name {
            pub fn raw(&self) -> $id {
                self.0
            }
        }

        impl From<$id> for $name {
            fn from(val: $id) -> Self {
                Self(val)
            }
        }

        impl From<$name> for $id {
            fn from(val: $name) -> Self {
                val.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}
