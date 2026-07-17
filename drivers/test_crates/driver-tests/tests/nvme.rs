#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;

#[bare_test::tests]
mod tests {
    use bare_test::{
        os::{
            mem::{dma::kernel_dma_op, mmio::kernel_mmio_op, page_size},
            platform::{PlatformDescriptor, get_platform_descriptor},
        },
        *,
    };
    use fdt_edit::{Fdt, NodeType, PciSpace};
    use nvme_driver::{Config, NvmeBlockDriver};
    use pcie::{
        CommandRegister, DeviceType, PciMem32, PciMem64, PcieController, PcieGeneric,
        enumerate_by_controller,
    };
    use rdif_block::{ControllerInitEndpoint, Interface};

    #[test]
    fn test_framework_boot() {
        println!("nvme bare-test bootstrap ok");
    }

    #[test]
    #[timeout = 100]
    fn test_framework_timeout_path() {
        println!("nvme bare-test timeout guard ok");
    }

    #[test]
    #[timeout = 10000]
    fn test_nvme_discovery_and_queue_contract() {
        println!("nvme discovery start");

        let mut block = discover_nvme();

        assert!(block.namespace_if_ready().is_none());
        let ControllerInitEndpoint::Pending(initializer) = block.controller_init() else {
            panic!("hardware discovery must return a pending initializer")
        };
        assert!(
            !initializer.irq_sources().is_empty(),
            "NVMe initialization requires an IRQ action"
        );
        assert!(
            block.create_queue().is_none(),
            "normal queues must not be published before initialization"
        );

        // Driver-only bare tests do not own an IRQ runtime. Data I/O belongs in
        // the Starry/ArceOS runtime tests, where the queue is activated only
        // after its IRQ action and shared worker are installed.
        println!("nvme IRQ queue contract ok");
    }

    fn discover_nvme() -> NvmeBlockDriver {
        let PlatformDescriptor::DeviceTree(dtb) = get_platform_descriptor() else {
            panic!("device tree not found");
        };
        let fdt = Fdt::from_bytes(dtb.as_slice()).unwrap();
        let (pcie_name, pcie) = match fdt
            .find_compatible(&["pci-host-ecam-generic"])
            .into_iter()
            .next()
            .unwrap()
        {
            node @ NodeType::Pci(_) => {
                let name = node.name();
                match node {
                    NodeType::Pci(pci) => (name, pci),
                    _ => unreachable!(),
                }
            }
            _ => panic!("pci host bridge not found"),
        };

        println!("pcie: {}", pcie_name);

        let reg = pcie.regs().into_iter().next().expect("pcie reg missing");
        let reg_size = reg.size.expect("pcie reg size missing");
        println!("pcie reg: {:#x}", reg.address);

        let mut controller = PcieController::new(
            PcieGeneric::new(reg.address as usize, reg_size as usize, kernel_mmio_op()).unwrap(),
        );

        for range in pcie.ranges().unwrap() {
            match range.space {
                PciSpace::Memory32 => {
                    controller.set_mem32(
                        PciMem32 {
                            address: range.cpu_address as _,
                            size: range.size as _,
                        },
                        range.prefetchable,
                    );
                }
                PciSpace::Memory64 => {
                    controller.set_mem64(
                        PciMem64 {
                            address: range.cpu_address as _,
                            size: range.size as _,
                        },
                        range.prefetchable,
                    );
                }
                _ => {}
            }
        }

        let page_size = page_size();

        for mut ep in enumerate_by_controller(&mut controller, None) {
            println!("{}", ep);
            if ep.device_type() == DeviceType::NvmeController {
                let bar = ep.bar_mmio(0).unwrap();
                println!("bar0: [{:#x}, {:#x})", bar.start, bar.end);
                println!("nvme discovery ok");

                ep.update_command(|mut cmd| {
                    cmd.insert(CommandRegister::BUS_MASTER_ENABLE | CommandRegister::MEMORY_ENABLE);
                    cmd
                });

                return NvmeBlockDriver::discover(
                    "nvme",
                    bar.start as u64,
                    bar.count(),
                    u64::MAX,
                    kernel_dma_op(),
                    kernel_mmio_op(),
                    Config::new(page_size, 1).with_intx_irq(),
                )
                .unwrap();
            }
        }

        panic!("no nvme found");
    }
}
