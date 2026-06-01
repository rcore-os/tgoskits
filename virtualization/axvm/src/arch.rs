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
        host::{HostConsole, HostMemory, HostTime, arceos::arceos_host},
        manager,
        vcpu::AxArchVCpuImpl,
    };

    struct X86VcpuHostIfImpl;

    #[impl_interface]
    impl X86VcpuHostIf for X86VcpuHostIfImpl {
        fn alloc_frame() -> Option<PhysAddr> {
            arceos_host().alloc_frame()
        }

        fn dealloc_frame(paddr: PhysAddr) {
            arceos_host().dealloc_frame(paddr);
        }

        fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
            arceos_host().alloc_contiguous_frames(frame_count, frame_align)
        }

        fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
            arceos_host().dealloc_contiguous_frames(start_paddr, frame_count);
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            arceos_host().phys_to_virt(paddr)
        }

        fn nanos_to_ticks(nanos: u64) -> u64 {
            arceos_host().nanos_to_ticks(nanos)
        }
    }

    struct X86VlapicHostIfImpl;

    #[impl_interface]
    impl X86VlapicHostIf for X86VlapicHostIfImpl {
        fn alloc_frame() -> Option<PhysAddr> {
            arceos_host().alloc_frame()
        }

        fn dealloc_frame(paddr: PhysAddr) {
            arceos_host().dealloc_frame(paddr);
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            arceos_host().phys_to_virt(paddr)
        }

        fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
            arceos_host().virt_to_phys(vaddr)
        }

        fn current_time() -> Duration {
            arceos_host().monotonic_time()
        }

        fn current_time_nanos() -> u64 {
            arceos_host().monotonic_time().as_nanos() as u64
        }

        fn register_timer(
            deadline: Duration,
            callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> usize {
            arceos_host().register_timer(deadline.as_nanos() as u64, callback)
        }

        fn cancel_timer(token: usize) {
            arceos_host().cancel_timer(token);
        }

        fn write_bytes(bytes: &[u8]) {
            arceos_host().write_bytes(bytes);
        }

        fn read_bytes(bytes: &mut [u8]) -> usize {
            arceos_host().read_bytes(bytes)
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
    use axaddrspace::{GuestPhysAddr, device::AccessWidth};
    use riscv_vcpu::host::RiscvVcpuHostIf;
    use riscv_vplic::host::RiscvVplicHostIf;

    use crate::host::{HostMemory, arceos::arceos_host};

    #[cfg(feature = "plat-dyn")]
    const GUEST_PLIC_PADDR: usize = 0x0c00_0000;

    struct RiscvVcpuHostIfImpl;

    #[impl_interface]
    impl RiscvVcpuHostIf for RiscvVcpuHostIfImpl {
        fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
            arceos_host().virt_to_phys(vaddr)
        }
    }

    struct RiscvVplicHostIfImpl;

    #[impl_interface]
    impl RiscvVplicHostIf for RiscvVplicHostIfImpl {
        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            arceos_host().phys_to_virt(paddr)
        }
    }

    #[cfg(feature = "plat-dyn")]
    pub(crate) fn register_platform_irq_injector() {
        axplat_dyn::register_virtual_irq_injector(inject_virtual_irq);
    }

    #[cfg(not(feature = "plat-dyn"))]
    pub(crate) fn register_platform_irq_injector() {}

    #[cfg(feature = "plat-dyn")]
    fn inject_virtual_irq(irq_id: usize) {
        debug!("injecting RISC-V virtual IRQ id: {irq_id}");

        let Some(vm_id) = crate::current_vm_id() else {
            warn!("cannot inject RISC-V virtual IRQ without current VM context");
            return;
        };

        let injected = crate::manager::with_vm(vm_id, |vm| {
            let Some(vplic) = vm
                .get_devices()
                .find_mmio_dev(GuestPhysAddr::from_usize(GUEST_PLIC_PADDR))
            else {
                warn!("VM[{vm_id}] has no virtual PLIC device");
                return;
            };

            let reg_offset = riscv_vplic::PLIC_PENDING_OFFSET + (irq_id / 32) * 4;
            let addr = GuestPhysAddr::from_usize(GUEST_PLIC_PADDR + reg_offset);
            let val: u32 = 1 << (irq_id % 32);

            if let Err(err) = vplic.handle_write(addr, AccessWidth::Dword, val as _) {
                warn!("failed to inject RISC-V virtual IRQ {irq_id}: {err:?}");
            }
        })
        .is_some();

        if !injected {
            warn!("cannot inject RISC-V virtual IRQ {irq_id}: VM[{vm_id}] not found");
        }
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

    use crate::host::{HostMemory, arceos::arceos_host};

    struct LoongArchVcpuHostIfImpl;

    #[impl_interface]
    impl LoongArchVcpuHostIf for LoongArchVcpuHostIfImpl {
        fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
            arceos_host().virt_to_phys(vaddr)
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use alloc::boxed::Box;
    use core::time::Duration;

    use arm_gic_driver::v3::{
        ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VTR_EL2, ReadWriteable, Readable,
        ich_lr_el2_get, ich_lr_el2_write,
    };
    use arm_vcpu::host::ArmVcpuHostIf;
    use arm_vgic::host::ArmVgicHostIf;
    use ax_crate_interface::impl_interface;
    use ax_memory_addr::{PhysAddr, VirtAddr};

    use crate::host::{HostCpu, HostMemory, HostTime, arceos::arceos_host};

    fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
        let mut gic = rdrive::get_one::<rdif_intc::Intc>()
            .expect("failed to get GIC driver")
            .lock()
            .expect("failed to lock GIC driver");
        f(&mut gic)
    }

    fn inject_interrupt(irq: usize) {
        debug!("Injecting virtual interrupt: {irq}");

        with_gic(|gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                use arm_gic_driver::{
                    IntId,
                    v2::{VirtualInterruptConfig, VirtualInterruptState},
                };

                let gich = gic.hypervisor_interface().expect("failed to get GICH");
                gich.enable();
                gich.set_virtual_interrupt(
                    0,
                    VirtualInterruptConfig::software(
                        unsafe { IntId::raw(irq as _) },
                        None,
                        0,
                        VirtualInterruptState::Pending,
                        false,
                        true,
                    ),
                );
                return;
            }

            if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
                inject_interrupt_gic_v3(irq);
                return;
            }

            panic!("no GIC driver found");
        });
    }

    fn inject_interrupt_gic_v3(vector: usize) {
        debug!("Injecting virtual interrupt: vector={vector}");
        let elsr = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS);
        let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;

        let mut free_lr = None;
        for i in 0..lr_num {
            if (1 << i) & elsr > 0 {
                free_lr.get_or_insert(i);
                continue;
            }

            let lr_val = ich_lr_el2_get(i);
            if lr_val.read(ICH_LR_EL2::VINTID) == vector as u64
                && lr_val.matches_any(&[ICH_LR_EL2::STATE::Pending, ICH_LR_EL2::STATE::Active])
            {
                debug!("Virtual interrupt {vector} already pending/active in LR{i}, skipping");
                return;
            }
        }

        let free_lr = free_lr
            .or_else(|| {
                (0..lr_num).find(|&i| ich_lr_el2_get(i).matches_all(ICH_LR_EL2::STATE::Invalid))
            })
            .unwrap_or_else(|| panic!("no free list register to inject IRQ {vector}"));

        ich_lr_el2_write(
            free_lr,
            ICH_LR_EL2::VINTID.val(vector as u64)
                + ICH_LR_EL2::STATE::Pending
                + ICH_LR_EL2::GROUP::SET,
        );

        if !ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN) {
            warn!("Virtual interrupt interface not enabled, enabling now");
            ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
        }

        debug!("Virtual interrupt {vector} injected successfully in LR{free_lr}");
    }

    fn read_gicd_iidr() -> u32 {
        with_gic(|gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                return gic.iidr_raw();
            }
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                return gic.iidr_raw();
            }
            panic!("no GIC driver found");
        })
    }

    fn read_gicd_typer() -> u32 {
        with_gic(|gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                return gic.typer_raw();
            }
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                return gic.typer_raw();
            }
            panic!("no GIC driver found");
        })
    }

    fn host_gicd_base() -> PhysAddr {
        with_gic(|gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                return arceos_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicd_addr())));
            }
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                return arceos_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicd_addr())));
            }
            panic!("no GIC driver found");
        })
    }

    fn host_gicr_base() -> PhysAddr {
        with_gic(|gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                return arceos_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicr_addr())));
            }
            panic!("no GICv3 driver found");
        })
    }

    fn fetch_irq() -> usize {
        with_gic(|gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                return u32::from(gic.cpu_interface().ack()) as usize;
            }
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                return gic.cpu_interface().ack1().to_u32() as usize;
            }
            panic!("no GIC driver found");
        })
    }

    struct ArmVcpuHostIfImpl;

    #[impl_interface]
    impl ArmVcpuHostIf for ArmVcpuHostIfImpl {
        fn hardware_inject_virtual_interrupt(vector: u8) {
            inject_interrupt(vector as usize);
        }

        fn fetch_irq() -> usize {
            fetch_irq()
        }

        fn handle_irq() {
            let _ = crate::host::arceos::handle_host_irq(0);
        }
    }

    struct ArmVgicHostIfImpl;

    #[impl_interface]
    impl ArmVgicHostIf for ArmVgicHostIfImpl {
        fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
            arceos_host().alloc_contiguous_frames(frame_count, frame_align)
        }

        fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
            arceos_host().dealloc_contiguous_frames(start_paddr, frame_count);
        }

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            arceos_host().phys_to_virt(paddr)
        }

        fn host_cpu_num() -> usize {
            arceos_host().cpu_count()
        }

        fn current_vcpu_id() -> usize {
            crate::current_vcpu_id().unwrap_or(0)
        }

        fn current_time_nanos() -> u64 {
            arceos_host().monotonic_time().as_nanos() as u64
        }

        fn register_timer(
            deadline: Duration,
            callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) {
            let _ = arceos_host().register_timer(deadline.as_nanos() as u64, callback);
        }

        fn read_vgicd_iidr() -> u32 {
            read_gicd_iidr()
        }

        fn read_vgicd_typer() -> u32 {
            read_gicd_typer()
        }

        fn get_host_gicd_base() -> PhysAddr {
            host_gicd_base()
        }

        fn get_host_gicr_base() -> PhysAddr {
            host_gicr_base()
        }

        fn hardware_inject_virtual_interrupt(vector: u8) {
            inject_interrupt(vector as usize);
        }
    }
}
