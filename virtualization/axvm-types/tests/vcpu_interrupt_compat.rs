// Copyright 2026 The Axvisor Team
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

use axvm_types::{
    GuestPhysAddr, InterruptTriggerMode, NestedPagingConfig, VCpuId, VMId, VmArchVcpuOps,
    VmBackendError, VmBackendResult,
};

#[derive(Debug, Default)]
struct LegacyVcpu {
    injected_vectors: Vec<usize>,
}

impl VmArchVcpuOps for LegacyVcpu {
    type CreateConfig = ();
    type SetupConfig = ();
    type Exit = ();

    fn new(_vm_id: VMId, _vcpu_id: VCpuId, _config: Self::CreateConfig) -> VmBackendResult<Self> {
        Ok(Self::default())
    }

    fn set_entry(&mut self, _entry: GuestPhysAddr) -> VmBackendResult {
        Ok(())
    }

    fn set_nested_page_table(&mut self, _config: NestedPagingConfig) -> VmBackendResult {
        Ok(())
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> VmBackendResult {
        Ok(())
    }

    fn run(&mut self) -> VmBackendResult<Self::Exit> {
        Ok(())
    }

    fn bind(&mut self) -> VmBackendResult {
        Ok(())
    }

    fn unbind(&mut self) -> VmBackendResult {
        Ok(())
    }

    fn set_gpr(&mut self, _reg: usize, _val: usize) {}

    fn inject_interrupt(&mut self, vector: usize) -> VmBackendResult {
        self.injected_vectors.push(vector);
        Ok(())
    }

    fn set_return_value(&mut self, _val: usize) {}
}

#[test]
fn legacy_backend_uses_safe_trigger_compatibility_default() {
    let mut vcpu = LegacyVcpu::new(1, 0, ()).unwrap();

    assert_eq!(
        vcpu.inject_interrupt_with_trigger(0x31, InterruptTriggerMode::EdgeTriggered),
        Ok(())
    );
    assert_eq!(vcpu.injected_vectors, [0x31]);

    assert_eq!(
        vcpu.inject_interrupt_with_trigger(0x32, InterruptTriggerMode::LevelTriggered),
        Err(VmBackendError::Unsupported)
    );
    assert_eq!(vcpu.injected_vectors, [0x31]);
}
