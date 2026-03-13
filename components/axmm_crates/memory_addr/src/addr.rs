use core::cmp::Ord;

/// A trait for memory address types.
///
/// Memory address types here include both physical and virtual addresses, as
/// well as any other similar types like guest physical addresses in a
/// hypervisor.
///
/// This trait is automatically implemented for any type that is `Copy`,
/// `From<usize>`, `Into<usize>`, and `Ord`, providing a set of utility methods
/// for address alignment and arithmetic.
pub trait MemoryAddr:
    // The address type should be trivially copyable. This implies `Clone`.
    Copy
    // The address type should be convertible to and from `usize`.
    + From<usize>
    + Into<usize>
    // The address type should be comparable.
    + Ord
{
    // No required methods for now. Following are some utility methods.

    //
    // This section contains utility methods for address alignment.
    //

    /// Aligns the address downwards to the given alignment.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn align_down<U>(self, align: U) -> Self
    where
        U: Into<usize>,
    {
        Self::from(crate::align_down(self.into(), align.into()))
    }

    /// Aligns the address upwards to the given alignment.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn align_up<U>(self, align: U) -> Self
    where
        U: Into<usize>,
    {
        Self::from(crate::align_up(self.into(), align.into()))
    }

    /// Returns the offset of the address within the given alignment.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn align_offset<U>(self, align: U) -> usize
    where
        U: Into<usize>,
    {
        crate::align_offset(self.into(), align.into())
    }

    /// Checks whether the address has the demanded alignment.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn is_aligned<U>(self, align: U) -> bool
    where
        U: Into<usize>,
    {
        crate::is_aligned(self.into(), align.into())
    }

    /// Aligns the address downwards to 4096 (bytes).
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn align_down_4k(self) -> Self {
        Self::from(crate::align_down(self.into(), crate::PAGE_SIZE_4K))
    }

    /// Aligns the address upwards to 4096 (bytes).
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn align_up_4k(self) -> Self {
        Self::from(crate::align_up(self.into(), crate::PAGE_SIZE_4K))
    }

    /// Returns the offset of the address within a 4K-sized page.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn align_offset_4k(self) -> usize {
        crate::align_offset(self.into(), crate::PAGE_SIZE_4K)
    }

    /// Checks whether the address is 4K-aligned.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn is_aligned_4k(self) -> bool {
        crate::is_aligned(self.into(), crate::PAGE_SIZE_4K)
    }

    //
    // This section contains utility methods for address arithmetic.
    //

    /// Adds a given offset to the address to get a new address.
    /// 
    /// # Panics
    /// 
    /// Panics if the result overflows.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn offset(self, offset: isize) -> Self {
        // todo: use `strict_add_signed` when it's stable.
        Self::from(usize::checked_add_signed(self.into(), offset).expect("overflow in `MemoryAddr::offset`"))
    }

    /// Adds a given offset to the address to get a new address.
    /// 
    /// Unlike `offset`, this method always wraps around on overflow.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn wrapping_offset(self, offset: isize) -> Self {
        Self::from(usize::wrapping_add_signed(self.into(), offset))
    }

    /// Gets the distance between two addresses.
    /// 
    /// # Panics
    /// 
    /// Panics if the result is not representable by `isize`.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn offset_from(self, base: Self) -> isize {
        let result = usize::wrapping_sub(self.into(), base.into()) as isize;
        if (result > 0) ^ (base < self) {
            // The result has overflowed.
            panic!("overflow in `MemoryAddr::offset_from`");
        } else {
            result
        }
    }

    /// Adds a given **unsigned** offset to the address to get a new address.
    /// 
    /// This method is similar to `offset`, but it takes an unsigned offset.
    /// 
    /// # Panics
    /// 
    /// Panics if the result overflows.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn add(self, rhs: usize) -> Self {
        Self::from(usize::checked_add(self.into(), rhs).expect("overflow in `MemoryAddr::add`"))
    }

    /// Adds a given **unsigned** offset to the address to get a new address.
    /// 
    /// Unlike `add`, this method always wraps around on overflow.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn wrapping_add(self, rhs: usize) -> Self {
        Self::from(usize::wrapping_add(self.into(), rhs))
    }

    /// Adds a given **unsigned** offset to the address to get a new address.
    /// 
    /// Unlike `add`, this method returns a tuple of the new address and a boolean indicating
    /// whether the addition has overflowed.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn overflowing_add(self, rhs: usize) -> (Self, bool) {
        let (result, overflow) = self.into().overflowing_add(rhs);
        (Self::from(result), overflow)
    }

    /// Adds a given **unsigned** offset to the address to get a new address.
    /// 
    /// Unlike `add`, this method returns `None` on overflow.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn checked_add(self, rhs: usize) -> Option<Self> {
        usize::checked_add(self.into(), rhs).map(Self::from)
    }

    /// Subtracts a given **unsigned** offset from the address to get a new address.
    /// 
    /// This method is similar to `offset(-rhs)`, but it takes an unsigned offset. 
    /// 
    /// # Panics
    /// 
    /// Panics if the result overflows.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn sub(self, rhs: usize) -> Self {
        Self::from(usize::checked_sub(self.into(), rhs).expect("overflow in `MemoryAddr::sub`"))
    }

    /// Subtracts a given **unsigned** offset from the address to get a new address.
    /// 
    /// Unlike `sub`, this method always wraps around on overflowed.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn wrapping_sub(self, rhs: usize) -> Self {
        Self::from(usize::wrapping_sub(self.into(), rhs))
    }

    /// Subtracts a given **unsigned** offset from the address to get a new address.
    /// 
    /// Unlike `sub`, this method returns a tuple of the new address and a boolean indicating
    /// whether the subtraction has overflowed.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn overflowing_sub(self, rhs: usize) -> (Self, bool) {
        let (result, overflow) = self.into().overflowing_sub(rhs);
        (Self::from(result), overflow)
    }

    /// Subtracts a given **unsigned** offset from the address to get a new address.
    /// 
    /// Unlike `sub`, this method returns `None` on overflow.
    #[inline]
    #[must_use = "this returns a new address, without modifying the original"]
    fn checked_sub(self, rhs: usize) -> Option<Self> {
        usize::checked_sub(self.into(), rhs).map(Self::from)
    }

    /// Subtracts another address from the address to get the offset between them.
    /// 
    /// # Panics
    /// 
    /// Panics if the result overflows.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn sub_addr(self, rhs: Self) -> usize {
        usize::checked_sub(self.into(), rhs.into()).expect("overflow in `MemoryAddr::sub_addr`")
    }

    /// Subtracts another address from the address to get the offset between them.
    /// 
    /// Unlike `sub_addr`, this method always wraps around on overflow.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn wrapping_sub_addr(self, rhs: Self) -> usize {
        usize::wrapping_sub(self.into(), rhs.into())
    }

    /// Subtracts another address from the address to get the offset between them.
    /// 
    /// Unlike `sub_addr`, this method returns a tuple of the offset and a boolean indicating
    /// whether the subtraction has overflowed.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn overflowing_sub_addr(self, rhs: Self) -> (usize, bool) {
        usize::overflowing_sub(self.into(), rhs.into())
    }

    /// Subtracts another address from the address to get the offset between them.
    /// 
    /// Unlike `sub_addr`, this method returns `None` on overflow.
    #[inline]
    #[must_use = "this function has no side effects, so it can be removed if the return value is not used"]
    fn checked_sub_addr(self, rhs: Self) -> Option<usize> {
        usize::checked_sub(self.into(), rhs.into())
    }
}

/// Implement the `MemoryAddr` trait for any type that is `Copy`, `From<usize>`,
/// `Into<usize>`, and `Ord`.
impl<T> MemoryAddr for T where T: Copy + From<usize> + Into<usize> + Ord {}

/// Creates a new address type by wrapping an `usize`.
///
/// For each `$vis type $name;`, this macro generates the following items:
/// - Definition of the new address type `$name`, which contains a single
///   private unnamed field of type `usize`.
/// - Default implementations (i.e. derived implementations) for the following
///   traits:
///   - `Copy`, `Clone`,
///   - `Default`,
///   - `Ord`, `PartialOrd`, `Eq`, and `PartialEq`.
/// - Implementations for the following traits:
///   - `From<usize>`, `Into<usize>` (by implementing `From<$name> for usize`),
///   - `Add<usize>`, `AddAssign<usize>`, `Sub<usize>`, `SubAssign<usize>`, and
///   - `Sub<$name>`.
/// - Two `const` methods to convert between the address type and `usize`:
///   - `from_usize`, which converts an `usize` to the address type, and
///   - `as_usize`, which converts the address type to an `usize`.
///
/// # Example
///
/// ```
/// use memory_addr::{def_usize_addr, MemoryAddr};
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

        impl core::ops::Sub<$name> for $name {
            type Output = usize;
            #[inline]
            fn sub(self, rhs: $name) -> usize {
                self.0 - rhs.0
            }
        }

        $crate::def_usize_addr!($($tt)*);
    };
    () => {};
}

/// Creates implementations for the [`Debug`](core::fmt::Debug),
/// [`LowerHex`](core::fmt::LowerHex), and [`UpperHex`](core::fmt::UpperHex)
/// traits for the given address types defined by the [`def_usize_addr`].
///
/// For each `$name = $format;`, this macro generates the following items:
/// - An implementation of [`core::fmt::Debug`] for the address type `$name`,
///   which formats the address with `format_args!($format,
///   format_args!("{:#x}", self.0))`,
/// - An implementation of [`core::fmt::LowerHex`] for the address type `$name`,
///   which formats the address in the same way as [`core::fmt::Debug`],
/// - An implementation of [`core::fmt::UpperHex`] for the address type `$name`,
///   which formats the address with `format_args!($format,
///   format_args!("{:#X}", self.0))`.
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
/// # fn main() {
/// assert_eq!(format!("{:?}", PhysAddr::from(0x1abc)), "PA:0x1abc");
/// assert_eq!(format!("{:x}", VirtAddr::from(0x1abc)), "VA:0x1abc");
/// assert_eq!(format!("{:X}", ExampleAddr::from(0x1abc)), "EA:0x1ABC");
/// # }
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
    /// Creates a new virtual address from a raw pointer.
    #[inline]
    pub fn from_ptr_of<T>(ptr: *const T) -> Self {
        Self(ptr as usize)
    }

    /// Creates a new virtual address from a mutable raw pointer.
    #[inline]
    pub fn from_mut_ptr_of<T>(ptr: *mut T) -> Self {
        Self(ptr as usize)
    }

    /// Converts the virtual address to a raw pointer.
    #[inline]
    pub const fn as_ptr(self) -> *const u8 {
        self.0 as *const u8
    }

    /// Converts the virtual address to a raw pointer of a specific type.
    #[inline]
    pub const fn as_ptr_of<T>(self) -> *const T {
        self.0 as *const T
    }

    /// Converts the virtual address to a mutable raw pointer.
    #[inline]
    pub const fn as_mut_ptr(self) -> *mut u8 {
        self.0 as *mut u8
    }

    /// Converts the virtual address to a mutable raw pointer of a specific
    /// type.
    #[inline]
    pub const fn as_mut_ptr_of<T>(self) -> *mut T {
        self.0 as *mut T
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

#[cfg(test)]
mod test {
    use core::mem::size_of;

    use super::*;

    def_usize_addr! {
        /// An example address type.
        pub type ExampleAddr;
        /// Another example address type.
        pub type AnotherAddr;
    }

    def_usize_addr_formatter! {
        ExampleAddr = "EA:{}";
        AnotherAddr = "AA:{}";
    }

    #[test]
    fn test_addr() {
        let addr = va!(0x2000);
        assert!(addr.is_aligned_4k());
        assert!(!addr.is_aligned(0x10000usize));
        assert_eq!(addr.align_offset_4k(), 0);
        assert_eq!(addr.align_down_4k(), va!(0x2000));
        assert_eq!(addr.align_up_4k(), va!(0x2000));

        let addr = va!(0x2fff);
        assert!(!addr.is_aligned_4k());
        assert_eq!(addr.align_offset_4k(), 0xfff);
        assert_eq!(addr.align_down_4k(), va!(0x2000));
        assert_eq!(addr.align_up_4k(), va!(0x3000));

        let align = 0x100000;
        let addr = va!(align * 5) + 0x2000;
        assert!(addr.is_aligned_4k());
        assert!(!addr.is_aligned(align));
        assert_eq!(addr.align_offset(align), 0x2000);
        assert_eq!(addr.align_down(align), va!(align * 5));
        assert_eq!(addr.align_up(align), va!(align * 6));
    }

    #[test]
    pub fn test_addr_convert_and_comparison() {
        let example1 = ExampleAddr::from_usize(0x1234);
        let example2 = ExampleAddr::from(0x5678);
        let another1 = AnotherAddr::from_usize(0x9abc);
        let another2 = AnotherAddr::from(0xdef0);

        assert_eq!(example1.as_usize(), 0x1234);
        assert_eq!(Into::<usize>::into(example2), 0x5678);
        assert_eq!(Into::<usize>::into(another1), 0x9abc);
        assert_eq!(another2.as_usize(), 0xdef0);

        assert_eq!(example1, ExampleAddr::from(0x1234));
        assert_eq!(example2, ExampleAddr::from_usize(0x5678));
        assert_eq!(another1, AnotherAddr::from_usize(0x9abc));
        assert_eq!(another2, AnotherAddr::from(0xdef0));

        assert!(example1 < example2);
        assert!(example1 <= example2);
        assert!(example2 > example1);
        assert!(example2 >= example1);
        assert!(example1 != example2);
    }

    #[test]
    pub fn test_addr_fmt() {
        assert_eq!(format!("{:?}", ExampleAddr::from(0x1abc)), "EA:0x1abc");
        assert_eq!(format!("{:x}", AnotherAddr::from(0x1abc)), "AA:0x1abc");
        assert_eq!(format!("{:X}", ExampleAddr::from(0x1abc)), "EA:0x1ABC");
    }

    #[test]
    pub fn test_alignment() {
        let alignment = 0x1000usize;
        let base = alignment * 2;
        let offset = 0x123usize;
        let addr = ExampleAddr::from_usize(base + offset);

        assert_eq!(addr.align_down(alignment), ExampleAddr::from_usize(base));
        assert_eq!(
            addr.align_up(alignment),
            ExampleAddr::from_usize(base + alignment)
        );
        assert_eq!(addr.align_offset(alignment), offset);
        assert!(!addr.is_aligned(alignment));
        assert!(ExampleAddr::from_usize(base).is_aligned(alignment));
        assert_eq!(
            ExampleAddr::from_usize(base).align_up(alignment),
            ExampleAddr::from_usize(base)
        );
    }

    #[test]
    pub fn test_addr_arithmetic() {
        let base = 0x1234usize;
        let offset = 0x100usize;
        let with_offset = base + offset;

        let addr = ExampleAddr::from_usize(base);
        let offset_addr = ExampleAddr::from_usize(with_offset);

        assert_eq!(addr.offset(offset as isize), offset_addr);
        assert_eq!(addr.wrapping_offset(offset as isize), offset_addr);
        assert_eq!(offset_addr.offset_from(addr), offset as isize);
        assert_eq!(addr.add(offset), offset_addr);
        assert_eq!(addr.wrapping_add(offset), offset_addr);
        assert_eq!(offset_addr.sub(offset), addr);
        assert_eq!(offset_addr.wrapping_sub(offset), addr);
        assert_eq!(offset_addr.sub_addr(addr), offset);
        assert_eq!(offset_addr.wrapping_sub_addr(addr), offset);

        assert_eq!(addr + offset, offset_addr);
        assert_eq!(offset_addr - offset, addr);
        assert_eq!(offset_addr - addr, offset);
    }

    #[test]
    pub fn test_addr_wrapping_arithmetic() {
        let base = usize::MAX - 0x100usize;
        let offset = 0x200usize;
        let with_offset = base.wrapping_add(offset);

        let addr = ExampleAddr::from_usize(base);
        let offset_addr = ExampleAddr::from_usize(with_offset);

        assert_eq!(addr.wrapping_offset(offset as isize), offset_addr);
        assert_eq!(offset_addr.wrapping_offset(-(offset as isize)), addr);
        assert_eq!(addr.wrapping_add(offset), offset_addr);
        assert_eq!(offset_addr.wrapping_sub(offset), addr);
        assert_eq!(offset_addr.wrapping_sub_addr(addr), offset);
    }

    #[test]
    pub fn test_addr_checked_arithmetic() {
        let low_addr = ExampleAddr::from_usize(0x100usize);
        let high_addr = ExampleAddr::from_usize(usize::MAX - 0x100usize);
        let small_offset = 0x50usize;
        let large_offset = 0x200usize;

        assert_eq!(
            low_addr.checked_sub(small_offset),
            Some(low_addr.wrapping_sub(small_offset))
        );
        assert_eq!(low_addr.checked_sub(large_offset), None);
        assert_eq!(
            high_addr.checked_add(small_offset),
            Some(high_addr.wrapping_add(small_offset))
        );
        assert_eq!(high_addr.checked_add(large_offset), None);

        assert_eq!(
            high_addr.checked_sub_addr(low_addr),
            Some(usize::MAX - 0x200usize)
        );
        assert_eq!(low_addr.checked_sub_addr(high_addr), None);
    }

    #[test]
    pub fn test_addr_overflowing_arithmetic() {
        let low_addr = ExampleAddr::from_usize(0x100usize);
        let high_addr = ExampleAddr::from_usize(usize::MAX - 0x100usize);
        let small_offset = 0x50usize;
        let large_offset = 0x200usize;

        assert_eq!(
            low_addr.overflowing_sub(small_offset),
            (low_addr.wrapping_sub(small_offset), false)
        );
        assert_eq!(
            low_addr.overflowing_sub(large_offset),
            (low_addr.wrapping_sub(large_offset), true)
        );
        assert_eq!(
            high_addr.overflowing_add(small_offset),
            (high_addr.wrapping_add(small_offset), false)
        );
        assert_eq!(
            high_addr.overflowing_add(large_offset),
            (high_addr.wrapping_add(large_offset), true)
        );
        assert_eq!(
            high_addr.overflowing_sub_addr(low_addr),
            (high_addr.wrapping_sub_addr(low_addr), false)
        );
        assert_eq!(
            low_addr.overflowing_sub_addr(high_addr),
            (low_addr.wrapping_sub_addr(high_addr), true)
        );
    }

    #[test]
    #[should_panic]
    pub fn test_addr_offset_overflow() {
        let addr = ExampleAddr::from_usize(usize::MAX);
        let _ = addr.offset(1);
    }

    #[test]
    #[should_panic]
    pub fn test_addr_offset_from_overflow() {
        let addr = ExampleAddr::from_usize(usize::MAX);
        let _ = addr.offset_from(ExampleAddr::from_usize(0));
    }

    #[test]
    #[should_panic]
    pub fn test_addr_offset_from_underflow() {
        let addr = ExampleAddr::from_usize(0);
        let _ = addr.offset_from(ExampleAddr::from_usize(usize::MAX));
    }

    #[test]
    #[should_panic]
    pub fn test_addr_add_overflow() {
        let addr = ExampleAddr::from_usize(usize::MAX);
        let _ = addr.add(1);
    }

    #[test]
    #[should_panic]
    pub fn test_addr_sub_underflow() {
        let addr = ExampleAddr::from_usize(0);
        let _ = addr.sub(1);
    }

    #[test]
    #[should_panic]
    pub fn test_addr_sub_addr_overflow() {
        let addr = ExampleAddr::from_usize(0);
        let _ = addr.sub_addr(ExampleAddr::from_usize(1));
    }

    #[test]
    pub fn test_virt_addr_ptr() {
        let a: [usize; 4] = [0x1234, 0x5678, 0x9abc, 0xdef0];

        let va0 = VirtAddr::from_ptr_of(&a as *const usize);
        let va1 = va0.add(size_of::<usize>());
        let va2 = va1.add(size_of::<usize>());
        let va3 = va2.add(size_of::<usize>());

        let p0 = va0.as_ptr() as *const usize;
        let p1 = va1.as_ptr_of::<usize>();
        let p2 = va2.as_mut_ptr() as *mut usize;
        let p3 = va3.as_mut_ptr_of::<usize>();

        // testing conversion back to virt addr
        assert_eq!(va0, VirtAddr::from_ptr_of(p0));
        assert_eq!(va1, VirtAddr::from_ptr_of(p1));
        assert_eq!(va2, VirtAddr::from_mut_ptr_of(p2));
        assert_eq!(va3, VirtAddr::from_mut_ptr_of(p3));

        // testing pointer read/write
        assert!(unsafe { *p0 } == a[0]);
        assert!(unsafe { *p1 } == a[1]);
        assert!(unsafe { *p2 } == a[2]);
        assert!(unsafe { *p3 } == a[3]);

        unsafe {
            *p2 = 0xdeadbeef;
        }
        unsafe {
            *p3 = 0xcafebabe;
        }
        assert_eq!(a[2], 0xdeadbeef);
        assert_eq!(a[3], 0xcafebabe);
    }
}
