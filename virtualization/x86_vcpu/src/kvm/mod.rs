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

mod cpuid;
mod regs;

pub(crate) use cpuid::{
    KVM_HYPERVISOR_FEATURE_LEAF, KVM_HYPERVISOR_INFO_LEAF, kvm_hypervisor_cpuid,
    rustvisor_hypervisor_cpuid,
};
pub(crate) use regs::{KVM_REGS_SIZE, KVM_SREGS_SIZE, KvmDtable, KvmRegs, KvmSegment, KvmSregs};
