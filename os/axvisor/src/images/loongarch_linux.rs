use alloc::vec::Vec;

use ax_errno::{AxResult, ax_err_type};
use axvm::{AxVMRef, VMMemoryRegion};
use axvmconfig::AxVMCrateConfig;

use crate::images::load_vm_image_from_memory;

const BOOTINFO_GPA: usize = 0x0008_0000;
const DTB_GPA: usize = 0x0010_0000;
const CMDLINE_GPA: usize = BOOTINFO_GPA;
const VENDOR_GPA: usize = BOOTINFO_GPA + 0x1000;
const SYSTAB_GPA: usize = BOOTINFO_GPA + 0x2000;
const CONFIG_TABLE_GPA: usize = BOOTINFO_GPA + 0x3000;
const BOOT_MEMMAP_GPA: usize = BOOTINFO_GPA + 0x4000;
const INITRD_TABLE_GPA: usize = BOOTINFO_GPA + 0x5000;
const BOOT_STACK_GPA: usize = 0x000f_0000;
const BOOT_STACK_SIZE: usize = 0x0001_0000;
const EFI_MEMORY_WB: u64 = 1 << 3;
const EFI_SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;
const EFI_2_10_SYSTEM_TABLE_REVISION: u32 = (2 << 16) | 10;
const EFI_MEMORY_DESCRIPTOR_VERSION: u32 = 1;
const EFI_LOADER_DATA: u32 = 2;
const EFI_CONVENTIONAL_MEMORY: u32 = 7;
const EFI_MEMORY_DESC_SIZE: usize = 40;

pub fn setup_bootinfo(vm: AxVMRef, crate_config: &AxVMCrateConfig) -> AxResult {
    let bootargs = crate_config
        .kernel
        .cmdline
        .clone()
        .ok_or_else(|| ax_err_type!(InvalidInput, "LoongArch Linux requires kernel.cmdline"))?;
    if !has_cmdline_arg(&bootargs, "init") && !has_cmdline_arg(&bootargs, "rdinit") {
        warn!("LoongArch Linux cmdline has no init= or rdinit= argument");
    }
    if !has_cmdline_arg(&bootargs, "console") {
        warn!("LoongArch Linux cmdline has no console= argument");
    }

    let mut cmdline = [0u8; 4096];
    let bytes = bootargs.as_bytes();
    let len = bytes.len().min(cmdline.len() - 1);
    cmdline[..len].copy_from_slice(&bytes[..len]);
    load_vm_image_from_memory(&cmdline, CMDLINE_GPA.into(), vm.clone())?;

    let mut vendor = [0u8; 64];
    let vendor_utf16 = [
        b'A' as u16,
        b'x' as u16,
        b'v' as u16,
        b'i' as u16,
        b's' as u16,
        b'o' as u16,
        b'r' as u16,
        0,
    ];
    for (idx, ch) in vendor_utf16.iter().enumerate() {
        vendor[idx * 2..idx * 2 + 2].copy_from_slice(&ch.to_le_bytes());
    }
    load_vm_image_from_memory(&vendor, VENDOR_GPA.into(), vm.clone())?;

    let vm_regions = vm.memory_regions();
    let regions = efi_memory_regions(&vm_regions, crate_config);
    let map_size = regions.len() * EFI_MEMORY_DESC_SIZE;
    let mut boot_memmap = Vec::new();
    write_u64(&mut boot_memmap, map_size as u64);
    write_u64(&mut boot_memmap, EFI_MEMORY_DESC_SIZE as u64);
    write_u32(&mut boot_memmap, EFI_MEMORY_DESCRIPTOR_VERSION);
    write_u32(&mut boot_memmap, 0);
    write_u64(&mut boot_memmap, 0);
    write_u64(&mut boot_memmap, map_size as u64);
    for region in &regions {
        let gpa = region.gpa.as_usize() as u64;
        let pages = (region.size() as u64).div_ceil(4096);
        let mem_type = match region.gpa.as_usize() {
            BOOTINFO_GPA | DTB_GPA => EFI_LOADER_DATA,
            _ => EFI_CONVENTIONAL_MEMORY,
        };
        write_efi_memory_desc(&mut boot_memmap, gpa, pages, mem_type);
    }
    load_vm_image_from_memory(&boot_memmap, BOOT_MEMMAP_GPA.into(), vm.clone())?;

    let (dtb_addr, initrd_range) = vm.with_config(|config| {
        let initrd_range = config.image_config.ramdisk.as_ref().and_then(|ramdisk| {
            let size = ramdisk.size?;
            let start = ramdisk.load_gpa.as_usize() as u64;
            Some((start, size as u64))
        });
        (config.image_config.dtb_load_gpa, initrd_range)
    });
    let mut config_table = Vec::new();
    write_guid(
        &mut config_table,
        0x800f683f,
        0xd08b,
        0x423a,
        &[0xa2, 0x93, 0x96, 0x5c, 0x3c, 0x6f, 0xe2, 0xb4],
    );
    write_u64(&mut config_table, BOOT_MEMMAP_GPA as u64);
    if let Some((base, size)) = initrd_range {
        let mut initrd_table = Vec::new();
        write_u64(&mut initrd_table, base);
        write_u64(&mut initrd_table, size);
        load_vm_image_from_memory(&initrd_table, INITRD_TABLE_GPA.into(), vm.clone())?;

        write_guid(
            &mut config_table,
            0x5568e427,
            0x68fc,
            0x4f3d,
            &[0xac, 0x74, 0xca, 0x55, 0x52, 0x31, 0xcc, 0x68],
        );
        write_u64(&mut config_table, INITRD_TABLE_GPA as u64);
    }
    if let Some(dtb_addr) = dtb_addr {
        write_guid(
            &mut config_table,
            0xb1b621d5,
            0xf19c,
            0x41a5,
            &[0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0],
        );
        write_u64(&mut config_table, dtb_addr.as_usize() as u64);
    }
    load_vm_image_from_memory(&config_table, CONFIG_TABLE_GPA.into(), vm.clone())?;

    let mut systab = Vec::new();
    write_u64(&mut systab, EFI_SYSTEM_TABLE_SIGNATURE);
    write_u32(&mut systab, EFI_2_10_SYSTEM_TABLE_REVISION);
    write_u32(&mut systab, 120);
    write_u32(&mut systab, 0);
    write_u32(&mut systab, 0);
    write_u64(&mut systab, VENDOR_GPA as u64);
    write_u32(&mut systab, 1);
    write_u32(&mut systab, 0);
    for _ in 0..8 {
        write_u64(&mut systab, 0);
    }
    write_u64(&mut systab, (config_table.len() / 24) as u64);
    write_u64(&mut systab, CONFIG_TABLE_GPA as u64);
    load_vm_image_from_memory(&systab, SYSTAB_GPA.into(), vm.clone())?;

    let boot_stack = [0u8; BOOT_STACK_SIZE];
    load_vm_image_from_memory(&boot_stack, BOOT_STACK_GPA.into(), vm.clone())?;

    vm.with_config(|config| {
        config.cpu_config.boot_args = [1, CMDLINE_GPA, SYSTAB_GPA];
        config.cpu_config.boot_stack_top = BOOT_STACK_GPA + BOOT_STACK_SIZE;
    });
    info!(
        "LoongArch Linux bootinfo: cmdline={:#x}, systab={:#x}, dtb={:?}, initrd={:?}, \
         boot_stack=[{:#x}, {:#x})",
        CMDLINE_GPA,
        SYSTAB_GPA,
        dtb_addr,
        initrd_range,
        BOOT_STACK_GPA,
        BOOT_STACK_GPA + BOOT_STACK_SIZE
    );

    Ok(())
}

fn efi_memory_regions<'a>(
    regions: &'a [VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
) -> Vec<&'a VMMemoryRegion> {
    let configured_region_count = if crate_config.kernel.configured_memory_region_count == 0 {
        crate_config.kernel.memory_regions.len()
    } else {
        crate_config
            .kernel
            .configured_memory_region_count
            .min(crate_config.kernel.memory_regions.len())
    };

    crate_config
        .kernel
        .memory_regions
        .iter()
        .take(configured_region_count)
        .filter_map(|cfg| {
            let region = regions
                .iter()
                .find(|region| region.gpa.as_usize() == cfg.gpa && region.size() == cfg.size);
            if region.is_none() {
                warn!(
                    "Configured guest memory region [{:#x}, {:#x}) is not mapped; skipping EFI memmap entry",
                    cfg.gpa,
                    cfg.gpa + cfg.size
                );
            }
            region
        })
        .collect()
}

fn has_cmdline_arg(cmdline: &str, name: &str) -> bool {
    cmdline
        .split_ascii_whitespace()
        .any(|arg| arg.split_once('=').is_some_and(|(key, _)| key == name))
}

fn write_efi_memory_desc(buf: &mut Vec<u8>, phys_addr: u64, num_pages: u64, mem_type: u32) {
    write_u32(buf, mem_type);
    write_u32(buf, 0);
    write_u64(buf, phys_addr);
    write_u64(buf, 0);
    write_u64(buf, num_pages);
    write_u64(buf, EFI_MEMORY_WB);
}

fn write_guid(buf: &mut Vec<u8>, a: u32, b: u16, c: u16, d: &[u8; 8]) {
    write_u32(buf, a);
    write_u16(buf, b);
    write_u16(buf, c);
    buf.extend_from_slice(d);
}

fn write_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}
