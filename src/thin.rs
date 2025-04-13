use core::{mem::MaybeUninit, ptr::NonNull};

use axerrno::AxResult;

use crate::{UserMutSlicePtr, UserSlicePtr};

/// A pointer to a value in the user-space virtual memory.
pub trait UserPtr: Copy {
    /// The type of data that the pointer points to.
    type Target;

    #[doc(hidden)]
    fn as_ptr(self) -> *const Self::Target;

    /// Returns a shared reference to the value with user space accessibility
    /// check. In contrast to [`UserPtr::as_ref_user`], this does not
    /// require that the value has to be initialized.
    ///
    /// Compared with [`as_uninit_ref`], this function does not check if the
    /// pointer is null. Null is treated as an error.
    ///
    /// [`as_uninit_ref`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.as_uninit_ref
    fn as_uninit_ref_user<'a>(self) -> AxResult<&'a MaybeUninit<Self::Target>> {
        let ptr = self.as_ptr();
        crate::check_access(ptr, false)?;
        // SAFETY: We have checked it.
        Ok(unsafe { &*ptr.cast() })
    }

    /// Returns a shared reference to the value with user space accessibility
    /// check. If the value may be uninitialized,
    /// [`UserPtr::as_uninit_ref_user`] must be used instead.
    ///
    /// Compared with [`as_ref`], this function does not check if the pointer is
    /// null. Null is treated as an error.
    ///
    /// # Safety
    /// The pointer must point to a [valid value] of type T.
    ///
    /// [`as_ref`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.as_ref
    /// [valid value]: https://doc.rust-lang.org/nightly/reference/behavior-considered-undefined.html#invalid-values
    unsafe fn as_ref_user<'a>(self) -> AxResult<&'a Self::Target> {
        let uninit = self.as_uninit_ref_user()?;
        // SAFETY: The caller guarantees that the memory is initialized.
        Ok(unsafe { uninit.assume_init_ref() })
    }

    /// A shortcut for [`core::ptr::slice_from_raw_parts`] and then
    /// [`UserSlicePtr::as_uninit_slice_user`].
    ///
    /// This function is equivalent to:
    /// ```ignore
    /// let ptr = core::ptr::slice_from_raw_parts(ptr, len);
    /// // Or:
    /// let ptr = NonNull::slice_from_raw_parts(ptr, len);
    /// let value = ptr.as_uninit_slice_user()?;
    /// ```
    fn as_uninit_slice_user<'a>(self, len: usize) -> AxResult<&'a [MaybeUninit<Self::Target>]> {
        let ptr = core::ptr::slice_from_raw_parts(self.as_ptr(), len);
        ptr.as_uninit_slice_user()
    }

    /// A shortcut for [`core::ptr::slice_from_raw_parts`] and then
    /// [`UserSlicePtr::as_slice_user`].
    ///
    /// This function is equivalent to:
    /// ```ignore
    /// let ptr = core::ptr::slice_from_raw_parts(ptr, len);
    /// // Or:
    /// let ptr = NonNull::slice_from_raw_parts(ptr, len);
    /// let value = ptr.as_slice_user()?;
    /// ```
    ///
    /// # Safety
    /// See [`UserSlicePtr::as_slice_user`].
    unsafe fn as_slice_user<'a>(self, len: usize) -> AxResult<&'a [Self::Target]> {
        let uninit = self.as_uninit_slice_user(len)?;
        // SAFETY: The caller guarantees that the memory is initialized.
        Ok(unsafe { uninit.assume_init_ref() })
    }
}

impl<T> UserPtr for *const T {
    type Target = T;

    fn as_ptr(self) -> *const T {
        self
    }
}

impl<T> UserPtr for *mut T {
    type Target = T;

    fn as_ptr(self) -> *const T {
        self
    }
}

impl<T> UserPtr for NonNull<T> {
    type Target = T;

    fn as_ptr(self) -> *const T {
        self.as_ptr()
    }
}

/// A pointer to a mutable value in the user-space virtual memory.
pub trait UserMutPtr: UserPtr {
    #[doc(hidden)]
    fn as_mut_ptr(self) -> *mut Self::Target {
        self.as_ptr().cast_mut()
    }

    /// Returns a mutable reference to the value with user space accessibility
    /// check. In contrast to [`UserPtrMut::as_mut_user`], this does not require
    /// that the value has to be initialized.
    ///
    /// Compared with [`as_uninit_mut`], this function does not check if the
    /// pointer is null. Null is treated as an error.
    ///
    /// [`as_uninit_mut`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.as_uninit_mut
    fn as_uninit_mut_user<'a>(self) -> AxResult<&'a mut MaybeUninit<Self::Target>> {
        let ptr = self.as_mut_ptr();
        crate::check_access(ptr, true)?;
        // SAFETY: We have checked it.
        Ok(unsafe { &mut *ptr.cast() })
    }

    /// Returns a mutable reference to the value with user space accessibility
    /// check. If the value may be uninitialized,
    /// [`UserPtrMut::as_uninit_mut_user`] must be used instead.
    ///
    /// Compared with [`as_mut`], this function does not check if the pointer is
    /// null. Null is treated as an error.
    ///
    /// # Safety
    /// The pointer must point to a [valid value] of type T.
    ///
    /// [`as_mut`]: https://doc.rust-lang.org/std/primitive.pointer.html#method.as_mut
    /// [valid value]: https://doc.rust-lang.org/nightly/reference/behavior-considered-undefined.html#invalid-values
    unsafe fn as_mut_user<'a>(self) -> AxResult<&'a mut Self::Target> {
        let uninit = self.as_uninit_mut_user()?;
        // SAFETY: The caller guarantees that the memory is initialized.
        Ok(unsafe { uninit.assume_init_mut() })
    }

    /// A shortcut for [`core::ptr::slice_from_raw_parts_mut`] and then
    /// [`UserMutSlicePtr::as_uninit_slice_mut_user`].
    ///
    /// This function is equivalent to:
    /// ```ignore
    /// let ptr = core::ptr::slice_from_raw_parts_mut(ptr, len);
    /// // Or:
    /// let ptr = NonNull::slice_from_raw_parts(ptr, len);
    /// let value = ptr.as_uninit_slice_mut_user()?;
    /// ```
    fn as_uninit_slice_mut_user<'a>(
        self,
        len: usize,
    ) -> AxResult<&'a mut [MaybeUninit<Self::Target>]> {
        let ptr = core::ptr::slice_from_raw_parts_mut(self.as_mut_ptr(), len);
        ptr.as_uninit_slice_mut_user()
    }

    /// A shortcut for [`core::ptr::slice_from_raw_parts_mut`] and then
    /// [`UserMutSlicePtr::as_mut_slice_user`].
    ///
    /// This function is equivalent to:
    /// ```ignore
    /// let ptr = core::ptr::slice_from_raw_parts_mut(ptr, len);
    /// // Or:
    /// let ptr = NonNull::slice_from_raw_parts(ptr, len);
    /// let value = ptr.as_mut_slice_user()?;
    /// ```
    ///
    /// # Safety
    /// See [`UserMutSlicePtr::as_mut_slice_user`].
    unsafe fn as_mut_slice_user<'a>(self, len: usize) -> AxResult<&'a mut [Self::Target]> {
        let uninit = self.as_uninit_slice_mut_user(len)?;
        // SAFETY: The caller guarantees that the memory is initialized.
        Ok(unsafe { uninit.assume_init_mut() })
    }
}

impl<T> UserMutPtr for *mut T {}

impl<T> UserMutPtr for NonNull<T> {}
