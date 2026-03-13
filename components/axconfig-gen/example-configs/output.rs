/// Architecture identifier.
pub const ARCH: &str = "x86_64";
/// Platform identifier.
pub const PLAT: &str = "x86_64-qemu-q35";
/// Number of CPUs.
pub const SMP: usize = 1;

///
/// Device specifications
///
pub mod devices {
    /// MMIO regions with format (`base_paddr`, `size`).
    pub const MMIO_REGIONS: &[(usize, usize)] = &[
        (0xb000_0000, 0x1000_0000),
        (0xfe00_0000, 0xc0_0000),
        (0xfec0_0000, 0x1000),
        (0xfed0_0000, 0x1000),
        (0xfee0_0000, 0x1000),
    ];
    /// End PCI bus number.
    pub const PCI_BUS_END: usize = 0;
    /// Base physical address of the PCIe ECAM space (should read from ACPI 'MCFG' table).
    pub const PCI_ECAM_BASE: usize = 0;
    /// PCI device memory ranges (not used on x86).
    pub const PCI_RANGES: &[(usize, usize)] = &[];
    /// VirtIO MMIO regions with format (`base_paddr`, `size`).
    pub const VIRTIO_MMIO_REGIONS: &[(usize, usize)] = &[];
}

///
/// Kernel configs
///
pub mod kernel {
    /// Stack size of each task.
    pub const TASK_STACK_SIZE: usize = 0;
    /// Number of timer ticks per second (Hz). A timer tick may contain several timer
    /// interrupts.
    pub const TICKS_PER_SEC: usize = 0;
}

///
/// Platform configs
///
pub mod platform {
    /// Kernel address space base.
    pub const KERNEL_ASPACE_BASE: usize = 0xffff_ff80_0000_0000;
    /// Kernel address space size.
    pub const KERNEL_ASPACE_SIZE: usize = 0x0000_007f_ffff_f000;
    /// Base physical address of the kernel image.
    pub const KERNEL_BASE_PADDR: usize = 0x20_0000;
    /// Base virtual address of the kernel image.
    pub const KERNEL_BASE_VADDR: usize = 0xffff_ff80_0020_0000;
    /// Offset of bus address and phys address. some boards, the bus address is
    /// different from the physical address.
    pub const PHYS_BUS_OFFSET: usize = 0;
    /// Base address of the whole physical memory.
    pub const PHYS_MEMORY_BASE: usize = 0;
    /// Size of the whole physical memory.
    pub const PHYS_MEMORY_SIZE: usize = 0x800_0000;
    /// Linear mapping offset, for quick conversions between physical and virtual
    /// addresses.
    pub const PHYS_VIRT_OFFSET: usize = 0xffff_ff80_0000_0000;
    /// Timer interrupt frequencyin Hz.
    pub const TIMER_FREQUENCY: usize = 0;
}
