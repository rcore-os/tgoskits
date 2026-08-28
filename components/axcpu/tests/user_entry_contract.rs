#[cfg(all(target_arch = "x86_64", feature = "uspace"))]
#[test]
fn user_return_mode_rejects_privileged_or_irq_masked_x86_state() {
    use ax_cpu::uspace::UserContext;
    use ax_memory_addr::VirtAddr;

    let mut context = UserContext::new(0x1000, VirtAddr::from(0x8000), 0);
    assert!(context.has_interruptible_user_return_mode());

    context.cs = 0;
    assert!(!context.has_interruptible_user_return_mode());

    let mut context = UserContext::new(0x1000, VirtAddr::from(0x8000), 0);
    context.rflags |= 0b11 << 12;
    assert!(!context.has_interruptible_user_return_mode());

    let mut context = UserContext::new(0x1000, VirtAddr::from(0x8000), 0);
    context.rflags &= !(1 << 9);
    assert!(!context.has_interruptible_user_return_mode());
}
