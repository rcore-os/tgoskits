#![no_std]
#![no_main]
#![feature(used_with_arg)]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;
extern crate bare_test;

#[bare_test::tests]
mod tests {
    use alloc::vec::Vec;
    use core::{hint::spin_loop, ptr::NonNull, sync::atomic::AtomicU32, time::Duration};

    use arm_scmi::{Scmi, Shmem, Smc};
    use bare_test::{
        hal::al::IrqId,
        os::{
            irq::register_handler,
            mem::{dma::kernel_dma_op, mmio::ioremap, page_size},
            platform::{PlatformDescriptor, get_platform_descriptor},
            time::since_boot,
        },
    };
    use dma_api::DeviceDma;
    use fdt_edit::{Fdt, Phandle};
    use num_align::NumAlign;
    use rk3588_clk::Rk3588Cru;
    use rknpu::{
        Rknpu, RknpuConfig, RknpuType, Submit,
        op::{self, Operation},
    };
    use rockchip_pm::{PowerDomain, RkBoard, RockchipPM};

    /// NPU 主电源域
    pub const NPU: PowerDomain = PowerDomain(8);
    /// NPU TOP 电源域  
    pub const NPUTOP: PowerDomain = PowerDomain(9);
    /// NPU1 电源域
    pub const NPU1: PowerDomain = PowerDomain(10);
    /// NPU2 电源域
    pub const NPU2: PowerDomain = PowerDomain(11);

    static IRQ_STATUS: AtomicU32 = AtomicU32::new(0);

    fn dma_device() -> DeviceDma {
        DeviceDma::new(u32::MAX as u64, kernel_dma_op())
    }

    #[test]
    fn it_works() {
        // set_up_scmi();

        let reg = get_syscon_addr();
        let board = RkBoard::Rk3588;

        let mut pm = RockchipPM::new(reg, board);

        pm.power_domain_on(NPUTOP).unwrap();
        pm.power_domain_on(NPU).unwrap();
        pm.power_domain_on(NPU1).unwrap();
        pm.power_domain_on(NPU2).unwrap();

        info!("Powered on NPU domains");

        let mut npu = find_rknpu();
        npu.open().unwrap();
        info!("Opened RKNPU");

        info!("Found RKNPU {:#x}", npu.get_hw_version());

        matul_test(&mut npu);
    }

    fn find_rknpu() -> Rknpu {
        let fdt = platform_fdt();
        let node = fdt
            .find_compatible(&["rockchip,rk3588-rknpu"])
            .into_iter()
            .next()
            .unwrap();

        info!("Found node: {}", node.name());
        let mut config = None;
        for c in node.as_node().compatibles() {
            if c == "rockchip,rk3588-rknpu" {
                config = Some(RknpuConfig {
                    rknpu_type: RknpuType::Rk3588,
                });
                break;
            }
        }
        // let clk_ls = node.clocks().collect::<Vec<_>>();
        // let mut clk_ctrl = configure_npu_clocks();
        // info!("Configured NPU clock tree");
        // for clk in &clk_ls {
        //     info!("Clock: {:?}", clk);
        //     if clk.node.name().contains("protocol") {
        //         continue;
        //     }
        //     clk_ctrl.npu_gate_enable(clk.select as _).unwrap();
        // }

        let config = config.expect("Unsupported RKNPU compatible");

        let mut base_regs = Vec::new();

        for reg in node.regs() {
            let start_raw = reg.address as usize;
            let size = reg.size.unwrap_or(page_size() as u64) as usize;
            let end = start_raw + size;

            let start = start_raw & !(page_size() - 1);
            let offset = start_raw - start;
            let end = (end + page_size() - 1) & !(page_size() - 1);
            let size = end - start;

            let mapping = ioremap(start.into(), size).unwrap();
            base_regs.push(unsafe { NonNull::new_unchecked(mapping.as_ptr().add(offset)) });
        }
        let rknpu = Rknpu::new(&base_regs, config, dma_device());

        let irq_handler0 = rknpu.new_irq_handler(0);

        let irq_ref = node.interrupts().into_iter().next().unwrap();
        let irq_id = IrqId::from(gic_irq_id(&irq_ref.specifier));
        register_handler(irq_id, move || {
            let status = irq_handler0.handle();
            IRQ_STATUS.store(status, core::sync::atomic::Ordering::SeqCst);
        });

        rknpu
    }

    fn get_syscon_addr() -> NonNull<u8> {
        let fdt = platform_fdt();

        let mut node = None;
        for candidate in fdt.find_compatible(&["syscon"]) {
            if candidate.name().contains("power-manage") {
                node = Some(candidate);
                break;
            }
        }
        let node = node.expect("Failed to find syscon node");

        info!("Found node: {}", node.name());

        let regs = node.regs();
        let start = regs[0].address as usize;
        let end = start + regs[0].size.unwrap_or(0) as usize;
        info!("Syscon address range: 0x{:x} - 0x{:x}", start, end);
        let start = start & !(page_size() - 1);
        let end = (end + page_size() - 1) & !(page_size() - 1);
        info!("Aligned Syscon address range: 0x{:x} - 0x{:x}", start, end);
        ioremap(start.into(), end - start).unwrap().as_nonnull_ptr()
    }

    fn configure_npu_clocks() -> Rk3588Cru {
        let cru_addr = get_cru_addr();
        Rk3588Cru::new(cru_addr)
        // let cru = Rk3588Cru::new(cru_addr);

        // // Program the primary NPU clock tree to known-good defaults. Ignore failures for now.
        // let _ = cru.npu_set_clk(HCLK_NPU_ROOT, 200_000_000);
        // let _ = cru.npu_set_clk(CLK_NPU_DSU0, 800_000_000);
        // let _ = cru.npu_set_clk(PCLK_NPU_ROOT, 100_000_000);
        // let _ = cru.npu_set_clk(HCLK_NPU_CM0_ROOT, 200_000_000);
        // let _ = cru.npu_set_clk(CLK_NPU_CM0_RTC, 24_000_000);
        // let _ = cru.npu_set_clk(CLK_NPUTIMER_ROOT, 100_000_000);

        // // Ensure the essential gates are open.
        // for gate in [
        //     ACLK_NPU0,
        //     HCLK_NPU0,
        //     ACLK_NPU1,
        //     HCLK_NPU1,
        //     ACLK_NPU2,
        //     HCLK_NPU2,
        //     PCLK_NPU_PVTM,
        //     PCLK_NPU_GRF,
        //     CLK_NPU_PVTM,
        //     CLK_CORE_NPU_PVTM,
        //     PCLK_NPU_TIMER,
        //     CLK_NPUTIMER0,
        //     CLK_NPUTIMER1,
        //     PCLK_NPU_WDT,
        //     TCLK_NPU_WDT,
        //     FCLK_NPU_CM0_CORE,
        // ] {
        //     if let Err(err) = cru.npu_gate_enable(gate) {
        //         warn!("Failed to enable gate {gate}: {err}");
        //     }
        // }
    }

    fn get_cru_addr() -> NonNull<u8> {
        let fdt = platform_fdt();

        let node = fdt
            .find_compatible(&["rockchip,rk3588-cru"])
            .into_iter()
            .next()
            .expect("Failed to find CRU node");

        info!("Found node: {}", node.name());

        let regs = node.regs();
        let reg = regs.first().copied().expect("CRU node missing reg range");

        let start_raw = reg.address as usize;
        let size = reg.size.unwrap_or(page_size() as u64) as usize;

        let start = start_raw & !(page_size() - 1);
        let end = (start_raw + size + page_size() - 1) & !(page_size() - 1);
        let offset = start_raw - start;

        let mapping = ioremap(start.into(), end - start).unwrap();
        let ptr = unsafe { mapping.as_ptr().add(offset) };

        // SAFETY: iomap guarantees a valid mapping; offset is within bounds.
        unsafe { NonNull::new_unchecked(ptr) }
    }

    fn set_up_scmi() {
        let fdt = platform_fdt();
        let node = fdt
            .find_compatible(&["arm,scmi-smc"])
            .into_iter()
            .next()
            .expect("scmi not found");

        info!("found scmi node: {:?}", node.name());

        let shmem_ph: Phandle = node
            .as_node()
            .get_property("shmem")
            .expect("shmem property not found")
            .get_u32()
            .expect("invalid shmem phandle")
            .into();

        let shmem_node = fdt.get_by_phandle(shmem_ph).expect("shmem node not found");

        info!("found shmem node: {:?}", shmem_node.name());

        let shmem_reg = shmem_node.regs();
        assert_eq!(shmem_reg.len(), 1);
        let shmem_reg = shmem_reg[0];
        let shmem_addr = ioremap(
            (shmem_reg.address as usize).into(),
            shmem_reg.size.unwrap().align_up(0x1000) as usize,
        )
        .unwrap();

        let func_id = node
            .as_node()
            .get_property("arm,smc-id")
            .expect("function-id property not found")
            .get_u32()
            .expect("invalid function-id");

        info!("shmem reg: {:?}", shmem_reg);
        info!("func_id: {:#x}", func_id);

        let irq_num = node
            .as_node()
            .get_property("a2p")
            .and_then(|irq_prop| irq_prop.get_u32());

        let shmem = Shmem {
            address: shmem_addr.as_nonnull_ptr(),
            bus_address: shmem_reg.child_bus_address as usize,
            size: shmem_reg.size.unwrap() as usize,
        };
        let kind = Smc::new(func_id, irq_num);
        let scmi = Scmi::new(kind, shmem);

        let mut pclk = scmi.protocol_clk();

        let ls = [
            (0u32, "clk0", 0x30a32c00),
            (2u32, "clk1", 0x30a32c00),
            (3u32, "clk2", 0x30a32c00),
            (6u32, "clk-npu", 0xbebc200),
        ];
        for (id, name, clk) in ls {
            pclk.clk_enable(id).unwrap();
            let rate = pclk.rate_get(id).unwrap();
            info!("Clock {} (id={}): rate={} Mz", name, id, rate / 1000000);
            pclk.rate_set(id, clk).unwrap();
            let rate = pclk.rate_get(id).unwrap();
            info!("Clock {} (id={}): new rate={} Mz", name, id, rate / 1000000);
        }
    }

    fn matul_test(npu: &mut Rknpu) {
        let m = 16;
        let k = 32;
        let n = 32;

        let a_data: Vec<i8> = (0..(m * k)).map(|x| x as _).collect();
        let b_data: Vec<i8> = (0..(k * n)).map(|x| x as _).collect();
        let mut want: Vec<i32> = vec![0i32; m * n];

        matmul_int(m, k, n, &a_data, &b_data, &mut want);

        let mut npu_matmul = op::matmul::MatMul::<i8, i32>::new(npu.dma(), m, k, n);

        npu_matmul.set_a(&a_data);

        npu_matmul.set_b(&b_data);

        let mut job = Submit::new(npu.dma(), vec![Operation::MatMulu8(npu_matmul)]);

        let bstatus = npu.handle_interrupt0();

        npu.submit(&mut job).unwrap();

        info!("Submitted matmul job to NPU");
        loop {
            spin_delay(Duration::from_millis(500));
            let status = IRQ_STATUS.load(core::sync::atomic::Ordering::SeqCst);

            // let status = npu.handle_interrupt0();
            if status != bstatus {
                info!("NPU interrupt status after matmul: 0x{:x}", status);
                break;
            }
        }

        let Operation::MatMulu8(val) = &job.tasks[0];

        let M = m as _;
        let N = n as _;
        for m in 1..=M {
            for n in 1..=N {
                let actual: i32 = val.get_output(m, n);
                let expected = want[((m - 1) * N) + (n - 1)];
                assert_eq!(
                    actual, expected,
                    "Matmul result mismatch at m={}, n={}: actual {}, expected {}",
                    m, n, actual, expected
                );
            }
        }

        info!("Matmul result matches expected output");
    }

    fn matmul_int(m: usize, k: usize, n: usize, src0: &[i8], src1: &[i8], dst: &mut [i32]) {
        for i in 0..m {
            for j in 0..n {
                let mut sum = 0;
                for l in 0..k {
                    sum += (src0[i * k + l] as i32) * (src1[j * k + l] as i32);
                }
                dst[i * n + j] = sum;
            }
        }
    }

    fn platform_fdt() -> Fdt {
        match get_platform_descriptor() {
            PlatformDescriptor::DeviceTree(dtb) => {
                Fdt::from_bytes(dtb.as_slice()).expect("failed to parse live device tree")
            }
            PlatformDescriptor::Acpi => panic!("ACPI platform is not supported"),
            PlatformDescriptor::None => panic!("device tree is unavailable"),
        }
    }

    fn gic_irq_id(specifier: &[u32]) -> usize {
        match specifier {
            [id] => *id as usize,
            [0, num, ..] => *num as usize + 32,
            [1, num, ..] => *num as usize + 16,
            [kind, ..] => panic!("unsupported GIC interrupt specifier type: {kind}"),
            [] => panic!("empty interrupt specifier"),
        }
    }

    fn spin_delay(duration: Duration) {
        let deadline = since_boot() + duration;
        while since_boot() < deadline {
            spin_loop();
        }
    }
}
