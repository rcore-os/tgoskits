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

//! Source contract for the x86 IOAPIC hard-IRQ control path.
//!
//! Device discovery and route construction belong to the boot/control plane.
//! Once a route is published, mask/unmask from a hard IRQ must use fixed,
//! allocation-free IRQ-side state and must not re-enter the rdrive registry.

const X86_IRQ: &str = include_str!("../src/arch/x86_64/mod.rs");
const RDIF_IRQ_TYPES: &str = include_str!("../../../drivers/interface/rdif-def/src/irq.rs");
const FRAMEWORK_IRQ_TYPES: &str = include_str!("../../../components/irq-framework/src/types.rs");
const RDRIVE_ACPI: &str = include_str!("../../../drivers/rdrive/src/probe/acpi.rs");
const OLD_VECTOR_BASE_NAME: &str = concat!("PCI_INTX_", "VECTOR_BASE");
const OLD_RESOLVE_VECTOR_NAME: &str = concat!("fn resolve_", "vector");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing `{start}`"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing `{end}` after `{start}`"))
        .0
}

#[test]
fn ioapic_masking_uses_a_prepublished_irq_endpoint() {
    let set_enable = section(X86_IRQ, "fn irq_set_enable", "fn irq_set_affinity");

    assert!(
        set_enable.contains("IOAPIC_CPU_IF.set_gsi_enabled"),
        "IOAPIC masking must use the pre-published CPU/IRQ-side endpoint"
    );
    for forbidden in ["intc_by_domain", "rdrive::get", ".try_lock()"] {
        assert!(
            !set_enable.contains(forbidden),
            "hard-IRQ-capable masking must not contain `{forbidden}`"
        );
    }

    let endpoint = section(X86_IRQ, "fn set_gsi_enabled", "fn reserve_gsi_endpoint");
    assert!(
        endpoint.contains("endpoint_for_gsi"),
        "hard-IRQ masking must resolve a fixed pre-published endpoint"
    );
    assert!(
        endpoint.contains("mmio_lock.lock()"),
        "IOREGSEL/IOWIN access must use the IRQ-safe MMIO lock"
    );
    for forbidden in [
        "rdrive::",
        "intc_by_domain",
        "get_list",
        "Vec<",
        "Box<",
        "alloc::",
        ".try_lock()",
    ] {
        assert!(
            !endpoint.contains(forbidden),
            "the transitive hard-IRQ endpoint must not contain `{forbidden}`"
        );
    }

    let lookup = section(X86_IRQ, "fn endpoint_for_gsi", "fn set_gsi_enabled");
    assert!(
        lookup.contains("0..EXTERNAL_VECTOR_CAPACITY"),
        "endpoint lookup must have a fixed scan bound"
    );
    for forbidden in [
        "rdrive::",
        "intc_by_domain",
        "get_list",
        "Vec<",
        "Box<",
        "alloc::",
        ".lock()",
        ".try_lock()",
    ] {
        assert!(
            !lookup.contains(forbidden),
            "the endpoint lookup helper must not contain `{forbidden}`"
        );
    }

    let configure = section(X86_IRQ, "fn configure_acpi", "fn set_enabled");
    assert!(
        configure.contains("reserve_gsi_endpoint"),
        "the control plane must reserve the IRQ-side endpoint before MMIO programming"
    );
    assert!(
        configure.contains("publish"),
        "the control plane must publish only after masked MMIO programming"
    );
}

#[test]
fn ioapic_gsi_storage_is_keyed_not_directly_indexed() {
    assert!(
        X86_IRQ.contains("IoApicEndpointSlot"),
        "IOAPIC routes require fixed preallocated endpoint slots"
    );
    assert!(
        !X86_IRQ.contains("gsi_routes: [AtomicU64; 256]"),
        "GSI numeric values must not index a 256-element route array"
    );
    assert!(
        !X86_IRQ.contains(".get(gsi as usize)"),
        "hard-IRQ lookup must use the full u32 GSI key"
    );
}

#[test]
fn acpi_route_types_cannot_carry_a_fabricated_cpu_vector() {
    for source in [RDIF_IRQ_TYPES, FRAMEWORK_IRQ_TYPES] {
        let route = section(source, "pub struct AcpiGsiRoute", "}");
        assert!(!route.contains("vector"));
    }
    assert!(!RDRIVE_ACPI.contains(OLD_RESOLVE_VECTOR_NAME));
    assert!(!RDRIVE_ACPI.contains(OLD_VECTOR_BASE_NAME));
}
