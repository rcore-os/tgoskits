#![cfg(all(target_arch = "riscv64", feature = "uspace"))]

use ax_cpu::{VirtAddr, user::UserContext};
use riscv::register::sstatus::{SPP, Sstatus, XLEN};

#[test]
fn user_return_requires_rv64_execution() {
    let mut context = UserContext::new(0x10000, VirtAddr::from_usize(0x3fffff000), 7);
    assert_eq!(context.sstatus.uxl(), XLEN::XLEN64);
    assert!(context.has_interruptible_user_return_mode());

    for uxl in [0, 1, 3] {
        context.sstatus = Sstatus::from_bits((1 << 18) | (1 << 5) | (uxl << 32));
        assert!(!context.has_interruptible_user_return_mode());
        context.prepare_clone_child_return_state();
        assert_eq!(context.sstatus.uxl(), XLEN::XLEN64);
        assert!(context.has_interruptible_user_return_mode());
    }

    context.sstatus.set_spp(SPP::Supervisor);
    assert!(!context.has_interruptible_user_return_mode());
    context.sstatus.set_spp(SPP::User);
    context.sstatus.set_spie(false);
    assert!(!context.has_interruptible_user_return_mode());

    context.sstatus.set_spie(true);
    context.sstatus.set_sie(true);
    assert!(!context.has_interruptible_user_return_mode());
    context.prepare_clone_child_return_state();
    assert!(!context.sstatus.sie());
    assert!(context.has_interruptible_user_return_mode());
}
