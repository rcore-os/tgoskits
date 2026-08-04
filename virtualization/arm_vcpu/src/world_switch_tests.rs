const EXCEPTION_ASSEMBLY: &str = include_str!("architecture/exception.S");
const CONTEXT_FRAME: &str = include_str!("architecture/context_frame.rs");
const VCPU: &str = include_str!("architecture/vcpu.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end {end:?}"))
        .0
}

fn assert_in_order(source: &str, operations: &[&str]) {
    let mut cursor = 0;
    for operation in operations {
        let offset = source[cursor..]
            .find(operation)
            .unwrap_or_else(|| panic!("missing ordered operation {operation:?}"));
        cursor += offset + operation.len();
    }
}

#[test]
fn timer_world_switch_keeps_guest_and_host_counter_domains_transactional() {
    let exit = section(
        EXCEPTION_ASSEMBLY,
        ".macro SAVE_VCPU_RUNTIME_FROM_EL1",
        ".endm",
    );
    assert_in_order(
        exit,
        &[
            "mrs     x9, cntv_ctl_el0",
            "mrs     x9, cntv_cval_el0",
            "mrs     x9, cntkctl_el1",
            "msr     cntv_ctl_el0, xzr",
            "isb",
            "msr     cntvoff_el2, xzr",
            "msr     cnthctl_el2, x9",
            "msr     cntkctl_el1, x9",
            "strb    wzr, [sp, {timer_loaded_offset}]",
            "isb",
        ],
    );

    let entry = section(
        EXCEPTION_ASSEMBLY,
        ".Lexception_return_guest_el1:",
        ".Lexception_return_current_el2:",
    );
    assert_in_order(
        entry,
        &[
            "mrs     x9, cnthctl_el2",
            "mrs     x9, cntkctl_el1",
            "msr     cntv_ctl_el0, xzr",
            "isb",
            "msr     cntvoff_el2, x9",
            "msr     cnthctl_el2, x9",
            "msr     cntkctl_el1, x9",
            "msr     cntv_cval_el0, x9",
            "isb",
            "msr     cntv_ctl_el0, x9",
            "strb    w9, [sp, {timer_loaded_offset}]",
            "isb",
            "eret",
        ],
    );

    let lower_irq = section(EXCEPTION_ASSEMBLY, ".macro HANDLE_LOWER_IRQ_VCPU", ".endm");
    assert_in_order(
        lower_irq,
        &[
            "SAVE_REGS_FROM_EL1",
            "ACK_PENDING_HOST_IRQ",
            "SAVE_VCPU_RUNTIME_FROM_EL1",
            "bl      vmexit_trampoline",
        ],
    );

    let acknowledge = section(EXCEPTION_ASSEMBLY, ".macro ACK_PENDING_HOST_IRQ", ".endm");
    assert_in_order(
        acknowledge,
        &[
            "mov     w9, #-1",
            "str     w9, [sp, {host_pending_irq_ack_offset}]",
            "ldr     x9, [sp, {host_irq_interface_offset}]",
            "mrs     x9, ICC_IAR1_EL1",
            "str     w9, [sp, {host_pending_irq_ack_offset}]",
            "dsb     sy",
        ],
    );
    assert_in_order(
        acknowledge,
        &[
            ".Lack_host_irq_gicv2\\@:",
            "ldr     x10, [sp, {host_irq_cpu_interface_base_offset}]",
            "ldr     w9, [x10, #0xc]",
            "str     w9, [sp, {host_pending_irq_ack_offset}]",
            "dsb     sy",
        ],
    );
}

#[test]
fn exception_vector_table_preserves_the_architectural_slot_layout() {
    let vector_table = section(
        EXCEPTION_ASSEMBLY,
        "exception_vector_base_vcpu:",
        ".global context_vm_entry",
    );
    assert_eq!(vector_table.matches("VECTOR_SLOT ").count(), 16);
    assert!(!vector_table.contains("SAVE_REGS_FROM_EL1"));
    assert!(!vector_table.contains("SAVE_VCPU_REGS_FROM_EL1"));

    let slot_macro = section(EXCEPTION_ASSEMBLY, ".macro VECTOR_SLOT", ".endm");
    assert_in_order(slot_macro, &["b       \\handler", ".space  0x80 - 4"]);
}

#[test]
fn tls_switch_occurs_only_inside_the_final_assembly_windows() {
    let restore = section(
        CONTEXT_FRAME,
        "    pub unsafe fn restore(&self)",
        "    }\n}",
    );
    let store = section(
        CONTEXT_FRAME,
        "    pub unsafe fn store(&mut self)",
        "    /// Restores the values",
    );
    assert!(!restore.contains("msr TPIDR_EL0"));
    assert!(!store.contains("mrs {0}, TPIDR_EL0"));

    let exit = section(
        EXCEPTION_ASSEMBLY,
        ".macro SAVE_VCPU_RUNTIME_FROM_EL1",
        ".endm",
    );
    assert_in_order(
        exit,
        &[
            "mrs     x9, tpidr_el0",
            "str     x9, [sp, {guest_tpidr_el0_offset}]",
            "ldr     x9, [sp, {host_tpidr_el0_offset}]",
            "msr     tpidr_el0, x9",
        ],
    );
    assert!(!exit.contains("bl      "));

    let entry = section(
        EXCEPTION_ASSEMBLY,
        ".macro RESTORE_GUEST_REGS_INTO_EL1",
        ".endm",
    );
    assert_in_order(
        entry,
        &[
            "ldr     x9, [sp, {guest_tpidr_el0_offset}]",
            "msr     tpidr_el0, x9",
            "ldp     x8, x9, [sp, 8 * 8]",
        ],
    );
    assert!(!entry.contains("bl      "));
    assert!(VCPU.contains("offset_of!(HostRuntimeContext, tpidr_el0)"));
    assert!(CONTEXT_FRAME.contains("offset_of!(GuestSystemRegisters, tpidr_el0)"));
}
