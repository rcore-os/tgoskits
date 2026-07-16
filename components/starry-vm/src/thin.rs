use core::{
    mem::{MaybeUninit, size_of},
    ptr::NonNull,
    slice,
};

use bytemuck::{AnyBitPattern, NoUninit};

use crate::{VmIo, VmResult, vm_read_slice, vm_write_slice};

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
    fn vm_write(self, value: Self::Target) -> VmResult
    where
        Self::Target: NoUninit,
    {
        vm_write_slice(self.as_ptr().cast_mut(), slice::from_ref(&value))
    }

    /// Overwrites a virtual-memory location from an ABI value whose object
    /// representation is guaranteed by the caller.
    ///
    /// Prefer [`VmMutPtr::vm_write`]. This escape hatch exists for private ABI
    /// frames that cannot implement [`NoUninit`] because they contain foreign
    /// types, while still making the padding-initialization proof explicit.
    ///
    /// # Safety
    ///
    /// Every byte in `value`, including padding, must be initialized. The
    /// pointed-to virtual range must accept a `size_of::<Self::Target>()`
    /// write.
    unsafe fn vm_write_abi(self, value: &Self::Target) -> VmResult {
        // SAFETY: the caller guarantees the complete object representation is
        // initialized and `value` remains borrowed for the duration of `write`.
        let bytes = unsafe {
            slice::from_raw_parts(
                (value as *const Self::Target).cast::<u8>(),
                size_of::<Self::Target>(),
            )
        };
        crate::VmImpl::new().write(self.as_ptr().addr(), bytes)
    }
}

impl<T> VmMutPtr for *mut T {}

impl<T> VmMutPtr for NonNull<T> {}
