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

//! Source contract for the registration-time IRQ-chip line binding.

const IRQ_API: &str = include_str!("../src/irq.rs");
const IRQ_LINE: &str = include_str!("../src/irq_line.rs");
const RISCV: &str = include_str!("../src/arch/riscv64/mod.rs");
const RISCV_PLIC: &str = include_str!("../src/arch/riscv64/plic.rs");
const LOONGARCH: &str = include_str!("../src/arch/loongarch64/mod.rs");
const AARCH64_GIC: &str = include_str!("../src/arch/aarch64/gic/mod.rs");
const X86_64: &str = include_str!("../src/arch/x86_64/mod.rs");

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
fn unmaskable_x86_ipi_uses_the_typed_action_gate() {
    let prepare = section(X86_64, "fn prepare_irq_line", "fn send_ipi");
    assert!(
        prepare.contains("lapic::ipi_vector(irq)?"),
        "the action-only endpoint must validate the exact LAPIC IPI identity"
    );
    assert!(
        prepare.contains("PreparedIrqChipLine::action_gate_only()"),
        "an unmaskable LAPIC IPI must not publish a fake maskable endpoint"
    );
}

#[test]
fn live_line_control_uses_only_a_prepared_value_binding() {
    assert!(
        IRQ_LINE.contains("pub fn prepare_irq_line"),
        "task context must resolve and validate an IRQ-chip line before publication"
    );
    assert!(
        IRQ_LINE.contains("pub fn set_bound_irq_enabled"),
        "the live path must consume the stable value-only line binding"
    );
    assert!(
        !IRQ_API.contains("pub fn set_controller_irq_enabled"),
        "the live path must not resolve a generic interrupt-controller device"
    );

    let live = section(
        IRQ_LINE,
        "pub fn set_bound_irq_enabled",
        "pub fn release_irq_line",
    );
    for forbidden in [
        "rdrive::",
        "intc_by_domain",
        ".lock()",
        ".try_lock()",
        "Vec<",
        "Box<",
        "alloc::",
        "Result<",
    ] {
        assert!(
            !live.contains(forbidden),
            "live IRQ-chip line control must not contain `{forbidden}`"
        );
    }
}

#[test]
fn architecture_live_paths_do_not_reenter_the_driver_registry() {
    let riscv = RISCV
        .split_once("unsafe impl IrqChipLine for RiscvIrqChipLine")
        .expect("missing RISC-V prepared line endpoint")
        .1;
    let loongarch = section(
        LOONGARCH,
        "unsafe impl IrqChipLine for LoongArchCpuLocalLine",
        "#[cfg(test)]",
    );
    let gic = section(
        AARCH64_GIC,
        "fn set_shared_line_enabled",
        "fn shared_line_status",
    );

    for (name, source) in [
        ("RISC-V", riscv),
        ("LoongArch", loongarch),
        ("AArch64", gic),
    ] {
        for forbidden in [
            "set_controller_irq_enabled",
            "intc_by_domain",
            "rdrive::",
            "with_gic_domain",
            ".try_lock()",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} live line control must not contain `{forbidden}`"
            );
        }
    }
}

#[test]
fn x86_live_line_owns_the_resolved_ioapic_endpoint() {
    let live = section(
        X86_64,
        "unsafe impl IrqChipLine for X86IrqChipLine",
        "fn ioapic_irq_for_vector",
    );
    assert!(
        X86_64.contains("IoApic { endpoint: IoApicLineEndpoint }"),
        "registration must retain the resolved IOAPIC MMIO endpoint"
    );
    for forbidden in ["endpoint_for_gsi", "set_gsi_enabled", "gsi_enabled"] {
        assert!(
            !live.contains(forbidden),
            "x86 live line control must not resolve `{forbidden}`"
        );
    }
}

#[test]
fn riscv_release_uses_the_exact_plic_lease_and_claim_barriers() {
    let endpoint_release = section(RISCV, "fn release(&self)", "}\n}");
    assert!(
        endpoint_release.contains("endpoint.lease_id()")
            && endpoint_release.contains("plic::release_irq_endpoints"),
        "RISC-V release must carry the exact generation-bearing endpoint lease"
    );

    let controller_release = section(
        RISCV_PLIC,
        "fn release_irq_endpoints(\n        &mut self,",
        "fn contexts_for_source",
    );
    assert!(
        controller_release.contains("PLIC_CLAIM_READERS.load(Ordering::Acquire)")
            && controller_release.contains("ACTIVE_PLIC_CLAIMS")
            && controller_release.contains("lease_generation_by_source[source_index]")
            && controller_release.contains("lease.generation"),
        "PLIC lease release must reject stale generations and synchronize claim readers and \
         active claims"
    );
}
