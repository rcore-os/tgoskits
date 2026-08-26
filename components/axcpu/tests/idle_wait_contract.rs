// SPDX-License-Identifier: Apache-2.0

//! Static checks for the IRQ-masked idle handoff on every supported architecture.

const X86_ASM: &str = include_str!("../src/x86_64/asm.rs");
const AARCH64_ASM: &str = include_str!("../src/aarch64/asm.rs");
const RISCV_ASM: &str = include_str!("../src/riscv/asm.rs");
const LOONGARCH_ASM: &str = include_str!("../src/loongarch64/asm.rs");

fn function<'a>(source: &'a str, name: &str, next_doc: &str) -> &'a str {
    source
        .split_once(name)
        .unwrap_or_else(|| panic!("missing function `{name}`"))
        .1
        .split_once(next_doc)
        .unwrap_or_else(|| panic!("missing function boundary after `{name}`"))
        .0
}

#[test]
fn every_architecture_has_an_irq_masked_wait_primitive() {
    for source in [X86_ASM, AARCH64_ASM, RISCV_ASM, LOONGARCH_ASM] {
        assert!(
            source.contains("pub fn wait_for_irqs_disabled()"),
            "scheduler idle needs an IRQ-masked final wait primitive"
        );
    }
}

#[test]
fn x86_uses_the_interrupt_shadow_for_enable_and_halt() {
    let wait = function(
        X86_ASM,
        "pub fn wait_for_irqs_disabled()",
        "/// Halt the current CPU.",
    );
    assert!(
        wait.contains("sti; hlt"),
        "x86 must not expose a wake-loss window between STI and HLT"
    );
}

#[test]
fn wfi_architectures_enable_irqs_only_after_the_masked_wait() {
    for source in [AARCH64_ASM, RISCV_ASM] {
        let wait = function(
            source,
            "pub fn wait_for_irqs_disabled()",
            "/// Halt the current CPU.",
        );
        let wfi = wait.find("wfi").expect("masked wait must execute WFI");
        let enable = wait
            .find("enable_irqs()")
            .expect("masked wait must restore local IRQ delivery");
        assert!(wfi < enable, "WFI must run before local IRQs are restored");
    }
}
