#[cfg(target_arch = "aarch64")]
mod arch {
    pub const ARCH: &str = "aarch64";
    pub const PACKAGE: &str = "axplat-aarch64-generic";
    pub const PLATFORM: &str = "aarch64-generic";
    pub const TIMER_IRQ: usize = 0xf0;
    pub const IPI_IRQ: usize = 0;
    pub const KERNEL_ASPACE_BASE: usize = 0xffff_8000_0000_0000;
    pub const KERNEL_ASPACE_SIZE: usize = 0x0000_7fff_ffff_f000;
    pub const KERNEL_BASE_PADDR: usize = 0x20_0000;
    pub const KERNEL_BASE_VADDR: usize = 0xffff_8000_0020_0000;
}

#[cfg(target_arch = "riscv64")]
mod arch {
    pub const ARCH: &str = "riscv64";
    pub const PACKAGE: &str = "axplat-dyn";
    pub const PLATFORM: &str = "riscv64-plat-dyn";
    const INTERRUPT_FLAG: usize = 1usize << (usize::BITS as usize - 1);
    pub const TIMER_IRQ: usize = INTERRUPT_FLAG | 5;
    pub const IPI_IRQ: usize = INTERRUPT_FLAG | 1;
    pub const KERNEL_ASPACE_BASE: usize = 0xffff_ffc0_0000_0000;
    pub const KERNEL_ASPACE_SIZE: usize = 0x0000_003f_ffff_f000;
    pub const KERNEL_BASE_PADDR: usize = 0x8020_0000;
    pub const KERNEL_BASE_VADDR: usize = 0xffff_ffff_8000_0000;
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
mod arch {
    pub const ARCH: &str = "aarch64";
    pub const PACKAGE: &str = "axplat-aarch64-generic";
    pub const PLATFORM: &str = "aarch64-generic";
    pub const TIMER_IRQ: usize = 0xf0;
    pub const IPI_IRQ: usize = 0;
    pub const KERNEL_ASPACE_BASE: usize = 0xffff_8000_0000_0000;
    pub const KERNEL_ASPACE_SIZE: usize = 0x0000_7fff_ffff_f000;
    pub const KERNEL_BASE_PADDR: usize = 0x20_0000;
    pub const KERNEL_BASE_VADDR: usize = 0xffff_8000_0020_0000;
}

#[doc = " Architecture identifier."]
pub const ARCH: &str = arch::ARCH;
#[doc = " Platform package."]
pub const PACKAGE: &str = arch::PACKAGE;
#[doc = " Platform identifier."]
pub const PLATFORM: &str = arch::PLATFORM;
#[doc = " Stack size of each task."]
pub const TASK_STACK_SIZE: usize = 0x40000;
#[doc = " Number of timer ticks per second (Hz). A timer tick may contain several timer"]
#[doc = " interrupts."]
pub const TICKS_PER_SEC: usize = 100;
#[doc = ""]
#[doc = " Device specifications"]
#[doc = ""]
pub mod devices {
    #[doc = " MMIO regions with format (`base_paddr`, `size`)."]
    pub const MMIO_REGIONS: &[(usize, usize)] = &[
        (0xb000_0000, 0x1000_0000), // PCI config space
        (0xfe00_0000, 0xc0_0000),   // PCI devices
        (0xfec0_0000, 0x1000),      // IO APIC
        (0xfed0_0000, 0x1000),      // HPET
        (0xfee0_0000, 0x1000),      // Local APIC
    ];
    #[doc = " End PCI bus number."]
    pub const PCI_BUS_END: usize = 0xff;
    #[doc = " Base physical address of the PCIe ECAM space."]
    pub const PCI_ECAM_BASE: usize = 0xb000_0000;
    #[doc = " PCI device memory ranges."]
    pub const PCI_RANGES: &[(usize, usize)] = &[];
    #[doc = " Timer interrupt num (PPI, physical timer)."]
    pub const TIMER_IRQ: usize = super::arch::TIMER_IRQ;
    #[doc = " IPI interrupt num."]
    pub const IPI_IRQ: usize = super::arch::IPI_IRQ;
    #[doc = " VirtIO MMIO regions with format (`base_paddr`, `size`)."]
    pub const VIRTIO_MMIO_REGIONS: &[(usize, usize)] = &[];
    #[doc = " SDMMC controller physical address."]
    pub const SDMMC_PADDR: usize = 0;
    #[doc = " SG2002 CV SD/MMC controller physical address."]
    pub const CVSD_PADDR: usize = 0;
    #[doc = " SG2002 system controller physical address."]
    pub const SYSCON_PADDR: usize = 0;
}
#[doc = ""]
#[doc = " Platform configs"]
#[doc = ""]
pub mod plat {
    #[doc = " Number of CPUs."]
    pub const MAX_CPU_NUM: usize = {
        match option_env!("SMP") {
            Some(s) => const_str::parse!(s, usize),
            None => 16,
        }
    };
    #[doc = " Platform family (deprecated)."]
    pub const FAMILY: &str = "";
    #[doc = " Kernel address space base."]
    pub const KERNEL_ASPACE_BASE: usize = super::arch::KERNEL_ASPACE_BASE;
    #[doc = " Kernel address space size."]
    pub const KERNEL_ASPACE_SIZE: usize = super::arch::KERNEL_ASPACE_SIZE;
    #[doc = " No need."]
    pub const KERNEL_BASE_PADDR: usize = super::arch::KERNEL_BASE_PADDR;
    #[doc = " Base virtual address of the kernel image."]
    pub const KERNEL_BASE_VADDR: usize = super::arch::KERNEL_BASE_VADDR;
    #[doc = " Offset of bus address and phys address. some boards, the bus address is"]
    #[doc = " different from the physical address."]
    pub const PHYS_BUS_OFFSET: usize = 0;
    #[doc = " No need."]
    pub const PHYS_MEMORY_BASE: usize = 0;
    #[doc = " No need."]
    pub const PHYS_MEMORY_SIZE: usize = 0x0;
    #[doc = " No need."]
    pub const PHYS_VIRT_OFFSET: usize = 0;
}
