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

#[test]
fn vm_core_does_not_handle_arch_local_exits() {
    let vm_rs = include_str!("../src/vm.rs");

    for forbidden in [
        "CurrentArch",
        "ArchOps",
        "CurrentArch::handle_vcpu_exit",
        "VcpuRunAction",
        "HostInterrupt",
    ] {
        assert!(
            !vm_rs.contains(forbidden),
            "vm.rs must not contain architecture-local exit handling detail: {forbidden}"
        );
    }
}

#[test]
fn runtime_vcpu_loop_only_consumes_scheduler_actions() {
    let runtime_vcpus_rs = include_str!("../src/runtime/vcpus.rs");

    for forbidden in [
        "VcpuRunAction::Continue",
        "VcpuRunAction::HostInterrupt",
        "HostInterrupt",
    ] {
        assert!(
            !runtime_vcpus_rs.contains(forbidden),
            "runtime/vcpus.rs must not match architecture-local exit action: {forbidden}"
        );
    }
}
