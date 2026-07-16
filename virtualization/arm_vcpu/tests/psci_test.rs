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

#[path = "../src/psci.rs"]
mod psci;

use psci::PsciCall;

#[test]
fn version_and_trusted_os_queries_are_completed_inside_the_vm() {
    assert_eq!(
        psci::decode(0x8400_0000, [0; 3]),
        Some(PsciCall::Complete(0x0001_0000))
    );
    assert_eq!(
        psci::decode(0x8400_0006, [0; 3]),
        Some(PsciCall::Complete(2))
    );
}

#[test]
fn features_report_only_vm_implemented_calls() {
    assert_eq!(
        psci::decode(0x8400_000a, [0xc400_0003, 0, 0]),
        Some(PsciCall::Complete(0))
    );
    assert_eq!(
        psci::decode(0x8400_000a, [0xc400_0001, 0, 0]),
        Some(PsciCall::Complete(u64::MAX))
    );
}

#[test]
fn cpu_on_keeps_the_vm_local_target_and_boot_arguments() {
    assert_eq!(
        psci::decode(0xc400_0003, [0x100, 0x8020_0000, 0x55]),
        Some(PsciCall::CpuOn {
            target_cpu: 0x100,
            entry_point: 0x8020_0000,
            context: 0x55,
        })
    );
}

#[test]
fn unsupported_psci_calls_do_not_fall_through_to_host_firmware() {
    assert_eq!(
        psci::decode(0x8400_0001, [0; 3]),
        Some(PsciCall::Complete(u64::MAX))
    );
    assert_eq!(psci::decode(0x8600_0000, [0; 3]), None);
}
