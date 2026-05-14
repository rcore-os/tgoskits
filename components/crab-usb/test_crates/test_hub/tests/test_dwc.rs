#![no_std]
#![no_main]
#![feature(used_with_arg)]

use rdrive::{Phandle, PlatformDevice, probe::OnProbeError, register::FdtInfo};

extern crate alloc;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use bare_test::{
    GetIrqConfig,
    async_std::time::sleep,
    fdt_parser::{Node, Status},
    globals::{PlatformInfoKind, global_val},
    irq::{IrqHandleResult, IrqInfo, IrqParam},
    mem::{iomap, page_size},
    println,
};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use crab_usb::*;
use log::*;
use rockchip_pm::RockchipPM;
use rockchip_soc::{Cru, CruOp, GpioDirection, PinConfig, PinCtrl, PinCtrlOp, SocType};

#[bare_test::tests]
mod tests {

    use core::ptr::NonNull;

    use bare_test::time::spin_delay;
    use crab_usb::usb_if::{DrMode, descriptor::ConfigurationDescriptor};
    use ktest_helper::KernelImpl;
    use rockchip_soc::CruOp;

    use super::*;

    static PROT_CHANGED: AtomicBool = AtomicBool::new(false);

    #[test]
    fn test_all() {
        // enable_clk();
        enable_power();
        // enable_vbus();
        setup_pinctrl();

        spin_on::spin_on(async {
            let info = get_usb_host();
            let irq_info = info.irq.clone().unwrap();

            let mut host = Box::pin(info.usb);

            register_irq(irq_info, &mut host);

            host.init().await.unwrap();
            info!("usb host init ok");
            info!("usb cmd test");

            for _ in 0..3 {
                if PROT_CHANGED.load(Ordering::Acquire) {
                    info!("port change detected");
                    PROT_CHANGED.store(false, Ordering::Release);
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }

            let mut ls = Vec::new();
            for _ in 0..10 {
                let ls2 = host.probe_devices().await.unwrap();
                if !ls2.is_empty() {
                    info!("found {} devices", ls2.len());
                    ls = ls2;
                    break;
                }
                sleep(Duration::from_millis(1000)).await;
            }

            for probed in ls {
                info!("{probed:#x?}");
                let Some(info) = probed.into_device_info() else {
                    continue;
                };

                let mut interface_desc = None;
                let mut config_desc: Option<ConfigurationDescriptor> = None;
                for config in info.configurations() {
                    info!("config: {:?}", config.configuration_value);

                    for interface in &config.interfaces {
                        for alt in &interface.alt_settings {
                            info!(
                                "interface[{}.{}] class {:?}",
                                alt.interface_number,
                                alt.alternate_setting,
                                alt.class()
                            );
                            if interface_desc.is_none() {
                                interface_desc = Some(alt.clone());
                                config_desc = Some(config.clone());
                            }
                        }
                    }
                }
                let interface_desc = interface_desc.unwrap();
                let config_desc = config_desc.unwrap();

                let mut device = host.open_device(&info).await.unwrap();

                info!("open device ok: {device:?}");

                device
                    .set_configuration(config_desc.configuration_value)
                    .await
                    .unwrap();
                info!("set configuration ok");

                //     // let config_value = device.current_configuration_descriptor().await.unwrap();
                //     // info!("get configuration: {config_value:?}");

                //     let mut interface = device
                //         .claim_interface(
                //             interface_desc.interface_number,
                //             interface_desc.alternate_setting,
                //         )
                //         .await
                //         .unwrap();
                //     info!(
                //         "claim interface ok: {interface}  class {:?} subclass {:?}",
                //         interface.descriptor.class, interface.descriptor.subclass
                //     );

                //     for ep_desc in &interface_desc.endpoints {
                //         info!("endpoint: {ep_desc:?}");

                //         match (ep_desc.transfer_type, ep_desc.direction) {
                //             (EndpointType::Bulk, Direction::In) => {
                //                 let mut bulk_in = interface.endpoint_bulk_in(ep_desc.address).unwrap();
                //                 // You can use bulk_in to transfer data

                //                 let mut buff = alloc::vec![0u8; 64];
                //                 while let Ok(n) = bulk_in.submit(&mut buff).unwrap().await {
                //                     let data = &buff[..n];
                //                     info!("bulk in data: {data:?}",);
                //                     break; // For testing, break after first transfer
                //                 }
                //             }
                //             // (EndpointType::Isochronous, Direction::In) => {
                //             //     let _iso_in = interface
                //             //         .endpoint::<Isochronous, In>(ep_desc.address)
                //             //         .unwrap();
                //             //     // You can use iso_in to transfer data
                //             // }
                //             _ => {
                //                 info!(
                //                     "unsupported {:?} {:?}",
                //                     ep_desc.transfer_type, ep_desc.direction
                //                 );
                //             }
                //         }
                //     }

                //     // let mut _bulk_in = interface.endpoint::<Bulk, In>(0x81).unwrap();

                //     // let mut buff = alloc::vec![0u8; 64];

                //     // while let Ok(n) = bulk_in.transfer(&mut buff).await {
                //     //     let data = &buff[..n];

                //     //     info!("bulk in data: {data:?}",);
                // }

                // drop(device);
            }
        });
    }

    struct XhciInfo {
        usb: USBHost,
        irq: Option<IrqInfo>,
    }

    fn get_usb_host() -> XhciInfo {
        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;

        let fdt = fdt.get();

        let mut count = 0;
        for node in fdt.all_nodes() {
            if matches!(node.status(), Some(Status::Disabled)) {
                continue;
            }

            if node.compatibles().any(|c| c.contains("snps,dwc3")) {
                // 只选择明确为 host 模式的控制器，避免误用 OTG 端口
                if let Some(prop) = node.find_property("dr_mode") {
                    let mode = prop.str();
                    if mode != "host" {
                        debug!("skip {} because dr_mode={}", node.name(), mode);
                        continue;
                    }
                }

                // 打印节点信息以便调试
                info!("=== Checking DWC3 node ===");
                info!("  Node name: {}", node.name());
                info!("  Node level: {}", node.level);
                info!(
                    "  Compatibles: {:?}",
                    node.compatibles().collect::<Vec<_>>()
                );

                println!("usb node: {}", node.name);
                let regs = node.reg().unwrap().collect::<Vec<_>>();
                println!("usb regs: {:?}", regs);

                for clk in node.clocks() {
                    println!("usb clock: {:?}", clk);
                }

                // ensure_rk3588_usb_power(&fdt, &node);

                // preper_3588_clk(&fdt, &node);

                let addr = iomap(
                    (regs[0].address as usize).into(),
                    regs[0].size.unwrap_or(0x1000),
                );

                let irq = node.irq_info();

                // 打印原始 IRQ 信息
                if let Some(ref i) = irq {
                    info!("IRQ from device tree: {:?}", i);
                    info!("  IRQ parent: {:?}", i.irq_parent);
                    info!("  IRQ configs: {:?}", i.cfgs);
                    for cfg in &i.cfgs {
                        info!("    IRQ: {:?}, flags: {:?}", cfg.irq, cfg.trigger);
                    }
                }

                let phys = node
                    .find_property("phys")
                    .unwrap()
                    .u32_list()
                    .collect::<Vec<_>>();

                // === USB2PHY 解析（phys[0]）===
                let u2phy_ph = phys[0].into();
                debug!("u2phy phandle: {}", u2phy_ph);
                let (u2_port_name, u2phy_pls) = find_phy_u2(u2phy_ph);

                let usbphy_grf = get_grf(u2phy_pls[0]);
                debug!("usb2phy-grf at {:p}", usbphy_grf.as_ptr());

                let u2_phy_node = fdt
                    .get_node_by_phandle(u2phy_pls[1])
                    .expect("Failed to find u2phy node");

                info!("Found USB2PHY node: {}", u2_phy_node.name());

                info!("u2phy port: {}", u2_port_name);

                let u2phy_reg = u2_phy_node.reg().unwrap().collect::<Vec<_>>().remove(0);
                let usb2phy_reg = u2phy_reg.address as usize;

                // // 解析 USB2PHY GRF
                // let usbctrl_grf_ph = u2_phy_node
                //     .find_property("rockchip,usbctrl-grf")
                //     .unwrap()
                //     .u32()
                //     .into();
                // let usbctrl_grf = get_grf(usbctrl_grf_ph);

                // 解析 USB2PHY resets
                let mut u2phy_rst_list = Vec::new();
                if let Some(resets_prop) = u2_phy_node.find_property("resets") {
                    let resets = resets_prop.u32_list().collect::<Vec<_>>();
                    if let Some(reset_names_prop) = u2_phy_node.find_property("reset-names") {
                        let reset_names = reset_names_prop.str_list().collect::<Vec<_>>();
                        for (cell, &name) in resets.chunks(2).zip(reset_names.iter()) {
                            u2phy_rst_list.push((name, cell[1] as u64));
                        }
                    }
                }

                // === USB3PHY 解析（phys[1]）===
                let u3phy_ph = phys[1].into();
                debug!("u3phy phandle: {}", u3phy_ph);

                // let phy_phandle = Phandle::from(0x495);
                let phy_node = find_phy_udp(u3phy_ph);
                let phy_id = phy_node.id;

                // 获取 USB2PHY ID
                let phy_phandle = phy_node.phandle;

                debug!("u3phy id: {}, phandle: {}", phy_id, phy_phandle);

                let u3_phy_node = fdt
                    .get_node_by_phandle(phy_phandle)
                    .expect("Failed to find u3phy node");

                info!("Found phy node: {}", u3_phy_node.name());

                info!("Preper PHY clocks");
                for clk in u3_phy_node.clocks() {
                    info!("enable `{:?}`, id: {}", clk.name, clk.select);
                    if clk.select == 0 {
                        debug!("skip clock with id 0");
                        continue;
                    }
                    let cru = rdrive::get_list::<CruDev>().remove(0);
                    let mut g = cru.lock().unwrap();
                    let id = clk.select.into();
                    if !g.0.clk_is_enabled(id).unwrap() {
                        g.0.clk_enable(id).unwrap();
                    }
                }

                let u3phy_reg = u3_phy_node.reg().unwrap().collect::<Vec<_>>().remove(0);

                let phy = iomap(
                    (u3phy_reg.address as usize).into(),
                    u3phy_reg.size.unwrap_or(0x1000),
                );

                let u2phy_grf = get_grf(
                    u3_phy_node
                        .find_property("rockchip,u2phy-grf")
                        .unwrap()
                        .u32()
                        .into(),
                );

                let usb_grf = get_grf(
                    u3_phy_node
                        .find_property("rockchip,usb-grf")
                        .unwrap()
                        .u32()
                        .into(),
                );

                let usbdpphy_grf = get_grf(
                    u3_phy_node
                        .find_property("rockchip,usbdpphy-grf")
                        .unwrap()
                        .u32()
                        .into(),
                );

                let vo_grf = get_grf(
                    u3_phy_node
                        .find_property("rockchip,vo-grf")
                        .unwrap()
                        .u32()
                        .into(),
                );

                // 完全按照 U-Boot 的逻辑处理 dp-lane-mux
                // 如果设备树中没有 rockchip,dp-lane-mux 属性，则使用纯 USB 模式
                let dp_lane_mux: Vec<u32> = match u3_phy_node.find_property("rockchip,dp-lane-mux")
                {
                    Some(prop) => prop.u32_list().collect(), // 有属性 → 读取 lane 配置
                    None => Vec::new(),                      // 无属性 → 纯 USB 模式
                };

                let mut phy_rst_list = Vec::new();
                let resets_prop = u3_phy_node
                    .find_property("resets")
                    .expect("Missing resets property");
                let resets = resets_prop.u32_list().collect::<Vec<_>>();
                let reset_names_prop = u3_phy_node
                    .find_property("reset-names")
                    .expect("Missing reset-names property");
                let reset_names = reset_names_prop.str_list().collect::<Vec<_>>();
                for (cell, &name) in resets.chunks(2).zip(reset_names.iter()) {
                    phy_rst_list.push((name, cell[1] as u64));
                }

                let mut rst_list = Vec::new();
                let resets_prop = node
                    .find_property("resets")
                    .expect("Missing resets property");
                let resets = resets_prop.u32_list().collect::<Vec<_>>();
                let reset_names_prop = node
                    .find_property("reset-names")
                    .expect("Missing reset-names property");
                let reset_names = reset_names_prop.str_list().collect::<Vec<_>>();
                for (cell, &name) in resets.chunks(2).zip(reset_names.iter()) {
                    rst_list.push((name, cell[1] as u64));
                }

                let mut params = DwcParams::default();

                let dr_mode = node
                    .find_property("dr_mode")
                    .map(|p| p.str())
                    .unwrap_or("host");
                match dr_mode {
                    "host" => params.dr_mode = DrMode::Host,
                    "peripheral" => params.dr_mode = DrMode::Peripheral,
                    "otg" => params.dr_mode = DrMode::Otg,
                    _ => {}
                }

                let phy_type = node.find_property("phy_type").unwrap().str();
                match phy_type {
                    "utmi" => params.hsphy_mode = UsbPhyInterfaceMode::Utmi,
                    "utmi_wide" => params.hsphy_mode = UsbPhyInterfaceMode::UtmiWide,
                    _ => {}
                }

                if node.find_property("snps,has-lpm-erratum").is_some() {
                    params.has_lpm_erratum = true;
                }

                if node.find_property("snps,is-utmi-l1-suspend").is_some() {
                    params.is_utmi_l1_suspend = true;
                }

                if node.find_property("snps,disable_scramble_quirk").is_some() {
                    params.disable_scramble_quirk = true;
                }

                if node.find_property("snps,u2exit_lfps_quirk").is_some() {
                    params.u2exit_lfps_quirk = true;
                }

                if node.find_property("snps,u2ss_inp3_quirk").is_some() {
                    params.u2ss_inp3_quirk = true;
                }

                if node.find_property("snps,req_p1p2p3_quirk").is_some() {
                    params.req_p1p2p3_quirk = true;
                }

                if node.find_property("snps,del_p1p2p3_quirk").is_some() {
                    params.del_p1p2p3_quirk = true;
                }

                if node.find_property("snps,del_phy_power_chg_quirk").is_some() {
                    params.del_phy_power_chg_quirk = true;
                }

                if node.find_property("snps,lfps_filter_quirk").is_some() {
                    params.lfps_filter_quirk = true;
                }

                if node.find_property("snps,rx_detect_poll_quirk").is_some() {
                    params.rx_detect_poll_quirk = true;
                }

                if node.find_property("snps,dis_u3_susphy_quirk").is_some() {
                    params.dis_u3_susphy_quirk = true;
                }

                if node.find_property("snps,dis_u2_susphy_quirk").is_some() {
                    params.dis_u2_susphy_quirk = true;
                }

                if node.find_property("snps,dis_enblslpm_quirk").is_some() {
                    params.dis_enblslpm_quirk = true;
                }

                if node
                    .find_property("snps,dis-u2-freeclk-exists-quirk")
                    .is_some()
                {
                    params.dis_u2_freeclk_exists_quirk = true;
                }

                if node.find_property("snps,tx_de_emphasis_quirk").is_some() {
                    params.tx_de_emphasis_quirk = true;
                }

                return XhciInfo {
                    usb: USBHost::new_dwc(DwcNewParams {
                        ctrl: addr,
                        phy,
                        phy_param: UdphyParam {
                            id: phy_id,
                            u2phy_grf,
                            usb_grf,
                            usbdpphy_grf,
                            vo_grf,
                            dp_lane_mux: &dp_lane_mux,
                            rst_list: &phy_rst_list,
                        },
                        usb2_phy_param: Usb2PhyParam {
                            reg: usb2phy_reg,
                            port_kind: Usb2PhyPortId::from_node_name(&u2_port_name)
                                .expect("Unknown USB2PHY port name"),
                            usb_grf: usbphy_grf,
                            rst_list: &u2phy_rst_list,
                        },
                        rst_list: &rst_list,
                        cru: CruOpImpl,
                        params,
                        kernel: &KernelImpl,
                    })
                    .unwrap(),
                    irq,
                };
            }
        }

        panic!("no xhci found");
    }

    fn register_irq(irq: IrqInfo, host: &mut USBHost) {
        let handle = host.create_event_handler();

        let cfg = irq.cfgs[0].clone();

        IrqParam {
            intc: irq.irq_parent,
            cfg,
        }
        .register_builder({
            move |_irq| {
                let event = handle.handle_event();
                if let Event::PortChange { .. } = event {
                    PROT_CHANGED.store(true, Ordering::Release);
                }

                IrqHandleResult::Handled
            }
        })
        .register();
    }

    fn enable_power() {
        let mmio = get_syscon_addr();
        let mut pm = RockchipPM::new(mmio, rockchip_pm::RkBoard::Rk3588);

        info!("RockchipPM initialized at {:p}", mmio.as_ptr());

        // 开启 USB 电源域 (domain 31)
        // 这是 usbdrd3_0 和 usbdrd3_1 控制器所需的电源域
        match pm.power_domain_on(rockchip_pm::PowerDomain(31)) {
            Ok(_) => info!("USB power domain (31) enabled successfully"),
            Err(e) => {
                error!("Failed to enable USB power domain: {:?}", e);
                panic!("USB power domain enable failed");
            }
        }

        // 可选：开启 PHP 总线结构域 (domain 32)
        // PHP 是处理高性能总线的电源域，某些 USB 配置可能需要
        match pm.power_domain_on(rockchip_pm::PowerDomain(32)) {
            Ok(_) => info!("PHP power domain (32) enabled successfully"),
            Err(e) => {
                warn!(
                    "Failed to enable PHP power domain: {:?} (may be optional)",
                    e
                );
            }
        }

        info!("All required power domains enabled");
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

    fn enable_vbus() {
        // GPIO4 寄存器偏移定义
        const GPIO_SWPORT_DR_L: usize = 0x0000; // 数据寄存器低 16 位
        const GPIO_SWPORT_DR_H: usize = 0x0004; // 数据寄存器高 16 位
        const GPIO_SWPORT_DDR_L: usize = 0x0008; // 方向寄存器低 16 位
        const GPIO_SWPORT_DDR_H: usize = 0x000C; // 方向寄存器高 16 位

        // GPIO 引脚 bit 8 的值（Rockchip GRF 格式：Bit[31:16] 是写使能掩码，Bit[15:0] 是数据值）
        // 0x01000100 = (0x0100 << 16) | 0x0100，控制 bit 8
        const GPIO_BIT8_VALUE: u32 = (0x0100u32 << 16) | 0x0100u32;

        // 映射 GPIO4 寄存器基址
        let gpio_base = iomap(0xFEC50000.into(), 0x1000);

        // 配置 GPIO4_B0 和 GPIO4_D0 为输出模式（DDR 寄存器）
        let ddr_l = (gpio_base.as_ptr() as usize + GPIO_SWPORT_DDR_L) as *mut u32;
        let ddr_h = (gpio_base.as_ptr() as usize + GPIO_SWPORT_DDR_H) as *mut u32;

        unsafe {
            // 设置 GPIO4_B0 方向为输出
            ddr_l.write_volatile(GPIO_BIT8_VALUE);
            // 设置 GPIO4_D0 方向为输出
            ddr_h.write_volatile(GPIO_BIT8_VALUE);
        }

        // 设置 GPIO4_B0 和 GPIO4_D0 输出高电平（DR 寄存器）
        let dr_l = (gpio_base.as_ptr() as usize + GPIO_SWPORT_DR_L) as *mut u32;
        let dr_h = (gpio_base.as_ptr() as usize + GPIO_SWPORT_DR_H) as *mut u32;

        unsafe {
            // 设置 GPIO4_B0 输出高电平
            dr_l.write_volatile(GPIO_BIT8_VALUE);
            // 设置 GPIO4_D0 输出高电平
            dr_h.write_volatile(GPIO_BIT8_VALUE);
        }

        info!("VBUS power enabled: GPIO4_B0 and GPIO4_D0 set high");
    }
}

fn get_grf(phandle: Phandle) -> NonNull<u8> {
    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();

    let node = fdt.get_node_by_phandle(phandle).unwrap();

    info!("Found node: {}", node.name());

    let regs = node.reg().unwrap().collect::<Vec<_>>();
    let reg = regs[0];
    iomap((reg.address as usize).into(), reg.size.unwrap_or(0x1000))
}

rdrive::module_driver! {
    name: "CRU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["rockchip,rk3588-cru"],
        // Use `probe_clk` above; this usage is because doctests cannot find the parent module.
        on_probe: on_probe_cru,
    }],
}

fn on_probe_cru(node: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
    // Initialization code for CRU can be added here if needed.
    // 获取 CRU 寄存器基址
    let Some(reg) = node.node.reg().and_then(|mut r| r.next()) else {
        warn!("CRU node has no valid register, skip clock enable");
        return Err(OnProbeError::KError(rdrive::KError::BadAddr(0)));
    };

    let base = iomap((reg.address as usize).into(), reg.size.unwrap_or(0x1000));

    info!("RK3588 CRU base at {:p}", base.as_ptr());

    let grf_phandle = node
        .node
        .find_property("rockchip,grf")
        .unwrap()
        .u32()
        .into();

    let grf = get_grf(grf_phandle);

    let clk = CruDev(Cru::new(SocType::Rk3588, base, grf));
    dev.register(clk);

    Ok(())
}

struct CruDev(Cru);

impl rdrive::DriverGeneric for CruDev {
    fn open(&mut self) -> Result<(), rdrive::KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), rdrive::KError> {
        Ok(())
    }
}

pub struct PhyNode {
    pub id: usize,
    pub phandle: Phandle,
}

pub fn find_phy_udp(ph: Phandle) -> PhyNode {
    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();

    let mut als = Vec::new();

    let aliases = fdt.find_nodes("/aliases").next().unwrap();
    for prop in aliases.propertys() {
        if prop.name.starts_with("usbdp") {
            let id = prop
                .name
                .trim_start_matches("usbdp")
                .parse::<usize>()
                .unwrap();
            als.push((id, prop.str().to_string()));
        }
    }

    for (id, path) in als {
        let name = path.split("/").last().unwrap();
        debug!("search phy node by name: {}", name);
        let mut iter = fdt.all_nodes();
        if let Some((usdpph, level)) = fdt_iter_find(&mut iter, name) {
            debug!("found phy node: {}", name);
            for node in iter {
                if node.level <= level {
                    break;
                }
                if let Some(p) = node.phandle()
                    && p == ph
                {
                    return PhyNode {
                        id,
                        phandle: usdpph,
                    };
                }
            }
        }
    }
    panic!("no phy node found");
}

pub fn find_phy_u2(ph: Phandle) -> (String, [Phandle; 3]) {
    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();

    debug!("search phy node by {ph}");
    let iter = fdt.all_nodes();
    let mut out = [Phandle::from(0); 3];
    for node in iter {
        if let Some(p) = node.phandle() {
            if node
                .compatibles()
                .any(|c| c == "rockchip,rk3588-usb2phy-grf")
            {
                out[0] = p;
            }
            if node.compatibles().any(|c| c == "rockchip,rk3588-usb2phy") {
                out[1] = p;
            }
            if p == ph {
                out[2] = p;
                return (node.name().to_string(), out);
            }
        }
    }

    panic!("no phy2 node found");
}

fn fdt_iter_find<'a>(
    iter: &mut impl Iterator<Item = Node<'a>>,
    name: &str,
) -> Option<(Phandle, usize)> {
    for node in iter.by_ref() {
        if node.name() == name {
            return Some((node.phandle().unwrap(), node.level));
        }
    }
    None
}
fn get_grf2(node: &Node, name: &str) -> NonNull<u8> {
    let ph = node.find_property(name).unwrap().u32().into();

    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();
    let node = fdt.get_node_by_phandle(ph).unwrap();

    let regs = node.reg().unwrap().collect::<Vec<_>>();
    let start = regs[0].address as usize;
    let end = start + regs[0].size.unwrap_or(0);
    info!("Syscon address range: 0x{:x} - 0x{:x}", start, end);
    let start = start & !(page_size() - 1);
    let end = (end + page_size() - 1) & !(page_size() - 1);
    info!("Aligned Syscon address range: 0x{:x} - 0x{:x}", start, end);
    iomap(start.into(), end - start)
}

fn find_pinctrl() -> PinCtrl {
    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();

    let pinctrl = fdt
        .find_compatible(&["rockchip,rk3588-pinctrl"])
        .next()
        .expect("Failed to find pinctrl node");
    info!("Found node: {}", pinctrl.name());

    let ioc = get_grf2(&pinctrl, "rockchip,grf");

    let mut gpio_banks = [NonNull::dangling(); 5];

    for (idx, node) in fdt.find_compatible(&["rockchip,gpio-bank"]).enumerate() {
        if idx >= 5 {
            warn!("More than 5 GPIO banks found, ignoring extra banks");
            break;
        }
        info!("Found GPIO bank node: {}", node.name());
        let reg = node.reg().unwrap().next().unwrap();

        let gpio_mmio = iomap(
            (reg.address as usize).into(),
            reg.size.unwrap_or(0).align_up(page_size()),
        );
        gpio_banks[idx] = gpio_mmio;
    }

    PinCtrl::new(SocType::Rk3588, ioc, &gpio_banks)
}

fn set_pinctrl(m: &mut PinCtrl, pinctrl_node: &str) {
    info!("Reading pinctrl node: {}", pinctrl_node);
    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();
    let node = fdt.find_nodes(pinctrl_node).next().unwrap();

    info!("Found node: {}", node.name());

    let pins = node
        .find_property("rockchip,pins")
        .unwrap()
        .u32_list()
        .collect::<Vec<_>>();

    let pin_conf = PinConfig::new_with_fdt(
        &pins,
        NonNull::new(fdt.as_slice().as_ptr() as usize as _).unwrap(),
    );

    let act_conf = m.get_config(pin_conf.id).unwrap();

    info!("PinConfig: {:?}", act_conf);
    let dir = m.gpio_direction(pin_conf.id).unwrap();
    let dst_dir = GpioDirection::Output(true);
    info!("GPIO Direction: {:?} -> {dst_dir:?}", dir);
    info!("dtb pin config: {:?}", pin_conf);

    m.set_config(pin_conf).expect("Failed to get pin config");
    m.set_gpio_direction(pin_conf.id, dst_dir).unwrap();
}

fn setup_pinctrl() {
    let mut pinctrl = find_pinctrl();
    set_pinctrl(&mut pinctrl, "/pinctrl/usb/vcc5v0-host-en");
    info!("VBUS power toggled via GPIO");
}

struct CruOpImpl;

impl crab_usb::CruOp for CruOpImpl {
    fn reset_assert(&self, id: u64) {
        let cru = rdrive::get_list::<CruDev>().remove(0);
        cru.lock().unwrap().0.reset_assert(id.into());
    }
    fn reset_deassert(&self, id: u64) {
        let cru = rdrive::get_list::<CruDev>().remove(0);
        cru.lock().unwrap().0.reset_deassert(id.into());
    }
}

trait Align {
    fn align_up(&self, align: usize) -> usize;
}

impl Align for usize {
    fn align_up(&self, align: usize) -> usize {
        if (*self).is_multiple_of(align) {
            *self
        } else {
            *self + align - *self % align
        }
    }
}
