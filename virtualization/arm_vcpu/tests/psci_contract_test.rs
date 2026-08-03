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
fn psci_version_is_completed_inside_the_vcpu_core() {
    let exception = include_str!("../src/exception.rs");
    let handler = exception
        .split_once("fn handle_psci_call")
        .expect("the PSCI handler must exist")
        .1
        .split_once("fn handle_smc64_exception")
        .expect("the SMC handler must follow the PSCI handler")
        .0;

    assert!(
        handler.contains("Some(PSCI_FN_VERSION) =>"),
        "PSCI_VERSION must have an explicit in-core response"
    );
    assert!(
        handler.contains("ctx.gpr[0] = PSCI_VERSION_0_2"),
        "PSCI_VERSION must return the advertised PSCI 0.2 encoding"
    );
    assert!(
        handler.contains("Some(Ok(ArmVmExit::Nothing))"),
        "PSCI_VERSION must resume the guest without a private hypercall exit"
    );
    assert!(
        !handler.contains("Some(PSCI_FN_VERSION..PSCI_FN_END) => None"),
        "PSCI_VERSION must not fall through to the private hypercall decoder"
    );
}
