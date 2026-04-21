struct VmInterruptIfImpl;

#[ax_crate_interface::impl_interface]
impl axplat_riscv64_qemu_virt_hv::irq::InjectIrqIf for VmInterruptIfImpl {
    fn inject_virtual_interrupt(irq: usize) {
        crate::hal::arch::inject_interrupt(irq);
    }
}
