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
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, DeviceId,
    NoopDeviceAccess, Resource,
};

struct DeviceA;

impl Device for DeviceA {
    fn name(&self) -> &str {
        "device-a"
    }

    fn resources(&self) -> &[Resource] {
        static RESOURCES: [Resource; 1] = [Resource::MmioRange {
            base: 0x1000,
            size: 0x1000,
        }];
        &RESOURCES
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        Ok(BusResponse::Read { value: access.addr })
    }
}

struct DeviceB;

impl Device for DeviceB {
    fn name(&self) -> &str {
        "device-b"
    }

    fn resources(&self) -> &[Resource] {
        static RESOURCES: [Resource; 1] = [Resource::MmioRange {
            base: 0x2000,
            size: 0x1000,
        }];
        &RESOURCES
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        Ok(BusResponse::Read { value: access.addr })
    }
}

#[test]
fn test_device_type_test() {
    let devices: Vec<Arc<dyn Device>> = vec![Arc::new(DeviceA), Arc::new(DeviceB)];

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
