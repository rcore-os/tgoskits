#[macro_use]
extern crate axconfig_macros;

mod config {
    include_configs!("../example-configs/defconfig.toml"); // root: CARGO_MANIFEST_DIR
}

#[cfg(feature = "nightly")]
mod config2 {
    parse_configs!(include_str!("../../example-configs/defconfig.toml"));
}

mod config_expect {
    include!("../../example-configs/output.rs");
}

macro_rules! mod_cmp {
    ($mod1:ident, $mod2:ident) => {
        assert_eq!($mod1::ARCH, $mod2::ARCH);
        assert_eq!($mod1::PLAT, $mod2::PLAT);
        assert_eq!($mod1::SMP, $mod2::SMP);

        assert_eq!($mod1::devices::MMIO_REGIONS, $mod2::devices::MMIO_REGIONS);
        assert_eq!($mod1::devices::PCI_BUS_END, $mod2::devices::PCI_BUS_END);
        assert_eq!($mod1::devices::PCI_ECAM_BASE, $mod2::devices::PCI_ECAM_BASE);
        assert_eq!($mod1::devices::PCI_RANGES, $mod2::devices::PCI_RANGES);
        assert_eq!(
            $mod1::devices::VIRTIO_MMIO_REGIONS,
            $mod2::devices::VIRTIO_MMIO_REGIONS
        );

        assert_eq!(
            $mod1::kernel::TASK_STACK_SIZE,
            $mod2::kernel::TASK_STACK_SIZE
        );
        assert_eq!($mod1::kernel::TICKS_PER_SEC, $mod2::kernel::TICKS_PER_SEC);

        assert_eq!(
            $mod1::platform::KERNEL_ASPACE_BASE,
            $mod2::platform::KERNEL_ASPACE_BASE
        );
        assert_eq!(
            $mod1::platform::KERNEL_ASPACE_SIZE,
            $mod2::platform::KERNEL_ASPACE_SIZE
        );
        assert_eq!(
            $mod1::platform::KERNEL_BASE_PADDR,
            $mod2::platform::KERNEL_BASE_PADDR
        );
        assert_eq!(
            $mod1::platform::KERNEL_BASE_VADDR,
            $mod2::platform::KERNEL_BASE_VADDR
        );
        assert_eq!(
            $mod1::platform::PHYS_BUS_OFFSET,
            $mod2::platform::PHYS_BUS_OFFSET
        );
        assert_eq!(
            $mod1::platform::PHYS_MEMORY_BASE,
            $mod2::platform::PHYS_MEMORY_BASE
        );
        assert_eq!(
            $mod1::platform::PHYS_MEMORY_SIZE,
            $mod2::platform::PHYS_MEMORY_SIZE
        );
        assert_eq!(
            $mod1::platform::PHYS_VIRT_OFFSET,
            $mod2::platform::PHYS_VIRT_OFFSET
        );
        assert_eq!(
            $mod1::platform::TIMER_FREQUENCY,
            $mod2::platform::TIMER_FREQUENCY
        );
    };
}

#[test]
fn test_include_configs() {
    mod_cmp!(config, config_expect);
}

#[cfg(feature = "nightly")]
#[test]
fn test_parse_configs() {
    mod_cmp!(config2, config_expect);
}
