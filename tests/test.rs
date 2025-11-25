#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;
#[macro_use]
extern crate log;

use rockchip_pm::*;

#[bare_test::tests]
mod tests {

    use core::ptr::NonNull;

    use super::*;
    use alloc::vec::Vec;
    use bare_test::{
        globals::{PlatformInfoKind, global_val},
        mem::{iomap, page_size},
    };
    // RK3588 NPU 相关电源域 ID

    /// NPU 主电源域
    pub const NPU: PD = PD(8);
    /// NPU TOP 电源域  
    pub const NPUTOP: PD = PD(9);
    /// NPU1 电源域
    pub const NPU1: PD = PD(10);
    /// NPU2 电源域
    pub const NPU2: PD = PD(11);

    #[test]
    fn test_pm() {
        let reg = get_syscon_addr();
        let board = RkBoard::Rk3588;

        let mut pm = RockchipPM::new(reg, board);

        let npu = get_npu_info();

        pm.power_domain_on(NPUTOP).unwrap();
        pm.power_domain_on(NPU).unwrap();
        pm.power_domain_on(NPU1).unwrap();
        pm.power_domain_on(NPU2).unwrap();

        unsafe {
            let ptr = npu.base.as_ptr() as *mut u32;
            let version = ptr.read_volatile();
            info!("NPU Version: {version:#x}");
        }
    }

    struct NpuInfo {
        base: NonNull<u8>,
        domains: Vec<PD>,
    }

    fn get_npu_info() -> NpuInfo {
        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
        let fdt = fdt.get();

        let node = fdt
            .find_compatible(&["rockchip,rk3588-rknpu"])
            .next()
            .expect("Failed to find npu0 node");

        info!("Found node: {}", node.name());

        let regs = node.reg().unwrap().collect::<Vec<_>>();
        let start = regs[0].address as usize;
        let end = start + regs[0].size.unwrap_or(0);
        info!("NPU0 address range: 0x{:x} - 0x{:x}", start, end);
        let start = start & !(page_size() - 1);
        let end = (end + page_size() - 1) & !(page_size() - 1);
        info!("Aligned NPU0 address range: 0x{:x} - 0x{:x}", start, end);
        let base = iomap(start.into(), end - start);

        let mut domains = Vec::new();

        let pd_prop = node
            .find_property("power-domains")
            .expect("Failed to find power-domains property");
        let pd_ls = pd_prop.u32_list().collect::<Vec<_>>();
        for pd in pd_ls.chunks(2) {
            let phandle = pd[0];
            let pd = PD::from(pd[1]);
            let pm_node = node
                .fdt()
                .get_node_by_phandle(phandle.into())
                .expect("Failed to find power domain node");
            info!("Found power domain node: {}", pm_node.name());

            domains.push(pd);
        }

        NpuInfo { base, domains }
    }

    fn get_syscon_addr() -> NonNull<u8> {
        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
        let fdt = fdt.get();

        let node = fdt
            .find_compatible(&["syscon"])
            .find(|n| n.name().contains("power-manage"))
            .expect("Failed to find syscon node");

        info!("Found node: {}", node.name());

        let regs = node.reg().unwrap().collect::<Vec<_>>();
        let start = regs[0].address as usize;
        let end = start + regs[0].size.unwrap_or(0);
        info!("Syscon address range: 0x{:x} - 0x{:x}", start, end);
        let start = start & !(page_size() - 1);
        let end = (end + page_size() - 1) & !(page_size() - 1);
        info!("Aligned Syscon address range: 0x{:x} - 0x{:x}", start, end);
        iomap(start.into(), end - start)
    }
}
