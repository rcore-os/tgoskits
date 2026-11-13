//! Owned vector banks and the temporary assembly entry descriptor.

use core::mem::align_of;

use super::VirtualizationError;
use crate::virtualization::StateMemory;

/// Vector controls retained across guest exits independently of host state.
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub(in crate::arch::riscv64) struct VectorCsrs {
    /// Element at which a suspended vector instruction resumes.
    pub vstart: usize,
    /// Active vector length.
    pub vl: usize,
    /// Vector type, including a valid `vill` reset state.
    pub vtype: usize,
    /// Fixed-point rounding mode and saturation flag.
    pub vcsr: usize,
}

#[repr(C)]
#[derive(Debug)]
pub(in crate::arch::riscv64) struct VectorBank {
    pub(in crate::arch::riscv64) registers: *mut usize,
    pub(in crate::arch::riscv64) csrs: VectorCsrs,
}

#[repr(C)]
#[derive(Debug)]
pub(in crate::arch::riscv64) struct VectorEntry {
    pub(in crate::arch::riscv64) guest: VectorBank,
    pub(in crate::arch::riscv64) host: VectorBank,
}

/// Storage belongs to the vCPU; cloning creates independent register banks.
#[derive(Debug, Clone)]
pub(in crate::arch::riscv64) struct VectorState<M: StateMemory> {
    guest: M,
    host: M,
    vlenb: usize,
    pub(in crate::arch::riscv64) controls: VectorCsrs,
}

impl<M: StateMemory> VectorState<M> {
    pub(in crate::arch::riscv64) fn new(
        vlenb: usize,
        host: M,
        guest: M,
    ) -> Result<Self, VirtualizationError> {
        if !(16..=8192).contains(&vlenb) || !vlenb.is_power_of_two() {
            return Err(VirtualizationError::InvalidVectorState);
        }
        let bytes = vlenb
            .checked_mul(32)
            .ok_or(VirtualizationError::InvalidVectorState)?;
        for bank in [&host, &guest] {
            let address = bank.virtual_address().as_ptr() as usize;
            if bank.byte_len() < bytes
                || !address.is_multiple_of(align_of::<usize>())
                || address.checked_add(bytes).is_none()
            {
                return Err(VirtualizationError::InvalidVectorState);
            }
        }
        for bank in [&host, &guest] {
            // SAFETY: StateMemory owns an initialized exclusive writable range;
            // validation above bounds these bytes. Both leases remain alive and
            // no assembly descriptor exists during construction.
            unsafe { bank.virtual_address().as_ptr().write_bytes(0, bytes) };
        }
        Ok(Self {
            guest,
            host,
            vlenb,
            controls: VectorCsrs::default(),
        })
    }

    pub(in crate::arch::riscv64) fn register_bytes(&self) -> usize {
        self.vlenb
    }

    // This descriptor is used only under the exclusive Vcpu entry borrow.
    // Neither owned bank can move, resize or be dropped during that call.
    pub(in crate::arch::riscv64) fn entry(&mut self) -> VectorEntry {
        VectorEntry {
            guest: VectorBank {
                registers: self.guest.virtual_address().as_ptr().cast(),
                csrs: self.controls,
            },
            host: VectorBank {
                registers: self.host.virtual_address().as_ptr().cast(),
                csrs: VectorCsrs::default(),
            },
        }
    }
}
