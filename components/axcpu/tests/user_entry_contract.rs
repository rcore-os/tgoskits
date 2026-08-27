#[test]
fn raw_user_entry_is_unsafe_on_every_architecture() {
    let sources = [
        include_str!("../src/x86_64/uspace.rs"),
        include_str!("../src/aarch64/uspace.rs"),
        include_str!("../src/riscv/uspace.rs"),
        include_str!("../src/loongarch64/uspace.rs"),
    ];

    for source in sources {
        assert!(
            !source.contains("pub fn run(&mut self) -> ReturnReason"),
            "architecture code must not expose a safe raw user entry"
        );
        assert!(
            source.contains("pub unsafe fn run_unchecked(&mut self) -> ReturnReason"),
            "architecture code must document an explicit unsafe raw entry"
        );
    }
}

#[test]
fn aarch64_entry_validator_requires_the_runtime_daif_profile() {
    let source = include_str!("../src/aarch64/uspace.rs");
    let validator = source
        .split("pub fn is_user_entry_state_valid(&self) -> bool")
        .nth(1)
        .and_then(|tail| tail.split("pub unsafe fn run_unchecked").next())
        .expect("AArch64 user-entry validator must precede the raw entry");

    for required in ["SPSR_EL1::D", "SPSR_EL1::A", "SPSR_EL1::F"] {
        assert!(
            validator.contains(required),
            "AArch64 user entry must validate {required} before ERET"
        );
    }
    assert!(
        validator.contains("runtime_daif.mask()"),
        "AArch64 DAIF validation must use the shifted FieldValue mask"
    );
    assert!(
        !validator.contains("SPSR_EL1::D.mask"),
        "the unshifted Field mask cannot validate the hardware DAIF bits"
    );
}

#[cfg(all(target_arch = "x86_64", feature = "uspace"))]
#[test]
fn safe_entry_rejects_privileged_or_irq_masked_x86_state() {
    use ax_cpu::uspace::UserContext;
    use ax_memory_addr::VirtAddr;

    let mut context = UserContext::new(0x1000, VirtAddr::from(0x8000), 0);
    assert!(context.is_user_entry_state_valid());

    context.cs = 0;
    assert!(!context.is_user_entry_state_valid());

    let mut context = UserContext::new(0x1000, VirtAddr::from(0x8000), 0);
    context.rflags |= 0b11 << 12;
    assert!(!context.is_user_entry_state_valid());

    let mut context = UserContext::new(0x1000, VirtAddr::from(0x8000), 0);
    context.rflags &= !(1 << 9);
    assert!(!context.is_user_entry_state_valid());
}
