// Copyright 2026 The TGOSKits Authors
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

//! x86 console IRQ identity must stay in the firmware GSI namespace until the
//! platform interrupt controller allocates a CPU vector.

const SOMEBOOT_CONSOLE: &str = include_str!("../../someboot/src/arch/x86_64/console.rs");
const PLATFORM_CONSOLE: &str = include_str!("../src/console.rs");
const RDRIVE_ACPI: &str = include_str!("../../../drivers/rdrive/src/probe/acpi.rs");
const OLD_VECTOR_BASE_NAME: &str = concat!("PCI_INTX_", "VECTOR_BASE");

#[test]
fn x86_console_exposes_gsi_without_fabricating_a_cpu_vector() {
    assert!(SOMEBOOT_CONSOLE.contains("const COM1_GSI: usize = 4"));
    assert!(SOMEBOOT_CONSOLE.contains("Some(COM1_GSI)"));
    assert!(PLATFORM_CONSOLE.contains("IrqSource::AcpiGsi(gsi)"));

    for source in [SOMEBOOT_CONSOLE, PLATFORM_CONSOLE, RDRIVE_ACPI] {
        assert!(!source.contains(OLD_VECTOR_BASE_NAME));
    }
    assert!(!SOMEBOOT_CONSOLE.contains("0x30 + 4"));
    assert!(!PLATFORM_CONSOLE.contains("checked_sub"));
}
