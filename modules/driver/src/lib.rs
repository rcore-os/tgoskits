#![no_std]

extern crate axklib;

use core::ptr::NonNull;

use rdrive::probe::OnProbeError;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[allow(unused_imports)]
#[macro_use]
extern crate log;

mod blk;
mod soc;
// mod serial;

#[allow(unused)]
fn iomap(base: u64, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mem::iomap((base as usize).into(), size)
        .map(|ptr| unsafe { NonNull::new_unchecked(ptr.as_mut_ptr()) })
        .map_err(|e| OnProbeError::Other(format!("{e}:?").into()))
}
