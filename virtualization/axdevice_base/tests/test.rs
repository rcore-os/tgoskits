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

extern crate alloc;

use alloc::{sync::Arc, vec};

use axdevice_base::{
    AccessWidth, BaseDeviceOps, BusAccess, BusKind, BusResponse, Device, DeviceId, DeviceResult,
    EmuDeviceType, MmioDeviceAdapter, NoopDeviceAccess,
};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

struct DeviceA;

impl BaseDeviceOps<GuestPhysAddrRange> for DeviceA {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        (0x1000..0x2000).try_into().unwrap()
    }

    fn handle_read(&self, addr: GuestPhysAddr, _width: AccessWidth) -> DeviceResult<usize> {
        Ok(addr.as_usize())
    }

    fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> DeviceResult {
        Ok(())
    }
}

struct DeviceB;

impl BaseDeviceOps<GuestPhysAddrRange> for DeviceB {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        (0x2000..0x3000).try_into().unwrap()
    }

    fn handle_read(&self, addr: GuestPhysAddr, _width: AccessWidth) -> DeviceResult<usize> {
        Ok(addr.as_usize())
    }

    fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> DeviceResult {
        Ok(())
    }
}

#[test]
fn test_device_type_test() {
    let devices: Vec<Arc<dyn Device>> = vec![
        MmioDeviceAdapter::from_arc(Arc::new(DeviceA)),
        MmioDeviceAdapter::from_arc(Arc::new(DeviceB)),
    ];

    for (index, device) in devices.iter().enumerate() {
        let addr = 0x1000 + index * 0x1000;
        let mut context = NoopDeviceAccess::new(DeviceId::new(index as u32));
        let resp = device
            .access(
                &BusAccess {
                    kind: BusKind::Mmio,
                    is_read: true,
                    addr: addr as u64,
                    width: AccessWidth::Byte,
                    data: 0,
                },
                &mut context,
            )
            .unwrap();
        assert!(matches!(
            resp,
            BusResponse::Read { value } if value as usize == addr
        ));
    }
}
