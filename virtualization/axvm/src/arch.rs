//! Architecture component glue owned by AxVM.

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use alloc::boxed::Box;
    use core::time::Duration;

    use ax_crate_interface::impl_interface;
    use ax_errno::AxResult;
    use ax_memory_addr::{PhysAddr, VirtAddr};
    use axvcpu::get_current_vcpu;
    use axvm_types::{InterruptVector, VCpuId, VMId};
    use x86_vcpu::host::X86VcpuHostIf;
    use x86_vlapic::host::X86VlapicHostIf;

    use crate::{
        host::{HostConsole, HostMemory, HostTime, default_host},
        manager,
        vcpu::AxArchVCpuImpl,
    };

    struct X86VcpuHostIfImpl;

    #[impl_interface]
    impl X86VcpuHostIf for X86VcpuHostIfImpl {
        fn alloc_frame() -> Option<PhysAddr> {
            default_host().alloc_frame()
        }

        fn dealloc_frame(paddr: PhysAddr) {
            default_host().dealloc_frame(paddr);
        }

        fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
            default_host().alloc_contiguous_frames(frame_count, frame_align)
        }

        fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
            default_host().dealloc_contiguous_frames(start_paddr, frame_count);
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            default_host().phys_to_virt(paddr)
        }

        fn nanos_to_ticks(nanos: u64) -> u64 {
            default_host().nanos_to_ticks(nanos)
        }
    }

    struct X86VlapicHostIfImpl;

    #[impl_interface]
    impl X86VlapicHostIf for X86VlapicHostIfImpl {
        fn alloc_frame() -> Option<PhysAddr> {
            default_host().alloc_frame()
        }

        fn dealloc_frame(paddr: PhysAddr) {
            default_host().dealloc_frame(paddr);
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            default_host().phys_to_virt(paddr)
        }

        fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
            default_host().virt_to_phys(vaddr)
        }

        fn current_time() -> Duration {
            default_host().monotonic_time()
        }

        fn current_time_nanos() -> u64 {
            default_host().monotonic_time().as_nanos() as u64
        }

        fn register_timer(
            deadline: Duration,
            callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> usize {
            default_host().register_timer(deadline.as_nanos() as u64, callback)
        }

        fn cancel_timer(token: usize) {
            default_host().cancel_timer(token);
        }

        fn write_bytes(bytes: &[u8]) {
            default_host().write_bytes(bytes);
        }

        fn read_bytes(bytes: &mut [u8]) -> usize {
            default_host().read_bytes(bytes)
        }

        fn current_vm_id() -> VMId {
            get_current_vcpu::<AxArchVCpuImpl>()
                .expect("current x86 vCPU is not set")
                .vm_id()
        }

        fn current_vm_vcpu_num() -> usize {
            let vm_id = Self::current_vm_id();
            manager::with_vm(vm_id, |vm| vm.vcpu_num()).unwrap_or(0)
        }

        fn current_vm_active_vcpus() -> usize {
            manager::active_vcpu_mask(Self::current_vm_id()).unwrap_or(0)
        }

        fn active_vcpus(vm_id: VMId) -> Option<usize> {
            manager::active_vcpu_mask(vm_id)
        }

        fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) -> AxResult {
            manager::inject_interrupt(vm_id, vcpu_id, vector as usize)
        }
    }
}

#[cfg(target_arch = "riscv64")]
mod riscv64 {
    use ax_crate_interface::impl_interface;
    use ax_memory_addr::{PhysAddr, VirtAddr};
    #[cfg(feature = "plat-dyn")]
    use axdevice_base::AccessWidth;
    #[cfg(feature = "plat-dyn")]
    use axvm_types::GuestPhysAddr;
    use riscv_vcpu::host::RiscvVcpuHostIf;
    use riscv_vplic::host::RiscvVplicHostIf;

    use crate::host::{HostMemory, default_host};

    #[cfg(feature = "plat-dyn")]
    const GUEST_PLIC_PADDR: usize = 0x0c00_0000;

    struct RiscvVcpuHostIfImpl;

    #[impl_interface]
    impl RiscvVcpuHostIf for RiscvVcpuHostIfImpl {
        fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
            default_host().virt_to_phys(vaddr)
        }
    }

    struct RiscvVplicHostIfImpl;

    #[impl_interface]
    impl RiscvVplicHostIf for RiscvVplicHostIfImpl {
        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            default_host().phys_to_virt(paddr)
        }
    }

    #[cfg(feature = "plat-dyn")]
    pub(crate) fn register_platform_irq_injector() {
        axplat_dyn::register_virtual_irq_injector(inject_virtual_irq);
    }

    #[cfg(not(feature = "plat-dyn"))]
    compile_error!("riscv64 Axvisor requires the plat-dyn feature");

    #[cfg(feature = "plat-dyn")]
    fn inject_virtual_irq(irq_id: usize) -> bool {
        debug!("injecting RISC-V virtual IRQ id: {irq_id}");

        let Some(vm_id) = crate::current_vm_id() else {
            warn!("cannot inject RISC-V virtual IRQ without current VM context");
            return false;
        };

        let Some(injected) = crate::manager::with_vm(vm_id, |vm| {
            let Some(vplic) = vm
                .get_devices()
                .find_mmio_dev(GuestPhysAddr::from_usize(GUEST_PLIC_PADDR))
            else {
                warn!("VM[{vm_id}] has no virtual PLIC device");
                return false;
            };

            let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
            let addr = GuestPhysAddr::from_usize(GUEST_PLIC_PADDR + reg_offset);
            let val: u32 = 1 << (irq_id % 32);

            if let Err(err) = vplic.handle_write(addr, AccessWidth::Dword, val as _) {
                warn!("failed to inject RISC-V virtual IRQ {irq_id}: {err:?}");
                return false;
            }
            true
        }) else {
            warn!("cannot inject RISC-V virtual IRQ {irq_id}: VM[{vm_id}] not found");
            return false;
        };

        injected
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn register_platform_irq_injector() {
    riscv64::register_platform_irq_injector();
}

#[cfg(not(target_arch = "riscv64"))]
pub(crate) fn register_platform_irq_injector() {}

#[cfg(target_arch = "loongarch64")]
mod loongarch64 {
    use ax_crate_interface::impl_interface;
    use ax_memory_addr::{PhysAddr, VirtAddr};
    use loongarch_vcpu::host::LoongArchVcpuHostIf;

    use crate::host::{HostMemory, default_host};

    struct LoongArchVcpuHostIfImpl;

    #[impl_interface]
    impl LoongArchVcpuHostIf for LoongArchVcpuHostIfImpl {
        fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
            default_host().virt_to_phys(vaddr)
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use alloc::boxed::Box;
    use core::time::Duration;

    use arm_vcpu::host::ArmVcpuHostIf;
    use arm_vgic::host::ArmVgicHostIf;
    use ax_crate_interface::impl_interface;
    use ax_memory_addr::{PhysAddr, VirtAddr};

    use crate::host::{HostCpu, HostMemory, HostTime, default_host, gic};

    struct ArmVcpuHostIfImpl;

    #[impl_interface]
    impl ArmVcpuHostIf for ArmVcpuHostIfImpl {
        fn hardware_inject_virtual_interrupt(vector: u8) {
            gic::inject_interrupt(vector as usize);
        }

        fn fetch_irq() -> usize {
            gic::fetch_irq()
        }

        fn handle_irq() {
            gic::handle_current_irq();
        }
    }

    struct ArmVgicHostIfImpl;

    #[impl_interface]
    impl ArmVgicHostIf for ArmVgicHostIfImpl {
        fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
            default_host().alloc_contiguous_frames(frame_count, frame_align)
        }

        fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
            default_host().dealloc_contiguous_frames(start_paddr, frame_count);
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            default_host().phys_to_virt(paddr)
        }

        fn host_cpu_num() -> usize {
            default_host().cpu_count()
        }

        fn current_vcpu_id() -> usize {
            crate::current_vcpu_id().expect("current AArch64 vCPU is not set")
        }

        fn current_time_nanos() -> u64 {
            default_host().monotonic_time().as_nanos() as u64
        }

        fn register_timer(
            deadline: Duration,
            callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) {
            let _ = default_host().register_timer(deadline.as_nanos() as u64, callback);
        }

        fn read_vgicd_iidr() -> u32 {
            gic::read_gicd_iidr()
        }

        fn read_vgicd_typer() -> u32 {
            gic::read_gicd_typer()
        }

        fn get_host_gicd_base() -> PhysAddr {
            gic::host_gicd_base()
        }

        fn get_host_gicr_base() -> PhysAddr {
            gic::host_gicr_base()
        }

        fn hardware_inject_virtual_interrupt(vector: u8) {
            gic::inject_interrupt(vector as usize);
        }
    }
}
