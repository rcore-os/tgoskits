struct VmInterruptIfImpl;

#[crate_interface::impl_interface]
impl axplat_riscv64_qemu_virt::irq::InjectIrqIf for VmInterruptIfImpl {
    fn inject_virtual_interrupt(irq: usize) {
        crate::hal::arch::inject_interrupt(irq);
    }
}
