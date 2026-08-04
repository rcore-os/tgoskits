//! Source-level contract for the LoongArch unaligned access helpers.
//!
//! Host tests cannot execute LoongArch exception-table assembly. Inspecting the
//! assembly is therefore the narrowest deterministic way to verify that every
//! memory fixup publishes the exact byte address before returning an error.

const UNALIGNED_ASM: &str = include_str!("../src/loongarch64/unaligned.S");

#[test]
fn memory_fault_fixups_publish_the_faulting_byte_address() {
    assert!(
        UNALIGNED_ASM.contains("a4: fault_addr"),
        "the read helper must accept a fault-address output"
    );
    assert!(
        UNALIGNED_ASM.contains("5:\tst.d\t$a0, $a4, 0"),
        "read fixups must publish the address of the failed byte load"
    );
    assert!(
        UNALIGNED_ASM.contains("a3: fault_addr"),
        "the write helper must accept a fault-address output"
    );
    assert!(
        UNALIGNED_ASM.contains("3:\tst.d\t$a0, $a3, 0"),
        "write fixups must publish the address of the failed byte store"
    );
}
