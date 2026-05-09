#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;

#[bare_test::tests]
mod tests {
    use alloc::vec::Vec;
    use bare_test::{
        globals::{PlatformInfoKind, global_val},
        irq::Phandle,
        mem::iomap,
        println,
    };
    use log::info;
    use nb::block;
    use num_align::NumAlign;
    use arm_scmi::{Scmi, Shmem, Smc};

    #[test]
    fn it_works() {
        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
        let fdt = fdt.get();
        let node = fdt
            .find_compatible(&["arm,scmi-smc"])
            .next()
            .expect("scmi not found");

        info!("found scmi node: {:?}", node.name());

        let shmem_ph: Phandle = node
            .find_property("shmem")
            .expect("shmem property not found")
            .u32()
            .into();

        let shmem_node = fdt
            .get_node_by_phandle(shmem_ph)
            .expect("shmem node not found");

        info!("found shmem node: {:?}", shmem_node.name());

        let shmem_reg = shmem_node.reg().unwrap().collect::<Vec<_>>();
        assert_eq!(shmem_reg.len(), 1);
        let shmem_reg = shmem_reg[0];
        let shmem_addr = iomap(
            (shmem_reg.address as usize).into(),
            shmem_reg.size.unwrap().align_up(0x1000),
        );

        let func_id = node
            .find_property("arm,smc-id")
            .expect("function-id property not found")
            .u32();

        info!("shmem reg: {:?}", shmem_reg);
        info!("func_id: {:#x}", func_id);

        let irq_num = node.find_property("a2p").map(|irq_prop| irq_prop.u32());

        let shmem = Shmem {
            address: shmem_addr,
            bus_address: shmem_reg.child_bus_address as usize,
            size: shmem_reg.size.unwrap(),
        };
        let kind = Smc::new(func_id, irq_num);
        let scmi = Scmi::new(kind, shmem);

        let mut pclk = scmi.protocol_clk();

        let ls = [
            (0u32, "clk0", 0x30a32c00),
            (2u32, "clk1", 0x30a32c00),
            (3u32, "clk2", 0x30a32c00),
        ];
        for (id, name, clk) in ls {
            pclk.clk_enable(id).unwrap();
            let rate = pclk.rate_get(id).unwrap();
            println!("Clock {} (id={}): rate={} Hz", name, id, rate);
            pclk.rate_set(id, clk).unwrap();
            let rate = pclk.rate_get(id).unwrap();
            println!("Clock {} (id={}): new rate={} Hz", name, id, rate);
        }

        println!("test passed!");
    }
}
