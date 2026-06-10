#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate bare_test;

#[bare_test::tests]
mod tests {
    use bare_test::{
        os::{
            mem::{dma::kernel_dma_op, mmio::kernel_mmio_op},
            platform::{PlatformDescriptor, get_platform_descriptor},
        },
        *,
    };
    use eth_intel::E1000;
    use fdt_edit::{Fdt, NodeType, PciSpace};
    use pcie::{
        CommandRegister, PciMem32, PciMem64, PcieController, PcieGeneric, enumerate_by_controller,
    };

    #[test]
    #[timeout = 10000]
    fn ping_test() {
        println!("ping_test: e1000 discovery start");
        let nic = get_e1000().expect("no e1000 found on pci bus");
        bare_test::net::ping::run_ping_test(nic);
        println!("ping_test: completed");
    }

    fn get_e1000() -> Option<E1000> {
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

        for mut ep in enumerate_by_controller(&mut controller, None) {
            println!("{}", ep);
            if E1000::check_vid_did(ep.vendor_id(), ep.device_id()) {
                let bar = ep.bar_mmio(0).expect("bar0");
                ep.update_command(|mut cmd| {
                    cmd.insert(CommandRegister::BUS_MASTER_ENABLE | CommandRegister::MEMORY_ENABLE);
                    cmd
                });

                return Some(
                    E1000::new(
                        bar.start as u64,
                        bar.count(),
                        u64::MAX,
                        kernel_dma_op(),
                        kernel_mmio_op(),
                    )
                    .expect("create e1000"),
                );
            }
        }

        None
    }
}
