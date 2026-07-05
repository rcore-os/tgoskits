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

//! x86_vcpu's thin facade over host-neutral KVM UAPI helpers.
//!
//! vCPU code imports from this module to keep KVM wire-format details separate
//! from VMX/SVM state access.

pub(crate) use kvm_uapi::x86::{
    KVM_HYPERVISOR_FEATURE_LEAF, KVM_HYPERVISOR_INFO_LEAF, KVM_REGS_SIZE, KVM_SREGS_SIZE,
    KvmDtable, KvmRegs, KvmSegment, KvmSregs, kvm_hypervisor_cpuid, rustvisor_hypervisor_cpuid,
};

pub(crate) fn map_kvm_uapi_error(_err: kvm_uapi::KvmUapiError) -> ax_errno::AxError {
    ax_errno::AxError::InvalidInput
}
