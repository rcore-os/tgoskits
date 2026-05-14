#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate bare_test;

#[bare_test::tests]
mod tests {
    use bare_test::{
        os::{
            mem::mmio::kernel_mmio_op,
            platform::{PlatformDescriptor, get_platform_descriptor},
        },
        *,
    };
    use fdt_edit::{Fdt, NodeType, PciSpace};
    use log::info;
    use pcie::{
        CommandRegister, PciMem32, PciMem64, PcieController, PcieGeneric, enumerate_by_controller,
    };

    #[test]
    fn test_framework_boot() {
        println!("pcie bare-test bootstrap ok");
    }

    #[test]
    #[timeout = 10000]
    fn test_iter() {
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

        println!("pcie discovery start");

        println!("pcie: {}", pcie_name);

        let reg = pcie.regs().into_iter().next().expect("pcie reg missing");
        let reg_size = reg.size.expect("pcie reg size missing");

        info!("Init PCIE @phys={:#x}, size={:#x}", reg.address, reg_size);

        let i =
            PcieGeneric::new(reg.address as usize, reg_size as usize, kernel_mmio_op()).unwrap();
        let mut drv = PcieController::new(i);

        for range in pcie.ranges().unwrap() {
            info!("{range:?}");
            match range.space {
                PciSpace::Memory32 => {
                    drv.set_mem32(
                        PciMem32 {
                            address: range.cpu_address as _,
                            size: range.size as _,
                        },
                        range.prefetchable,
                    );
                }
                PciSpace::Memory64 => {
                    drv.set_mem64(
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

        for mut ep in enumerate_by_controller(&mut drv, None) {
            println!("{}", ep);
            println!("  BARs:");
            for i in 0..6 {
                if let Some(bar) = ep.bar(i) {
                    println!("    BAR{}: {:x?}", i, bar);
                }
            }
            for cap in ep.capabilities() {
                println!("  {:?}", cap);
            }

            ep.update_command(|mut cmd| {
                cmd.insert(CommandRegister::MEMORY_ENABLE);
                cmd
            });
        }

        println!("test passed!");
    }
}
