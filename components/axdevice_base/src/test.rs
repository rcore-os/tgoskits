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

use alloc::vec;
use alloc::{sync::Arc, vec::Vec};
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, device::AccessWidth};
use axerrno::AxResult;

use crate::{BaseDeviceOps, EmuDeviceType, map_device_of_type};

const DEVICE_A_TEST_METHOD_ANSWER: usize = 42;

struct DeviceA;

impl BaseDeviceOps<GuestPhysAddrRange> for DeviceA {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        (0x1000..0x2000).try_into().unwrap()
    }

    fn handle_read(&self, addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(addr.as_usize())
    }

    fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

impl DeviceA {
    /// A test method unique to DeviceA.
    pub fn test_method(&self) -> usize {
        DEVICE_A_TEST_METHOD_ANSWER
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

    fn handle_read(&self, addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(addr.as_usize())
    }

    fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

#[test]
fn test_device_type_test() {
    let devices: Vec<Arc<dyn BaseDeviceOps<GuestPhysAddrRange>>> =
        vec![Arc::new(DeviceA), Arc::new(DeviceB)];

    let mut device_a_found = false;
    for device in devices {
        assert_eq!(
            device.handle_read(0x2000.into(), AccessWidth::Byte),
            Ok(0x2000)
        );

        if let Some(answer) = map_device_of_type(&device, |d: &DeviceA| d.test_method()) {
            assert_eq!(answer, DEVICE_A_TEST_METHOD_ANSWER);
            device_a_found = true;
        }
    }
    assert!(device_a_found, "DeviceA was not found");
}
