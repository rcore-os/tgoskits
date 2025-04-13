use core::{
    ffi::{CStr, c_char},
    mem::MaybeUninit,
    ptr::NonNull,
    slice,
};

use axerrno::{AxError, AxResult};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange};

use crate::{Guard, UserPtr};

pub trait Zeroable: Copy + Eq {
    const SIZE: usize = size_of::<Self>();
    const ZERO: Self = unsafe { core::mem::zeroed() };
}

impl<T: ?Sized> Zeroable for *const T {}
impl<T: ?Sized> Zeroable for *mut T {}
impl<T: ?Sized> Zeroable for NonNull<T> {}

impl Zeroable for u8 {}
impl Zeroable for u16 {}
impl Zeroable for u32 {}
impl Zeroable for u64 {}
impl Zeroable for u128 {}
impl Zeroable for usize {}
impl Zeroable for i8 {}
impl Zeroable for i16 {}
impl Zeroable for i32 {}
impl Zeroable for i64 {}
impl Zeroable for i128 {}
impl Zeroable for isize {}

/// A pointer to a slice in the user-space virtual memory.
///
/// It's different from [`UserPtr`] since it should be a fat pointer containing
/// the length of the slice.
///
/// [`UserPtr`]: crate::UserPtr
pub trait UserSlicePtr: Copy {
    /// The type of data that the slice contains.
    type Target;

    /// Returns a shared slice to the value with user space accessibility check.
    /// In contrast to [`UserSlicePtr::as_slice_user`], this does not require
    /// that the values have to be initialized.
    fn as_uninit_slice_user<'a>(self) -> AxResult<&'a [MaybeUninit<Self::Target>]>;

    /// Returns a shared slice to the value with user space accessibility check.
    /// If the values may be uninitialized,
    /// [`UserSlicePtr::as_uninit_slice_user`] must be used instead.
    ///
    /// # Safety
    /// Calling this when the content is not yet fully initialized causes
    /// undefined behavior: it is up to the caller to guarantee that every
    /// `MaybeUninit<T>` in the slice really is in an initialized state.
    unsafe fn as_slice_user<'a>(self) -> AxResult<&'a [Self::Target]> {
        let uninit = self.as_uninit_slice_user()?;
        // SAFETY: The caller guarantees that the memory is initialized.
        Ok(unsafe { uninit.assume_init_ref() })
    }
}

impl<T> UserSlicePtr for *const [T] {
    type Target = T;

    fn as_uninit_slice_user<'a>(self) -> AxResult<&'a [MaybeUninit<T>]> {
        crate::check_access(self, false)?;
        // SAFETY: We have checked it.
        Ok(unsafe { slice::from_raw_parts(self as *const MaybeUninit<T>, self.len()) })
    }
}

impl<T> UserSlicePtr for *mut [T] {
    type Target = T;

    fn as_uninit_slice_user<'a>(self) -> AxResult<&'a [MaybeUninit<T>]> {
        crate::check_access(self, false)?;
        // SAFETY: We have checked it.
        Ok(unsafe { slice::from_raw_parts(self as *const MaybeUninit<T>, self.len()) })
    }
}

impl<T> UserSlicePtr for NonNull<[T]> {
    type Target = T;

    fn as_uninit_slice_user<'a>(self) -> AxResult<&'a [MaybeUninit<T>]> {
        crate::check_access(self.as_ptr(), false)?;
        // SAFETY: We have checked it.
        Ok(unsafe { slice::from_raw_parts(self.as_ptr() as *const MaybeUninit<T>, self.len()) })
    }
}

/// A pointer to a mutable slice in the user-space virtual memory.
pub trait UserMutSlicePtr: UserSlicePtr {
    /// Returns a mutable slice to the value with user space accessibility
    /// check. In contrast to [`UserMutSlicePtr::as_mut_slice_user`], this
    /// does not require that the values have to be initialized.
    fn as_uninit_slice_mut_user<'a>(self) -> AxResult<&'a mut [MaybeUninit<Self::Target>]>;

    /// Returns a mutable slice to the value with user space accessibility
    /// check. If the values may be uninitialized,
    /// [`UserMutSlicePtr::as_uninit_slice_mut_user`] must be used instead.
    ///
    /// # Safety
    /// Calling this when the content is not yet fully initialized causes
    /// undefined behavior: it is up to the caller to guarantee that every
    /// `MaybeUninit<T>` in the slice really is in an initialized state.
    unsafe fn as_mut_slice_user<'a>(self) -> AxResult<&'a mut [Self::Target]> {
        let uninit = self.as_uninit_slice_mut_user()?;
        // SAFETY: The caller guarantees that the memory is initialized.
        Ok(unsafe { uninit.assume_init_mut() })
    }
}

impl<T> UserMutSlicePtr for *mut [T] {
    fn as_uninit_slice_mut_user<'a>(self) -> AxResult<&'a mut [MaybeUninit<T>]> {
        crate::check_access(self, true)?;
        // SAFETY: We have checked it.
        Ok(unsafe { slice::from_raw_parts_mut(self as *mut MaybeUninit<T>, self.len()) })
    }
}

impl<T> UserMutSlicePtr for NonNull<[T]> {
    fn as_uninit_slice_mut_user<'a>(self) -> AxResult<&'a mut [MaybeUninit<T>]> {
        crate::check_access(self.as_ptr(), true)?;
        // SAFETY: We have checked it.
        Ok(unsafe { slice::from_raw_parts_mut(self.as_ptr() as *mut MaybeUninit<T>, self.len()) })
    }
}

// Quick zero byte pattern searching inspired by:
// https://doc.rust-lang.org/src/core/slice/memchr.rs.html#19

#[inline]
const fn splat_u8(x: u8) -> usize {
    x as usize * (usize::MAX / u8::MAX as usize)
}

#[inline]
const fn contains_0u8(x: usize) -> bool {
    const LO: usize = splat_u8(0x01);
    const HI: usize = splat_u8(0x80);
    x.wrapping_sub(LO) & !x & HI != 0
}

#[inline]
const fn splat_u16(x: u16) -> usize {
    x as usize * (usize::MAX / u16::MAX as usize)
}

#[inline]
const fn contains_0u16(x: usize) -> bool {
    const LO: usize = splat_u16(0x0001);
    const HI: usize = splat_u16(0x8000);
    x.wrapping_sub(LO) & !x & HI != 0
}

#[inline]
const fn splat_u32(x: u32) -> usize {
    x as usize * (usize::MAX / u32::MAX as usize)
}

#[inline]
const fn contains_0u32(x: usize) -> bool {
    const LO: usize = splat_u32(0x0000_0001);
    const HI: usize = splat_u32(0x8000_0000);
    x.wrapping_sub(LO) & !x & HI != 0
}

#[inline]
fn search_naive<T: Zeroable>(bytes: &[T]) -> Option<usize> {
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == T::ZERO {
            return Some(i);
        }

        i += 1;
    }

    None
}

const USIZE_BYTES: usize = size_of::<usize>();

fn len_until_nul<T: Zeroable>(start: *const T, write: bool) -> AxResult<usize> {
    if !start.is_aligned() {
        return Err(AxError::BadAddress);
    }

    let mut ptr = start;

    // before we start, align ptr to at least usize
    let offset = ptr.align_offset(USIZE_BYTES);
    if offset != 0 {
        let len = USIZE_BYTES - offset;
        let bytes = unsafe { ptr.as_slice_user(len)? };
        if let Some(i) = search_naive(bytes) {
            return Ok(i);
        }
        ptr = ptr.wrapping_byte_add(len);
    }

    let guard = Guard::new();
    loop {
        let page_start = VirtAddr::from_ptr_of(ptr).align_down_4k();
        guard.access_range(
            VirtAddrRange::from_start_size(page_start, PAGE_SIZE_4K),
            write,
        )?;
        let page_end = (page_start + PAGE_SIZE_4K).as_ptr_of();

        if T::SIZE < USIZE_BYTES {
            while ptr < page_end {
                let x = unsafe { *(ptr as *const usize) };
                if T::SIZE == 1 && contains_0u8(x) {
                    break;
                }
                if T::SIZE == 2 && contains_0u16(x) {
                    break;
                }
                if T::SIZE == 4 && contains_0u32(x) {
                    break;
                }
                ptr = ptr.wrapping_byte_add(USIZE_BYTES);
            }
        }

        let bytes = unsafe { slice::from_raw_parts(ptr, page_end.sub_ptr(ptr)) };
        if let Some(i) = search_naive(bytes) {
            ptr = ptr.wrapping_byte_add(i * T::SIZE);
            break;
        }

        ptr = page_end;
    }

    Ok((ptr.addr() - start.addr()) / T::SIZE)
}

/// Forms a slice from a pointer until the first null byte.
///
/// # Safety
/// * The memory pointed to by `start` must contain a valid nul terminator at
///   the end of the slice.
/// * The entire memory range of this slice must be contained within a single
///   allocated object.
/// * The memory referenced by the returned slice must not be mutated for the
///   duration of lifetime `'a`.
/// * The nul terminator must be within `isize::MAX` from `start`.
pub unsafe fn slice_until_nul<'a, T: Zeroable>(start: *const T) -> AxResult<&'a [T]> {
    let len = len_until_nul(start, false)?;
    Ok(unsafe { slice::from_raw_parts(start, len) })
}

/// Performs the same functionality as [`slice_until_nul`], except that a
/// mutable slice is returned.
///
/// # Safety
/// * The memory pointed to by `start` must contain a valid nul terminator at
///   the end of the slice.
/// * The entire memory range of this slice must be contained within a single
///   allocated object.
/// * The memory referenced by the returned slice must not be accessed through
///   any other pointer (not derived from the return value) for the duration of
///   lifetime `'a`. Both read and write accesses are forbidden.
/// * The nul terminator must be within `isize::MAX` from `start`.
pub unsafe fn slice_until_nul_mut<'a, T: Zeroable>(start: *mut T) -> AxResult<&'a mut [T]> {
    let len = len_until_nul(start, true)?;
    Ok(unsafe { slice::from_raw_parts_mut(start, len) })
}

/// Forms a C string from a raw C string pointer until the first null byte.
/// This is similar to [`CStr::from_ptr`], but with user space accessibility
/// check.
///
/// # Safety
/// * The memory pointed to by `start` must contain a valid nul terminator at
///   the end of the slice.
/// * The entire memory range of this slice must be contained within a single
///   allocated object.
/// * The memory referenced by the returned slice must not be mutated for the
///   duration of lifetime `'a`.
/// * The nul terminator must be within `isize::MAX` from `start`.
pub unsafe fn cstr_until_nul<'a>(start: *const c_char) -> AxResult<&'a CStr> {
    let len = len_until_nul(start, false)?;
    unsafe {
        Ok(CStr::from_bytes_with_nul_unchecked(slice::from_raw_parts(
            start.cast(),
            len + 1,
        )))
    }
}

#[test]
fn test_contains_zero() {
    assert!(!contains_0u8(usize::MAX));
    assert!(contains_0u8(0));
    assert!(!contains_0u8(splat_u8(0xf0)));
    assert!(contains_0u8(splat_u16(0xff00)));

    assert!(!contains_0u16(usize::MAX));
    assert!(contains_0u16(0));
    assert!(!contains_0u16(splat_u16(0xf000)));
    assert!(!contains_0u16(splat_u32(0xff00_00ff)));
    assert!(contains_0u16(splat_u32(0xffff_0000)));

    assert!(!contains_0u32(usize::MAX));
    assert!(contains_0u32(0));
    assert!(!contains_0u32(0xff00_0000_0000_00ff));
    assert!(contains_0u32(0xffff_0000_0000_0000));
}
