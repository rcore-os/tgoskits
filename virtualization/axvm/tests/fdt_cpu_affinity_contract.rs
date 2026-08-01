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
fn fdt_cpu_resolution_preserves_explicit_affinity_masks() {
    let parser = include_str!("../src/boot/fdt/core/parser.rs");
    let set_phys_cpu_sets = parser
        .split_once("pub fn set_phys_cpu_sets(")
        .expect("the shared FDT parser must resolve physical CPU topology")
        .1
        .split_once("fn add_device_address_config(")
        .expect("device address parsing must follow CPU topology resolution")
        .0;

    assert!(
        set_phys_cpu_sets.contains("crate_config.base.phys_cpu_sets.is_none()"),
        "FDT-derived singleton masks must only be installed when phys_cpu_sets is absent"
    );
}
