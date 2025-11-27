//! FDT parsing and processing functionality.

use alloc::{string::ToString, vec::Vec};
use axvm::config::{AxVMConfig, AxVMCrateConfig, PassThroughDeviceConfig};
use fdt_parser::{Fdt, FdtHeader, PciRange, PciSpace};

use crate::vmm::fdt::crate_guest_fdt_with_cache;
use crate::vmm::fdt::create::update_cpu_node;

pub fn get_host_fdt() -> &'static [u8] {
    const FDT_VALID_MAGIC: u32 = 0xd00d_feed;
    let bootarg: usize = std::os::arceos::modules::axhal::get_bootarg();
    let header = unsafe {
        core::slice::from_raw_parts(bootarg as *const u8, core::mem::size_of::<FdtHeader>())
    };
    let fdt_header = FdtHeader::from_bytes(header)
        .map_err(|e| format!("Failed to parse FDT header: {e:#?}"))
        .unwrap();

    if fdt_header.magic.get() != FDT_VALID_MAGIC {
        error!(
            "FDT magic is invalid, expected {:#x}, got {:#x}",
            FDT_VALID_MAGIC,
            fdt_header.magic.get()
        );
    }

    unsafe { core::slice::from_raw_parts(bootarg as *const u8, fdt_header.total_size()) }
}

pub fn setup_guest_fdt_from_vmm(
    fdt_bytes: &[u8],
    vm_cfg: &mut AxVMConfig,
    crate_config: &AxVMCrateConfig,
) {
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");

    // Call the modified function and get the returned device name list
    let passthrough_device_names = super::device::find_all_passthrough_devices(vm_cfg, &fdt);

    let dtb_data = super::create::crate_guest_fdt(&fdt, &passthrough_device_names, crate_config);
    crate_guest_fdt_with_cache(dtb_data, crate_config);
}

pub fn set_phys_cpu_sets(vm_cfg: &mut AxVMConfig, fdt: &Fdt, crate_config: &AxVMCrateConfig) {
    // Find and parse CPU information from host DTB
    let host_cpus: Vec<_> = fdt.find_nodes("/cpus/cpu").collect();
    info!("Found {} host CPU nodes", &host_cpus.len());

    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_ref()
        .expect("ERROR: phys_cpu_ids not found in config.toml");

    // Collect all CPU node information into Vec to avoid using iterators multiple times
    let cpu_nodes_info: Vec<_> = host_cpus
        .iter()
        .filter_map(|cpu_node| {
            if let Some(mut cpu_reg) = cpu_node.reg() {
                if let Some(r) = cpu_reg.next() {
                    info!(
                        "CPU node: {}, phys_cpu_id: 0x{:x}",
                        cpu_node.name(),
                        r.address
                    );
                    Some((cpu_node.name().to_string(), r.address as usize))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();
    // Create mapping from phys_cpu_id to physical CPU index
    // Collect all unique CPU addresses, maintaining the order of appearance in the device tree
    let mut unique_cpu_addresses = Vec::new();
    for (_, cpu_address) in &cpu_nodes_info {
        if !unique_cpu_addresses.contains(cpu_address) {
            unique_cpu_addresses.push(*cpu_address);
        } else {
            panic!("Duplicate CPU address found");
        }
    }

    // Assign index to each CPU address in the device tree and print detailed information
    for (index, &cpu_address) in unique_cpu_addresses.iter().enumerate() {
        // Find all CPU nodes using this address
        for (cpu_name, node_address) in &cpu_nodes_info {
            if *node_address == cpu_address {
                debug!(
                    "  CPU node: {cpu_name}, address: 0x{cpu_address:x}, assigned index: {index}"
                );
                break; // Print each address only once
            }
        }
    }

    // Calculate phys_cpu_sets based on phys_cpu_ids in vcpu_mappings
    let mut new_phys_cpu_sets = Vec::new();
    for phys_cpu_id in phys_cpu_ids {
        // Find the index corresponding to phys_cpu_id in unique_cpu_addresses
        if let Some(cpu_index) = unique_cpu_addresses
            .iter()
            .position(|&addr| addr == *phys_cpu_id)
        {
            let cpu_mask = 1usize << cpu_index; // Convert index to mask bit
            new_phys_cpu_sets.push(cpu_mask);
            debug!(
                "vCPU {} with phys_cpu_id 0x{:x} mapped to CPU index {} (mask: 0x{:x})",
                vm_cfg.id(),
                phys_cpu_id,
                cpu_index,
                cpu_mask
            );
        } else {
            error!(
                "vCPU {} with phys_cpu_id 0x{:x} not found in device tree!",
                vm_cfg.id(),
                phys_cpu_id
            );
        }
    }

    // Update phys_cpu_sets in VM configuration (if VM configuration supports setting)
    info!("Calculated phys_cpu_sets: {new_phys_cpu_sets:?}");

    vm_cfg
        .phys_cpu_ls_mut()
        .set_guest_cpu_sets(new_phys_cpu_sets);

    debug!(
        "vcpu_mappings: {:?}",
        vm_cfg.phys_cpu_ls_mut().get_vcpu_affinities_pcpu_ids()
    );
}

/// Add address mapping configuration for a device
fn add_device_address_config(
    vm_cfg: &mut AxVMConfig,
    node_name: &str,
    base_address: usize,
    size: usize,
    index: usize,
    prefix: Option<&str>,
) {
    // Only process devices with address information
    if size == 0 {
        return;
    }

    // Create a device configuration for each address segment
    let device_name = if index == 0 {
        match prefix {
            Some(p) => format!("{node_name}-{p}"),
            None => node_name.to_string(),
        }
    } else {
        match prefix {
            Some(p) => format!("{node_name}-{p}-region{index}"),
            None => format!("{node_name}-region{index}"),
        }
    };

    // Add new device configuration
    let pt_dev = axvm::config::PassThroughDeviceConfig {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
        irq_id: 0,
    };
    vm_cfg.add_pass_through_device(pt_dev);
}

/// Add ranges property configuration for PCIe devices
fn add_pci_ranges_config(vm_cfg: &mut AxVMConfig, node_name: &str, range: &PciRange, index: usize) {
    let base_address = range.cpu_address as usize;
    let size = range.size as usize;

    // Only process devices with address information
    if size == 0 {
        return;
    }

    // Create a device configuration for each address segment
    let prefix = match range.space {
        PciSpace::Configuration => "config",
        PciSpace::IO => "io",
        PciSpace::Memory32 => "mem32",
        PciSpace::Memory64 => "mem64",
    };

    let device_name = if index == 0 {
        format!("{node_name}-{prefix}")
    } else {
        format!("{node_name}-{prefix}-region{index}")
    };

    // Add new device configuration
    let pt_dev = axvm::config::PassThroughDeviceConfig {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
        irq_id: 0,
    };
    vm_cfg.add_pass_through_device(pt_dev);

    trace!(
        "Added PCIe passthrough device {}: base=0x{:x}, size=0x{:x}, space={:?}",
        node_name, base_address, size, range.space
    );
}

pub fn parse_passthrough_devices_address(vm_cfg: &mut AxVMConfig, dtb: &[u8]) {
    let devices = vm_cfg.pass_through_devices().to_vec();
    if !devices.is_empty() && devices[0].length != 0 {
        for (index, device) in devices.iter().enumerate() {
            add_device_address_config(
                vm_cfg,
                &device.name,
                device.base_gpa,
                device.length,
                index,
                None,
            );
        }
    } else {
        let fdt = Fdt::from_bytes(dtb)
            .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");

        // Clear existing passthrough device configurations
        vm_cfg.clear_pass_through_devices();

        // Traverse all device tree nodes
        for node in fdt.all_nodes() {
            // Skip root node
            if node.name() == "/" || node.name().starts_with("memory") {
                continue;
            }

            let node_name = node.name().to_string();

            // Check if it's a PCIe device node
            if node_name.starts_with("pcie@") || node_name.contains("pci") {
                // Process PCIe device's ranges property
                if let Some(pci) = node.clone().into_pci()
                    && let Ok(ranges) = pci.ranges()
                {
                    for (index, range) in ranges.enumerate() {
                        add_pci_ranges_config(vm_cfg, &node_name, &range, index);
                    }
                }

                // Process PCIe device's reg property (ECAM space)
                if let Some(reg_iter) = node.reg() {
                    for (index, reg) in reg_iter.enumerate() {
                        let base_address = reg.address as usize;
                        let size = reg.size.unwrap_or(0);

                        add_device_address_config(
                            vm_cfg,
                            &node_name,
                            base_address,
                            size,
                            index,
                            Some("ecam"),
                        );
                    }
                }
            } else {
                // Get device's reg property (process regular devices)
                if let Some(reg_iter) = node.reg() {
                    // Process all address segments of the device
                    for (index, reg) in reg_iter.enumerate() {
                        // Get device's address and size information
                        let base_address = reg.address as usize;
                        let size = reg.size.unwrap_or(0);

                        add_device_address_config(
                            vm_cfg,
                            &node_name,
                            base_address,
                            size,
                            index,
                            None,
                        );
                    }
                }
            }
        }
        trace!(
            "All passthrough devices: {:#x?}",
            vm_cfg.pass_through_devices()
        );
        debug!(
            "Finished parsing passthrough devices, total: {}",
            vm_cfg.pass_through_devices().len()
        );
    }
}

pub fn parse_vm_interrupt(vm_cfg: &mut AxVMConfig, dtb: &[u8]) {
    const GIC_PHANDLE: usize = 1;
    let fdt = Fdt::from_bytes(dtb)
        .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");

    for node in fdt.all_nodes() {
        let name = node.name();

        if name.starts_with("memory") {
            continue;
        }
        // Skip the interrupt controller, as we will use vGIC
        // TODO: filter with compatible property and parse its phandle from DT; maybe needs a second pass?
        else if name.starts_with("interrupt-controller")
            || name.starts_with("intc")
            || name.starts_with("its")
        {
            info!("skipping node {name} to use vGIC");
            continue;
        }

        // Collect all GIC_SPI interrupts and add them to vGIC
        if let Some(interrupts) = node.interrupts() {
            // TODO: skip non-GIC interrupt
            if let Some(parent) = node.interrupt_parent() {
                trace!("node: {}, intr parent: {}", name, parent.node.name());
                if let Some(phandle) = parent.node.phandle() {
                    if phandle.as_usize() != GIC_PHANDLE {
                        debug!(
                            "node: {}, intr parent: {}, phandle: 0x{:x} is not GIC!",
                            name,
                            parent.node.name(),
                            phandle.as_usize()
                        );
                    }
                } else {
                    warn!(
                        "node: {}, intr parent: {} no phandle!",
                        name,
                        parent.node.name(),
                    );
                }
            } else {
                warn!("node: {name} no interrupt parent!");
            }

            for interrupt in interrupts {
                // <GIC_SPI/GIC_PPI, IRQn, trigger_mode>
                for (k, v) in interrupt.enumerate() {
                    match k {
                        0 => {
                            if v == 0 {
                                trace!("node: {name}, GIC_SPI");
                            } else {
                                debug!("node: {name}, intr type: {v}, not GIC_SPI, not supported!");
                                break;
                            }
                        }
                        1 => {
                            trace!("node: {name}, interrupt id: 0x{v:x}");
                            vm_cfg.add_pass_through_spi(v);
                        }
                        2 => {
                            trace!("node: {name}, interrupt mode: 0x{v:x}");
                        }
                        _ => {
                            warn!("unknown interrupt property {k}:0x{v:x}")
                        }
                    }
                }
            }
        }
    }

    vm_cfg.add_pass_through_device(PassThroughDeviceConfig {
        name: "Fake Node".to_string(),
        base_gpa: 0x0,
        base_hpa: 0x0,
        length: 0x20_0000,
        irq_id: 0,
    });
}

pub fn update_provided_fdt(provided_dtb: &[u8], host_dtb: &[u8], crate_config: &AxVMCrateConfig) {
    let provided_fdt = Fdt::from_bytes(provided_dtb)
        .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");
    let host_fdt = Fdt::from_bytes(host_dtb)
        .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");
    let provided_dtb_data = update_cpu_node(&provided_fdt, &host_fdt, crate_config);
    crate_guest_fdt_with_cache(provided_dtb_data, crate_config);
}
