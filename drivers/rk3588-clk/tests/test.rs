//! Integration tests for RK3588 CRU driver
//!
//! This module contains tests for verifying clock and reset unit functionality on the RK3588 SoC.
//! Tests are designed to run on real hardware or QEMU using the bare-test framework.

#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;

#[bare_test::tests]
mod tests {
    use alloc::{boxed::Box, vec::Vec};
    use bare_test::{
        globals::{PlatformInfoKind, global_val},
        mem::{iomap, page_size},
        println,
        time::since_boot,
    };
    use log::{info, warn};
    use rk3588_clk::{Rk3588Cru, constant::*};
    use rockchip_pm::PowerDomain;
    use sdmmc::emmc::EMmcHost;
    use sdmmc::{
        Kernel,
        emmc::clock::{Clk, ClkError, init_global_clk},
        set_impl,
    };

    use core::ptr::NonNull;
    use rockchip_pm::{RkBoard, RockchipPM};

    /// NPU 主电源域 (dt-binding 索引 8)
    pub const NPU: PowerDomain = PowerDomain(8);
    /// NPU TOP 电源域 (索引 9)
    pub const NPUTOP: PowerDomain = PowerDomain(9);
    /// NPU1 电源域 (索引 10)
    pub const NPU1: PowerDomain = PowerDomain(10);
    /// NPU2 电源域 (索引 11)
    pub const NPU2: PowerDomain = PowerDomain(11);

    /// Kernel implementation for bare-test framework
    struct SKernel;

    impl Kernel for SKernel {
        fn sleep(us: u64) {
            let start = since_boot();
            let duration = core::time::Duration::from_micros(us);

            while since_boot() - start < duration {
                core::hint::spin_loop();
            }
        }
    }

    set_impl!(SKernel);

    /// Main platform test that initializes and tests all peripherals
    #[test]
    fn test_platform() {
        let emmc_addr_ptr = get_device_addr("rockchip,dwcmshc-sdhci");
        let clk_add_ptr = get_device_addr("rockchip,rk3588-cru");
        let sys_npu_grf_ptr = get_device_addr("rockchip,rk3588-npu-grf");
        let npu_addr_ptr = get_device_addr("rockchip,rk3588-rknpu");
        let pmu_addr_ptr = get_device_addr("rockchip,rk3588-pmu");

        info!("emmc ptr: {:p}", emmc_addr_ptr);
        info!("clk ptr: {:p}", clk_add_ptr);
        info!("npu grf ptr: {:p}", sys_npu_grf_ptr);
        info!("npu ptr: {:p}", npu_addr_ptr);
        info!("pmu ptr: {:p}", pmu_addr_ptr);

        let emmc_addr = emmc_addr_ptr.as_ptr() as usize;
        let clk_addr = clk_add_ptr.as_ptr() as usize;
        let sys_npu_grf = sys_npu_grf_ptr.as_ptr() as usize;
        let npu_addr = npu_addr_ptr.as_ptr() as usize;
        let pmu_addr = pmu_addr_ptr.as_ptr() as usize;

        info!("emmc addr: {:#x}", emmc_addr);
        info!("clk addr: {:#x}", clk_addr);
        info!("npu grf addr: {:#x}", sys_npu_grf);
        info!("npu addr: {:#x}", npu_addr);
        info!("pmu addr: {:#x}", pmu_addr);

        test_pm(pmu_addr_ptr);

        test_emmc(emmc_addr, clk_addr);

        let npu = unsafe { core::ptr::read_volatile(sys_npu_grf as *const u32) };
        println!("npu version: {:#x}", npu);

        test_npu_cru(npu_addr, clk_addr);

        info!("test uboot");
    }

    /// Clock unit wrapper that implements the sdmmc Clk trait
    pub struct ClkUnit(Rk3588Cru);

    impl ClkUnit {
        /// Create a new clock unit from a CRU instance
        pub fn new(cru: Rk3588Cru) -> Self {
            ClkUnit(cru)
        }
    }

    impl Clk for ClkUnit {
        fn emmc_get_clk(&self) -> Result<u64, ClkError> {
            if let Ok(rate) = self.0.mmc_get_clk(CCLK_EMMC) {
                Ok(rate as u64)
            } else {
                Err(ClkError::InvalidClockRate)
            }
        }

        fn emmc_set_clk(&self, rate: u64) -> Result<u64, ClkError> {
            if let Ok(rate) = self.0.mmc_set_clk(CCLK_EMMC, rate as usize) {
                Ok(rate as u64)
            } else {
                Err(ClkError::InvalidClockRate)
            }
        }
    }

    fn init_clk(clk_addr: usize) -> Result<(), ClkError> {
        let cru = ClkUnit::new(Rk3588Cru::new(
            core::ptr::NonNull::new(clk_addr as *mut u8).unwrap(),
        ));

        let static_clk: &'static dyn Clk = Box::leak(Box::new(cru));
        init_global_clk(static_clk);
        Ok(())
    }

    /// Test eMMC functionality including initialization, reading, writing, and verification
    fn test_emmc(emmc_addr: usize, clock: usize) {
        // Initialize custom SDHCI controller
        let mut emmc = EMmcHost::new(emmc_addr);
        let _ = init_clk(clock);

        // Try to initialize the SD card
        match emmc.init() {
            Ok(_) => {
                println!("SD card initialization successful!");

                // Get card information
                match emmc.get_card_info() {
                    Ok(card_info) => {
                        println!("Card type: {:?}", card_info.card_type);
                        println!("Manufacturer ID: 0x{:02X}", card_info.manufacturer_id);
                        println!("Capacity: {} MB", card_info.capacity_bytes / (1024 * 1024));
                        println!("Block size: {} bytes", card_info.block_size);
                    }
                    Err(e) => {
                        warn!("Failed to get card info: {:?}", e);
                    }
                }

                // Test reading the first block
                println!("Attempting to read first block...");
                let mut buffer: [u8; 512] = [0; 512];

                match emmc.read_blocks(5034498, 1, &mut buffer) {
                    Ok(_) => {
                        println!("Successfully read first block!");
                        let block_bytes: Vec<u8> = (0..512).map(|i| buffer[i]).collect();
                        println!("First 16 bytes of first block: {:02X?}", block_bytes);
                    }
                    Err(e) => {
                        warn!("Block read failed: {:?}", e);
                    }
                }

                // Test writing and reading back a block
                println!("Testing write and read back...");
                let test_block_id = 0x3; // Use a safe block address for testing

                let mut write_buffer: [u8; 512] = [0; 512];
                for i in 0..512 {
                    // write_buffer[i] = (i % 256) as u8; // Fill with test pattern data
                    write_buffer[i] = 0 as u8;
                }

                // Write data
                match emmc.write_blocks(test_block_id, 1, &write_buffer) {
                    Ok(_) => {
                        println!("Successfully wrote to block {}!", test_block_id);

                        // Read back data
                        let mut read_buffer: [u8; 512] = [0; 512];

                        match emmc.read_blocks(test_block_id, 1, &mut read_buffer) {
                            Ok(_) => {
                                println!("Successfully read back block {}!", test_block_id);

                                // Verify data consistency
                                let mut data_match = true;
                                for i in 0..512 {
                                    if write_buffer[i] != read_buffer[i] {
                                        data_match = false;
                                        println!(
                                            "Data mismatch: offset {}, wrote {:02X}, read {:02X}",
                                            i, write_buffer[i], read_buffer[i]
                                        );
                                        break;
                                    }
                                }

                                println!(
                                    "First 16 bytes of read block: {:?}",
                                    read_buffer.to_vec()
                                );

                                if data_match {
                                    println!(
                                        "Data verification successful: written and read data match perfectly!"
                                    );
                                } else {
                                    println!(
                                        "Data verification failed: written and read data do not match!"
                                    );
                                }
                            }
                            Err(e) => {
                                warn!("Failed to read back block: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Block write failed: {:?}", e);
                    }
                }

                // Test multi-block read
                println!("Testing multi-block read...");
                let multi_block_addr = 200;
                let block_count = 4; // Read 4 blocks

                // Using a fixed size of 2048 (which is 512 * 4) instead of computing it at runtime
                let mut multi_buffer: [u8; 2048] = [0; 2048];

                match emmc.read_blocks(multi_block_addr, block_count, &mut multi_buffer) {
                    Ok(_) => {
                        println!(
                            "Successfully read {} blocks starting at block address {}!",
                            block_count, multi_block_addr
                        );

                        let first_block_bytes: Vec<u8> = (0..16).map(|i| multi_buffer[i]).collect();
                        println!("First 16 bytes of first block: {:02X?}", first_block_bytes);

                        let last_block_offset = (block_count as usize - 1) * 512;
                        let last_block_bytes: Vec<u8> = (0..16)
                            .map(|i| multi_buffer[last_block_offset + i])
                            .collect();
                        println!("First 16 bytes of last block: {:02X?}", last_block_bytes);
                    }
                    Err(e) => {
                        warn!("Multi-block read failed: {:?}", e);
                    }
                }
            }
            Err(e) => {
                warn!("SD card initialization failed: {:?}", e);
            }
        }

        // Test complete
        println!("SD card test complete");
    }

    /// Test power management for NPU power domains
    fn test_pm(reg: NonNull<u8>) {
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

    /// NPU information including base address and power domains
    struct NpuInfo {
        base: NonNull<u8>,
        #[allow(dead_code)]
        domains: Vec<PowerDomain>,
    }

    /// Extract NPU information from device tree
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
            let pd = PowerDomain::from(pd[1]);
            let pm_node = node
                .fdt()
                .get_node_by_phandle(phandle.into())
                .expect("Failed to find power domain node");
            info!("Found power domain node: {}", pm_node.name());

            domains.push(pd);
        }

        NpuInfo { base, domains }
    }

    /// Get device address from device tree by compatible string
    fn get_device_addr(dtc_str: &str) -> NonNull<u8> {
        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
        let fdt = fdt.get();

        let binding = [dtc_str];
        let node = fdt
            .find_compatible(&binding)
            .next()
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

    /// Test NPU clock gates by enabling various NPU clocks
    fn test_npu_cru(npu_addr: usize, clock: usize) {
        info!("test npu cru");

        let cru = ClkUnit::new(Rk3588Cru::new(
            core::ptr::NonNull::new(clock as *mut u8).unwrap(),
        ));

        let aclk_npu0 = cru.0.npu_gate_enable(ACLK_NPU0).unwrap();
        println!("npu gate enable: {}", aclk_npu0);
        let hclk_npu0 = cru.0.npu_gate_enable(HCLK_NPU0).unwrap();
        println!("npu gate enable: {}", hclk_npu0);
        let aclk_npu1 = cru.0.npu_gate_enable(ACLK_NPU1).unwrap();
        println!("npu gate enable: {}", aclk_npu1);
        let hclk_npu1 = cru.0.npu_gate_enable(HCLK_NPU1).unwrap();
        println!("npu gate enable: {}", hclk_npu1);
        let aclk_npu2 = cru.0.npu_gate_enable(ACLK_NPU2).unwrap();
        println!("npu gate enable: {}", aclk_npu2);
        let hclk_npu2 = cru.0.npu_gate_enable(HCLK_NPU2).unwrap();
        println!("npu gate enable: {}", hclk_npu2);
        let pclk_npu_grf = cru.0.npu_gate_enable(PCLK_NPU_GRF).unwrap();
        println!("npu gate enable: {}", pclk_npu_grf);

        let npu = unsafe { core::ptr::read_volatile(npu_addr as *const u32) };
        println!("npu version: {:#x}", npu);

        info!("test npu cru end");
    }
}
