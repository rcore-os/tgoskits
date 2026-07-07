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
fn arm_vcpu_manifest_has_no_ax_runtime_dependencies() {
    let manifest = include_str!("../Cargo.toml");

    for forbidden in ["ax-errno", "ax-percpu", "ax-crate-interface", "axvm-types"] {
        assert!(
            !manifest.contains(forbidden),
            "arm_vcpu core must not depend on {forbidden}"
        );
    }
}
