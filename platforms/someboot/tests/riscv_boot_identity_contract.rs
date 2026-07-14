use std::{fs, path::PathBuf};

fn source(relative_path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

#[test]
fn riscv_sscratch_points_to_a_versioned_boot_record() {
    let boot = source("src/arch/riscv64/boot.rs");
    assert!(boot.contains("#[repr(C)]"));
    assert!(boot.contains("struct CpuBootInfoV1"));
    assert!(boot.contains("abi_magic"));
    assert!(boot.contains("abi_version"));
    assert!(boot.contains("record_size"));

    let entry = source("src/arch/riscv64/entry.rs");
    assert!(
        !entry.contains("csrw sscratch, a0"),
        "firmware hart IDs must not be stored directly in sscratch"
    );
    assert_eq!(
        entry.matches("csrw sscratch, sp").count(),
        2,
        "both primary and secondary entries must publish the boot-record pointer"
    );
    assert_eq!(
        entry
            .matches("addi sp, sp, -{boot_info_stack_size}")
            .count(),
        2,
        "both entry stacks must reserve the boot-record slot"
    );

    let paging = source("src/arch/riscv64/paging.rs");
    let primary = paging
        .split("pub fn enable_mmu()")
        .nth(1)
        .expect("primary MMU path must exist")
        .split("pub fn enable_mmu_secondary")
        .next()
        .unwrap();
    assert!(primary.contains("let v_sp = meta.stack_top_virt;"));

    let secondary = paging
        .split("pub fn enable_mmu_secondary")
        .nth(1)
        .expect("secondary MMU path must exist");
    assert!(
        secondary.contains("meta.stack_top_virt - super::boot::STACK_SIZE"),
        "the aliased secondary stack must retain its boot-record slot"
    );
}

#[test]
fn shared_rust_reads_hart_identity_through_the_typed_record() {
    let arch = source("src/arch/riscv64/mod.rs");
    assert!(arch.contains("boot::current().hart_id()"));
    assert!(
        !arch.contains("csrr {hart_id}, sscratch"),
        "shared Rust must not interpret sscratch as a raw hart ID"
    );
}

#[test]
fn secondary_high_mapping_preserves_the_existing_metadata_abi() {
    let entry = source("src/arch/riscv64/entry.rs");
    let trampoline = entry
        .split("fn secondary_mmu_entry")
        .nth(1)
        .expect("secondary MMU trampoline must exist");
    assert!(trampoline.contains("cpu_meta_offset = const"));
    assert!(trampoline.contains("ld a0, {cpu_meta_offset}(a0)"));
    assert!(trampoline.contains("jr a1"));

    let paging = source("src/arch/riscv64/paging.rs");
    assert!(paging.contains("enable_mmu_secondary(cpu_boot_info_paddr: usize)"));
    assert!(paging.contains("in(\"a0\") cpu_boot_info_paddr"));
    let secondary = paging
        .split("pub fn enable_mmu_secondary")
        .nth(1)
        .expect("secondary MMU path must exist")
        .split("fn setup_page_table")
        .next()
        .unwrap();
    assert!(
        !secondary.contains("__kimage_va("),
        "pre-MMU secondary Rust must not dereference relocated global state"
    );
    assert!(secondary.contains("secondary_entry_phys"));
    assert!(secondary.contains("wrapping_sub(secondary_entry_phys)"));
}

#[test]
fn secondary_entry_owns_its_trap_vector_before_entering_shared_rust() {
    let entry = source("src/arch/riscv64/entry.rs");
    let secondary = entry
        .split("fn _secondary_entry")
        .nth(1)
        .expect("secondary entry must exist")
        .split("fn secondary_start")
        .next()
        .unwrap();

    assert!(secondary.contains("csrci sstatus, 2"));
    assert!(secondary.contains("csrw sie, zero"));
    assert!(secondary.contains("secondary_early_trap"));
    assert!(secondary.contains("csrw stvec"));
}
