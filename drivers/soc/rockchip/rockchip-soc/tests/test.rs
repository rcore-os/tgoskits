#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;

mod pin;

#[bare_test::tests]
mod tests {
    use alloc::vec::Vec;
    use core::ptr::NonNull;

    use bare_test::{
        globals::{PlatformInfoKind, global_val},
        mem::{iomap, page_size},
        println,
        time::since_boot,
    };
    use log::{info, warn};
    use rockchip_soc::{Cru, CruOp, SocType, rk3588::CCLK_EMMC};
    use sdmmc::emmc::{
        EMmcHost,
        clock::{Clk, ClkError, init_global_clk},
    };
    use spin::{Mutex, Once};

    use crate::pin::test_pin;

    static INIT: Once<Mutex<Cru>> = Once::new();

    pub fn initclk(clk: Cru) {
        INIT.call_once(|| Mutex::new(clk));
    }

    #[test]
    fn it_works() {
        let cru3588 = 0xfd7c0000usize;
        // let sys_grf = Cru::grf_mmio_ls()[0];
        let sys_grf_base = 0xfd58c000usize;
        let sys_grf_size = 0x1000usize;

        let base = iomap(cru3588.into(), 0x5c000);
        let sys_grf = iomap(sys_grf_base.into(), sys_grf_size);

        let cru = Cru::new(SocType::Rk3588, base, sys_grf);
        initclk(cru);

        test_with_emmc();
        test_pin();
    }

    fn test_with_emmc() {
        info!("test with emmc");
        let emmc_addr_ptr = get_device_addr("rockchip,dwcmshc-sdhci");
        info!("emmc addr ptr: {:?}", emmc_addr_ptr);
        init_global_clk(&ClkUnit);
        let mut emmc = EMmcHost::new(emmc_addr_ptr.as_ptr() as usize);
        emmc.init().unwrap();

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
            write_buffer[i] = 0_u8;
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

                        println!("First 16 bytes of read block: {:?}", read_buffer.to_vec());

                        if data_match {
                            println!(
                                "Data verification successful: written and read data match \
                                 perfectly!"
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

    pub struct ClkUnit;

    impl Clk for ClkUnit {
        fn emmc_get_clk(&self) -> Result<u64, ClkError> {
            if let Ok(rate) = INIT.wait().lock().clk_get_rate(CCLK_EMMC) {
                Ok(rate)
            } else {
                Err(ClkError::InvalidClockRate)
            }
        }

        fn emmc_set_clk(&self, rate: u64) -> Result<u64, ClkError> {
            if let Ok(rate) = INIT.wait().lock().clk_set_rate(CCLK_EMMC, rate) {
                Ok(rate)
            } else {
                Err(ClkError::InvalidClockRate)
            }
        }
    }

    struct SKernel;

    impl sdmmc::Kernel for SKernel {
        fn sleep(us: u64) {
            let start = since_boot();
            let duration = core::time::Duration::from_micros(us);

            while since_boot() - start < duration {
                core::hint::spin_loop();
            }
        }
    }

    sdmmc::set_impl!(SKernel);
}
