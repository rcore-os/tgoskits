use axtest::prelude::*;

#[axtest]
fn someboot_mem_constants_and_cache_line_rules_hold() {
    ax_assert!(crate::mem::mem_constants_and_cache_line_rules_hold_for_test());
}

#[axtest]
fn someboot_mem_constants_and_types_hold() {
    ax_assert!(crate::mem::mem_constants_and_types_hold_for_test());
}

#[axtest]
fn someboot_mem_byte_unit_types_hold() {
    ax_assert!(crate::mem::mem_byte_unit_types_hold_for_test());
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn someboot_trap_constants_and_structs_hold() {
    ax_assert!(crate::arch::x86_64::trap::trap_constants_and_structs_hold_for_test());
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn someboot_pit_and_tsc_constants_hold() {
    ax_assert!(crate::arch::x86_64::trap::pit_and_tsc_constants_hold_for_test());
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn someboot_trap_idt_and_gate_constants_hold() {
    ax_assert!(crate::arch::x86_64::trap::trap_idt_and_gate_constants_hold_for_test());
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn someboot_trap_msr_and_efer_constants_hold() {
    ax_assert!(crate::arch::x86_64::trap::trap_msr_and_efer_constants_hold_for_test());
}
