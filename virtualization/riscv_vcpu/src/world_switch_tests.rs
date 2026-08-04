const TRAP_ASSEMBLY: &str = include_str!("trap.S");
const TRAP_RUST: &str = include_str!("trap.rs");
const VCPU: &str = include_str!("vcpu.rs");

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
fn vm_exit_restores_host_anchors_before_returning_to_rust() {
    let exit = section(TRAP_ASSEMBLY, "_guest_exit:", "ret");
    assert_in_order(
        exit,
        &[
            "sd   gp, ({guest_gp})(a0)",
            "sd   tp, ({guest_tp})(a0)",
            "sd    t0, ({guest_a0})(a0)",
            "RESTORE_HOST_ANCHORS",
            "_restore_csrs:",
        ],
    );

    let restore = section(TRAP_ASSEMBLY, ".macro RESTORE_HOST_ANCHORS", ".endm");
    assert_in_order(
        restore,
        &[
            "ld    t1, ({hyp_sscratch})(a0)",
            "csrw  sscratch, t1",
            "ld    gp, ({hyp_gp})(a0)",
            "ld    tp, ({hyp_tp})(a0)",
        ],
    );
    assert!(!exit.contains("call ") && !exit.contains("tail "));

    for binding in [
        "hyp_gp = const hyp_gpr_offset(GprIndex::GP)",
        "hyp_tp = const hyp_gpr_offset(GprIndex::TP)",
        "hyp_sscratch = const hyp_csr_offset!(sscratch)",
    ] {
        assert!(
            TRAP_RUST.contains(binding),
            "missing typed offset {binding:?}"
        );
    }
}

#[test]
fn mmio_decode_uses_the_trap_snapshot() {
    let decode = section(
        VCPU,
        "fn decode_instr_at(",
        "/// Handle a guest page fault.",
    );
    assert!(decode.contains("self.regs.trap_csrs.htinst"));
    assert!(!decode.contains("riscv_h::register::htinst::read()"));
}
