// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use axaddrspace::{device::AccessWidth, HostPhysAddr};
use axerrno::AxResult;

pub(crate) fn perform_mmio_read(addr: HostPhysAddr, width: AccessWidth) -> AxResult<usize> {
    let addr = axvisor_api::memory::phys_to_virt(addr).as_ptr();

    match width {
        AccessWidth::Byte => Ok(unsafe { addr.read_volatile() as _ }),
        AccessWidth::Word => Ok(unsafe { (addr as *const u16).read_volatile() as _ }),
        AccessWidth::Dword => Ok(unsafe { (addr as *const u32).read_volatile() as _ }),
        AccessWidth::Qword => Ok(unsafe { (addr as *const u64).read_volatile() as _ }),
    }
}

pub(crate) fn perform_mmio_write(
    addr: HostPhysAddr,
    width: AccessWidth,
    val: usize,
) -> AxResult<()> {
    let addr = axvisor_api::memory::phys_to_virt(addr).as_mut_ptr();

    match width {
        AccessWidth::Byte => unsafe {
            addr.write_volatile(val as _);
        },
        AccessWidth::Word => unsafe {
            (addr as *mut u16).write_volatile(val as _);
        },
        AccessWidth::Dword => unsafe {
            (addr as *mut u32).write_volatile(val as _);
        },
        AccessWidth::Qword => unsafe {
            (addr as *mut u64).write_volatile(val as _);
        },
    }

    Ok(())
}

pub use super::vgicr::enable_one_lpi;
