/// A trait for memory address types.
///
/// Memory address types here include both physical and virtual addresses, as well as any other
/// similar types like guest physical addresses in a hypervisor.
pub trait MemoryAddr:
    // The address type should be trivially copyable. This implies `Clone`.
    Copy
    // The address type should be convertible to and from `usize`.
    + From<usize>
    + Into<usize>
{
    // Empty for now.
}

/// Implement the `MemoryAddr` trait for any type that satisfies the required bounds.
impl<T> MemoryAddr for T where T: Copy + From<usize> + Into<usize> {}

/// Creates a new address type by wrapping an `usize`.
///
/// For each `$vis type $name;`, this macro generates the following items:
/// - Definition of the new address type `$name`, which contains a single private unnamed field of
///   type `usize`.
/// - Default implementations (i.e. derived implementations) for the following traits:
///   - `Copy`, `Clone`,
///   - `Default`,
///   - `Ord`, `PartialOrd`, `Eq`, and `PartialEq`.
/// - Implementations for the following traits:
///   - `From<usize>`, `Into<usize>` (by implementing `From<$name> for usize`),
///   - `Add<usize>`, `AddAssign<usize>`, `Sub<usize>`, `SubAssign<usize>`, as well as
/// - Two `const` methods to convert between the address type and `usize`:
///   - `from_usize`, which converts an `usize` to the address type, and
///   - `as_usize`, which converts the address type to an `usize`.
/// - Methods to align the address, namely:
///   - `align_down`, `align_up`, `align_offset`, `is_aligned`, `align_down_4k`, `align_up_4k`,
///     `align_offset_4k`, and `is_aligned_4k`, which correspond to the functions with the same
///     names in the crate root.
///
/// # Example
///
/// ```
/// use memory_addr::def_usize_addr;
///
/// def_usize_addr! {
///     /// A example address type.
///     #[derive(Debug)]
///     pub type ExampleAddr;
/// }
///
/// # fn main() {
/// const EXAMPLE: ExampleAddr = ExampleAddr::from_usize(0x1234);
/// const EXAMPLE_USIZE: usize = EXAMPLE.as_usize();
/// assert_eq!(EXAMPLE_USIZE, 0x1234);
/// assert_eq!(EXAMPLE.align_down(0x10usize), ExampleAddr::from_usize(0x1230));
/// assert_eq!(EXAMPLE.align_up_4k(), ExampleAddr::from_usize(0x2000));
/// # }
/// ```
#[macro_export]
macro_rules! def_usize_addr {
    (
        $(#[$meta:meta])*
        $vis:vis type $name:ident;

        $($tt:tt)*
    ) => {
        #[repr(transparent)]
        #[derive(Copy, Clone, Default, Ord, PartialOrd, Eq, PartialEq)]
        $(#[$meta])*
        pub struct $name(usize);

        impl $name {
            #[doc = concat!("Converts an `usize` to an [`", stringify!($name), "`].")]
            #[inline]
            pub const fn from_usize(addr: usize) -> Self {
                Self(addr)
            }

            #[doc = concat!("Converts an [`", stringify!($name), "`] to an `usize`.")]
            #[inline]
            pub const fn as_usize(self) -> usize {
                self.0
            }
        }

        impl $name {
            /// Aligns the address downwards to the given alignment.
            #[inline]
            pub fn align_down<U>(self, align: U) -> Self
            where
                U: Into<usize>,
            {
                Self::from_usize($crate::align_down(self.0, align.into()))
            }

            /// Aligns the address upwards to the given alignment.
            #[inline]
            pub fn align_up<U>(self, align: U) -> Self
            where
                U: Into<usize>,
            {
                Self::from_usize($crate::align_up(self.0, align.into()))
            }

            /// Returns the offset of the address within the given alignment.
            #[inline]
            pub fn align_offset<U>(self, align: U) -> usize
            where
                U: Into<usize>,
            {
                $crate::align_offset(self.0, align.into())
            }

            /// Checks whether the address has the demanded alignment.
            #[inline]
            pub fn is_aligned<U>(self, align: U) -> bool
            where
                U: Into<usize>,
            {
                $crate::is_aligned(self.0, align.into())
            }

            /// Aligns the address downwards to 4096 (bytes).
            #[inline]
            pub const fn align_down_4k(self) -> Self {
                Self::from_usize($crate::align_down(self.0, $crate::PAGE_SIZE_4K))
            }

            /// Aligns the address upwards to 4096 (bytes).
            #[inline]
            pub const fn align_up_4k(self) -> Self {
                Self::from_usize($crate::align_up(self.0, $crate::PAGE_SIZE_4K))
            }

            /// Returns the offset of the address within a 4K-sized page.
            #[inline]
            pub const fn align_offset_4k(self) -> usize {
                $crate::align_offset(self.0, $crate::PAGE_SIZE_4K)
            }

            /// Checks whether the address is 4K-aligned.
            #[inline]
            pub const fn is_aligned_4k(self) -> bool {
                $crate::is_aligned(self.0, $crate::PAGE_SIZE_4K)
            }
        }

        impl From<usize> for $name {
            #[inline]
            fn from(addr: usize) -> Self {
                Self(addr)
            }
        }

        impl From<$name> for usize {
            #[inline]
            fn from(addr: $name) -> usize {
                addr.0
            }
        }

        impl core::ops::Add<usize> for $name {
            type Output = Self;
            #[inline]
            fn add(self, rhs: usize) -> Self {
                Self(self.0 + rhs)
            }
        }

        impl core::ops::AddAssign<usize> for $name {
            #[inline]
            fn add_assign(&mut self, rhs: usize) {
                self.0 += rhs;
            }
        }

        impl core::ops::Sub<usize> for $name {
            type Output = Self;
            #[inline]
            fn sub(self, rhs: usize) -> Self {
                Self(self.0 - rhs)
            }
        }

        impl core::ops::SubAssign<usize> for $name {
            #[inline]
            fn sub_assign(&mut self, rhs: usize) {
                self.0 -= rhs;
            }
        }

        $crate::def_usize_addr!($($tt)*);
    };
    () => {};
}

/// Creates implementations for the [`core::fmt::Debug`], [`core::fmt::LowerHex`], and
/// [`core::fmt::UpperHex`] traits for the given address types defined by the [`def_usize_addr`].
///
/// For each `$name = $format;`, this macro generates the following items:
/// - An implementation of [`core::fmt::Debug`] for the address type `$name`, which formats the
///   address with `format_args!($format, format_args!("{:#x}", self.0))`,
/// - An implementation of [`core::fmt::LowerHex`] for the address type `$name`, which formats the
///   address in the same way as [`core::fmt::Debug`],
/// - An implementation of [`core::fmt::UpperHex`] for the address type `$name`, which formats the
///   address with `format_args!($format, format_args!("{:#X}", self.0))`.
///
/// # Example
///
/// ```
/// use memory_addr::{PhysAddr, VirtAddr, def_usize_addr, def_usize_addr_formatter};
///
/// def_usize_addr! {
///     /// An example address type.
///     pub type ExampleAddr;
/// }
///
/// def_usize_addr_formatter! {
///     ExampleAddr = "EA:{}";
/// }
///
/// fn main() {
///     assert_eq!(format!("{:?}", PhysAddr::from(0x1abc)), "PA:0x1abc");
///     assert_eq!(format!("{:x}", VirtAddr::from(0x1abc)), "VA:0x1abc");
///     assert_eq!(format!("{:X}", ExampleAddr::from(0x1abc)), "EA:0x1ABC");
/// }
/// ```
#[macro_export]
macro_rules! def_usize_addr_formatter {
    (
        $name:ident = $format:literal;

        $($tt:tt)*
    ) => {
        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_fmt(format_args!($format, format_args!("{:#x}", self.0)))
            }
        }

        impl core::fmt::LowerHex for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_fmt(format_args!($format, format_args!("{:#x}", self.0)))
            }
        }

        impl core::fmt::UpperHex for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_fmt(format_args!($format, format_args!("{:#X}", self.0)))
            }
        }

        $crate::def_usize_addr_formatter!($($tt)*);
    };
    () => {};
}

def_usize_addr! {
    /// A physical memory address.
    pub type PhysAddr;

    /// A virtual memory address.
    pub type VirtAddr;
}

def_usize_addr_formatter! {
    PhysAddr = "PA:{}";
    VirtAddr = "VA:{}";
}

impl VirtAddr {
    /// Converts the virtual address to a raw pointer.
    #[inline]
    pub const fn as_ptr(self) -> *const u8 {
        self.0 as *const u8
    }

    /// Converts the virtual address to a mutable raw pointer.
    #[inline]
    pub const fn as_mut_ptr(self) -> *mut u8 {
        self.0 as *mut u8
    }
}

/// Alias for [`PhysAddr::from_usize`].
#[macro_export]
macro_rules! pa {
    ($addr:expr) => {
        $crate::PhysAddr::from_usize($addr)
    };
}

/// Alias for [`VirtAddr::from_usize`].
#[macro_export]
macro_rules! va {
    ($addr:expr) => {
        $crate::VirtAddr::from_usize($addr)
    };
}
