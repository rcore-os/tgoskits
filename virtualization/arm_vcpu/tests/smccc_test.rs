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

#[path = "../src/smccc.rs"]
mod smccc;

#[test]
fn architecture_discovery_is_vm_local_and_bounded() {
    assert_eq!(
        smccc::architecture_call(0x8000_0000, 0),
        Some(0x0001_0001),
        "PSCI_FEATURES advertises the SMCCC v1.1 version call"
    );
    assert_eq!(
        smccc::architecture_call(0x8000_0001, 0x8000_8000),
        Some(u64::MAX),
        "unimplemented architectural features must be reported as unsupported"
    );
    assert_eq!(
        smccc::architecture_call(0xc000_0042, 0),
        Some(u64::MAX),
        "the architecture-owned 64-bit range must not escape to VMM hypercalls"
    );
    assert_eq!(
        smccc::architecture_call(0x8600_0000, 0),
        None,
        "vendor hypervisor calls remain outside the architecture dispatcher"
    );
}
