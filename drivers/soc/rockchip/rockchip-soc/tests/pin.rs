use alloc::vec::Vec;
use core::ptr::NonNull;

use bare_test::{
    fdt_parser::Node,
    globals::{PlatformInfoKind, global_val},
    mem::{iomap, page_size},
};
use log::*;
use num_align::NumAlign;
use rockchip_soc::{PinConfig, rk3588::PinCtrl};

pub fn test_pin() {
    info!("Testing RK3588 PinManager...");

    let mut pinctrl = find_pinctrl();

    // read_pinctrl(&pinctrl, "/pinctrl/usb/vcc5v0-host-en");
    // read_pinctrl(&pinctrl, "/pinctrl/usb-typec/usbc0-int");
    read_pinctrl(&mut pinctrl, "/pinctrl/usb-typec/typec5v-pwren");

    info!("=== Test Complete ===");
}

fn read_pinctrl(m: &mut PinCtrl, pinctrl_node: &str) {
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

    info!("PinConfig: {:?}", pin_conf);

    let config = m.get_config(pin_conf.id).expect("Failed to get pin config");

    m.set_config(pin_conf).unwrap();

    info!("act config: {:?}", config);
}

fn find_pinctrl() -> PinCtrl {
    let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;
    let fdt = fdt.get();

    let pinctrl = fdt
        .find_compatible(&["rockchip,rk3588-pinctrl"])
        .next()
        .expect("Failed to find pinctrl node");
    info!("Found node: {}", pinctrl.name());

    let ioc = get_grf(&pinctrl, "rockchip,grf");

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

    PinCtrl::new(ioc, &gpio_banks)
}

pub fn get_grf(node: &Node, name: &str) -> NonNull<u8> {
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
