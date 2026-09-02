extern crate alloc;

use alloc::vec::Vec;

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
const LOAD_CHUNK_SIZE: usize = 32;

fn next_load_chunk(
    pointer: usize,
    loaded_elements: usize,
    element_size: usize,
) -> VmResult<(usize, usize)> {
    if element_size == 0 {
        return Err(VmError::TooLong);
    }
    let max_elements = MAX_BYTES / element_size;
    if loaded_elements >= max_elements {
        return Err(VmError::TooLong);
    }

    let loaded_bytes = loaded_elements
        .checked_mul(element_size)
        .ok_or(VmError::BadAddress)?;
    let start = pointer
        .checked_add(loaded_bytes)
        .ok_or(VmError::BadAddress)?;
    let bytes_to_boundary = LOAD_CHUNK_SIZE - start % LOAD_CHUNK_SIZE;
    let remaining_elements = max_elements - loaded_elements;
    let elements_to_boundary = bytes_to_boundary.div_ceil(element_size).max(1);
    let chunk_elements = elements_to_boundary.min(remaining_elements);
    let chunk_bytes = chunk_elements
        .checked_mul(element_size)
        .ok_or(VmError::BadAddress)?;
    start.checked_add(chunk_bytes).ok_or(VmError::BadAddress)?;
    Ok((start, chunk_elements))
}

/// Loads elements from the given pointer until a zero element is found.
pub fn vm_load_until_nul<T: Pod>(ptr: *const T) -> VmResult<Vec<T>> {
    if !ptr.is_aligned() {
        return Err(VmError::BadAddress);
    }

    let size = size_of::<T>();
    let mut result = Vec::new();
    let mut vm = VmImpl::new();

    loop {
        let (start, len) = next_load_chunk(ptr.addr(), result.len(), size)?;

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

    Ok(result)
}

#[cfg(test)]
fn vm_alloc_is_zero_and_max_bytes_rules_hold_for_test() -> bool {
    // is_zero: zero value returns true
    let zero_val: u64 = 0;
    assert!(is_zero(&zero_val));

    // is_zero: non-zero value returns false
    let nonzero_val: u64 = 42;
    assert!(!is_zero(&nonzero_val));

    // MAX_BYTES constant check
    assert!(MAX_BYTES == 131072);

    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn allocation_constants_and_zero_value_helpers_hold() {
        assert!(super::vm_alloc_is_zero_and_max_bytes_rules_hold_for_test());
    }

    #[test]
    fn load_chunk_rejects_address_overflow() {
        assert_eq!(
            super::next_load_chunk(usize::MAX, 0, 1),
            Err(crate::VmError::BadAddress)
        );
    }

    #[test]
    fn load_chunk_rejects_zero_sized_and_exhausted_inputs() {
        assert_eq!(
            super::next_load_chunk(0x1000, 0, 0),
            Err(crate::VmError::TooLong)
        );
        assert_eq!(
            super::next_load_chunk(0x1000, super::MAX_BYTES, 1),
            Err(crate::VmError::TooLong)
        );
    }

    #[test]
    fn load_chunk_reads_at_least_one_large_element() {
        assert_eq!(super::next_load_chunk(0x1001, 0, 64), Ok((0x1001, 1)));
    }
}
