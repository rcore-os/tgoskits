#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;

#[bare_test::tests]
mod tests {
    use alloc::vec;
    use bare_test::{
        os::{
            mem::{dma::kernel_dma_op, ioremap, mmio::kernel_mmio_op, page_size},
            platform::{PlatformDescriptor, get_platform_descriptor},
        },
        *,
    };
    use core::ptr::NonNull;
    use fdt_parser::{Fdt, Node, PciSpace};
    use nvme_driver::{Config, Nvme, NvmeBlockDriver};
    use pcie::{
        CommandRegister, DeviceType, PciMem32, PciMem64, PcieController, PcieGeneric,
        enumerate_by_controller,
    };

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
    fn test_nvme_end_to_end() {
        println!("nvme discovery start");

        let mut nvme = get_nvme();

        println!("nvme init ok");

        let namespace_list = nvme.namespace_list().unwrap();

        println!("namespace count: {}", namespace_list.len());
        assert!(!namespace_list.is_empty(), "namespace list is empty");

        for ns in &namespace_list {
            println!(
                "namespace id={} lba_size={} lba_count={}",
                ns.id, ns.lba_size, ns.lba_count
            );
        }

        println!("namespace query ok");

        let ns = namespace_list[0];
        let mut block =
            NvmeBlockDriver::with_namespace("nvme", nvme, ns).into_block(kernel_dma_op());
        let mut queue = block.create_queue().unwrap();

        assert_eq!(queue.block_size(), ns.lba_size);
        assert_eq!(queue.num_blocks(), ns.lba_count);

        for block in 0..128 {
            let mut write_buf = vec![0u8; ns.lba_size];
            let message = alloc::format!("hello world! block {block}");
            let message_bytes = message.as_bytes();

            write_buf[..message_bytes.len()].copy_from_slice(message_bytes);

            let write_result = queue.write_blocks_blocking(block, &write_buf);
            assert!(write_result.into_iter().all(|entry| entry.is_ok()));

            let mut read_result = queue.read_blocks_blocking(block, 1).into_iter();
            let read_buf = read_result.next().unwrap().unwrap();

            assert_eq!(&read_buf[..message_bytes.len()], message_bytes);

            if block == 0 || block == 127 {
                println!("block {} io ok", block);
            }
        }

        println!("nvme io ok");
    }

    fn get_nvme() -> Nvme {
        let PlatformDescriptor::DeviceTree(dtb) = get_platform_descriptor() else {
            panic!("device tree not found");
        };
        let fdt = Fdt::from_bytes(dtb.as_slice()).unwrap();
        let pcie = match fdt
            .find_compatible(&["pci-host-ecam-generic"])
            .into_iter()
            .next()
            .unwrap()
        {
            Node::Pci(pci) => pci,
            _ => panic!("pci host bridge not found"),
        };

        println!("pcie: {}", pcie.name());

        let mut pcie_regs = vec![];

        for reg in pcie.reg().unwrap() {
            println!("pcie reg: {:#x}", reg.address);
            let reg_size = reg.size.expect("pcie reg size missing");
            pcie_regs.push(ioremap((reg.address as usize).into(), reg_size).unwrap());
        }

        let base_vaddr = pcie_regs[0];
        let base_vaddr = NonNull::new(base_vaddr.raw() as *mut u8).unwrap();

        let mut controller = PcieController::new(PcieGeneric::new(base_vaddr));

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

                return Nvme::new(
                    bar.start as u64,
                    bar.count(),
                    u64::MAX,
                    kernel_dma_op(),
                    kernel_mmio_op(),
                    Config {
                        page_size,
                        io_queue_pair_count: 1,
                    },
                )
                .unwrap();
            }
        }

        panic!("no nvme found");
    }
}
