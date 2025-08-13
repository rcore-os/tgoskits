extern crate alloc;

use alloc::{ffi::CString, vec::Vec};

use bytemuck::{AnyBitPattern, Pod, bytes_of, zeroed};

use crate::{VmError, VmImpl, VmIo, VmResult, vm_read_slice};

/// Loads a vector of elements from the virtual memory.
///
/// # Safety
///
/// The caller must ensure the memory pointed to by `ptr` is valid and
/// initialized.
pub unsafe fn vm_load_any<T>(ptr: *const T, len: usize) -> VmResult<Vec<T>> {
    let mut buf = Vec::with_capacity(len);
    vm_read_slice(ptr, &mut buf.spare_capacity_mut()[..len])?;
    // SAFETY: The caller guarantees that the memory is valid and initialized.
    unsafe { buf.set_len(len) }
    Ok(buf)
}

/// Loads a vector of elements from the virtual memory.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn vm_load<T: AnyBitPattern>(ptr: *const T, len: usize) -> VmResult<Vec<T>> {
    // SAFETY: `AnyBitPattern`
    unsafe { vm_load_any(ptr, len) }
}

#[inline]
fn is_zero<T: Pod>(value: &T) -> bool {
    bytes_of(value) == bytes_of(&zeroed::<T>())
}

const MAX_BYTES: usize = 131072;

/// Loads elements from the given pointer until a zero element is found.
pub fn vm_load_until_nul<T: Pod>(ptr: *const T) -> VmResult<Vec<T>> {
    if !ptr.is_aligned() {
        return Err(VmError::BadAddress);
    }

    let size = size_of::<T>();
    let mut result = Vec::new();
    let mut vm = VmImpl::new();

    loop {
        const CHUNK_SIZE: usize = 4096; // 4 KiB

        let start = ptr.addr() + result.len() * size;
        let end = (start + 1).next_multiple_of(CHUNK_SIZE);
        let len = (end - start) / size;

        result.reserve(len);
        let buf = &mut result.spare_capacity_mut()[..len];
        vm.read(start, buf.as_bytes_mut())?;

        // SAFETY: `Pod`
        let buf = unsafe { buf.assume_init_ref() };
        let pos = buf.iter().position(is_zero);

        unsafe { result.set_len(result.len() + pos.unwrap_or(len)) };
        if result.len() >= MAX_BYTES / size {
            return Err(VmError::TooLong);
        }

        if pos.is_some() {
            break;
        }
    }

    result.shrink_to_fit();
    Ok(result)
}

/// Loads a null-terminated C string from the virtual memory.
pub fn vm_load_c_string(ptr: *const u8) -> VmResult<CString> {
    let bytes = vm_load_until_nul(ptr)?;
    // SAFETY: vm_load_until_nul guarantees no interior 0 byte.
    Ok(unsafe { CString::from_vec_unchecked(bytes) })
}
