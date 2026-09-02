use core::{mem::MaybeUninit, ptr::NonNull, slice};

use bytemuck::AnyBitPattern;

use crate::{VmResult, vm_read_slice, vm_write_slice};

/// A virtual memory pointer.
pub trait VmPtr: Copy {
    /// The type of data that the pointer points to.
    type Target;

    #[doc(hidden)]
    fn as_ptr(self) -> *const Self::Target;

    /// Returns `None` if the pointer is null, otherwise returns `Some(self)`.
    fn nullable(self) -> Option<Self> {
        if self.as_ptr().is_null() {
            None
        } else {
            Some(self)
        }
    }

    /// Reads the value from this virtual memory pointer. In contrast to
    /// [`VmPtr::vm_read`], this does not require that the value has to be
    /// initialized.
    fn vm_read_uninit(self) -> VmResult<MaybeUninit<Self::Target>> {
        let mut uninit = MaybeUninit::<Self::Target>::uninit();
        vm_read_slice(self.as_ptr(), slice::from_mut(&mut uninit))?;
        Ok(uninit)
    }

    /// Reads the value from this virtual memory pointer.
    fn vm_read(self) -> VmResult<Self::Target>
    where
        Self::Target: AnyBitPattern,
    {
        let uninit = self.vm_read_uninit()?;
        // SAFETY: `AnyBitPattern`
        Ok(unsafe { uninit.assume_init() })
    }

    /// Reads a value whose validity cannot be expressed with
    /// [`AnyBitPattern`].
    ///
    /// # Safety
    ///
    /// Every possible bit pattern copied from the virtual address must be a
    /// valid value of `Self::Target`. This is suitable for C ABI records made
    /// only of integers and raw pointers, but not for references, enums,
    /// booleans, or other types with invalid representations.
    unsafe fn vm_read_any(self) -> VmResult<Self::Target> {
        let uninit = self.vm_read_uninit()?;
        // SAFETY: guaranteed by the caller's all-bit-patterns-valid contract.
        Ok(unsafe { uninit.assume_init() })
    }
}

impl<T> VmPtr for *const T {
    type Target = T;

    fn as_ptr(self) -> *const T {
        self
    }
}

impl<T> VmPtr for *mut T {
    type Target = T;

    fn as_ptr(self) -> *const T {
        self
    }
}

impl<T> VmPtr for NonNull<T> {
    type Target = T;

    fn as_ptr(self) -> *const T {
        self.as_ptr()
    }
}

/// A mutable virtual memory pointer.
pub trait VmMutPtr: VmPtr {
    /// Overwrites a virtual memory location with the given value.
    fn vm_write(self, value: Self::Target) -> VmResult {
        vm_write_slice(self.as_ptr().cast_mut(), slice::from_ref(&value))
    }
}

impl<T> VmMutPtr for *mut T {}

impl<T> VmMutPtr for NonNull<T> {}
