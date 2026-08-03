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

use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, DeviceResult,
    Resource,
};
use axvm_types::GuestPhysAddr;

use crate::vgic::Vgic;

const VGIC_V2_BASE: usize = 0x800_0000;
const VGIC_V2_SIZE: usize = 0x10000;
static VGIC_V2_RESOURCES: [Resource; 1] = [Resource::MmioRange {
    base: VGIC_V2_BASE as u64,
    size: VGIC_V2_SIZE as u64,
}];

impl Vgic {
    /// Handles memory read operations.
    ///
    /// Based on the given physical address and read width, performs the corresponding read operation.
    /// Supports reading 1 byte, 2 bytes, and 4 bytes. This function dereferences the provided physical
    /// address and calls the specific read function based on the width parameter.
    ///
    /// Parameters:
    /// - `addr`: The physical address to read from.
    /// - `width`: The width of the data to be read, determining the size of the read operation.
    ///
    /// Returns:
    /// - `DeviceResult<usize>`: The result of the read operation, including any errors and the size of the data read.
    pub fn read_register(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if !contains_vgic_v2(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        // Perform bitwise operation to ensure the address is aligned to byte boundaries
        let addr = addr.as_usize() & 0xfff;

        // Match different read operations based on the width parameter
        let value = match width {
            AccessWidth::Byte => {
                // Handle 1-byte read
                self.handle_read8(addr)?
            }
            AccessWidth::Word => {
                // Handle 2-byte read
                self.handle_read16(addr)?
            }
            AccessWidth::Dword => {
                // Handle 4-byte read
                self.handle_read32(addr)?
            }
            // Return success for unsupported widths without performing any operation
            _ => 0,
        };
        Ok(value)
    }
    /// Handles write operations of different widths.
    ///
    /// This function performs a write operation based on the given physical address, width, and value.
    /// It first converts the physical address to a `usize` and applies a mask to ensure proper alignment.
    /// Then, depending on the width parameter, it calls the corresponding write handling function.
    ///
    /// Parameters:
    /// - `addr`: The physical address to write to.
    /// - `width`: The byte width of the data to be written (1, 2, 4 for 8-bit, 16-bit, and 32-bit data respectively).
    /// - `val`: The value to be written.
    pub fn write_register(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        if !contains_vgic_v2(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        // Convert the physical address to a `usize` and apply a mask to ensure proper alignment
        let addr = addr.as_usize() & 0xfff;

        // Depending on the width parameter, perform the corresponding write operation
        match width {
            AccessWidth::Byte => {
                // Handle 8-bit write operation
                self.handle_write8(addr, val);
                Ok(())
            }
            AccessWidth::Word => {
                // Handle 16-bit write operation
                self.handle_write16(addr, val);
                Ok(())
            }
            AccessWidth::Dword => {
                // Handle 32-bit write operation
                self.handle_write32(addr, val);
                Ok(())
            }
            // For other width values, do nothing
            _ => Ok(()),
        }
    }
}

impl Device for Vgic {
    fn name(&self) -> &str {
        "aarch64-vgic-v2"
    }

    fn resources(&self) -> &[Resource] {
        &VGIC_V2_RESOURCES
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let addr = GuestPhysAddr::from_usize(access.addr as usize);
        if access.is_read {
            self.read_register(addr, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
        } else {
            self.write_register(addr, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
        }
    }
}

fn contains_vgic_v2(addr: GuestPhysAddr) -> bool {
    let addr = addr.as_usize();
    (VGIC_V2_BASE..VGIC_V2_BASE + VGIC_V2_SIZE).contains(&addr)
}
