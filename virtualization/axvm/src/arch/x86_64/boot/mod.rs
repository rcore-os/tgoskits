//! x86_64 Linux, BIOS, UEFI, and MP-table image planning.

use alloc::format;

use ax_errno::{AxResult, ax_err_type};
use axvm_types::GuestPhysAddr;
use axvmconfig::{EmulatedDeviceType, VMBootProtocol, VmMemMappingType};

use super::X86_64Arch;
use crate::{
    architecture::BootImagePlatform,
    boot::{
        BootImageProvider, StaticVmImage,
        images::{ImageLoaderCore, load_vm_image_from_memory},
    },
};

mod boot_params;
mod linux;
mod linux_boot;
mod mptable;
mod multiboot;

pub struct ImageLoader<'a>(ImageLoaderCore<'a>);

impl<'a> ImageLoader<'a> {
    pub fn new(
        main_memory: crate::VMMemoryRegion,
        config: axvmconfig::AxVMCrateConfig,
        vm: crate::AxVMRef,
        provider: &'a dyn BootImageProvider,
    ) -> Self {
        Self(ImageLoaderCore::new(
            main_memory,
            config,
            vm,
            provider,
            None,
        ))
    }

    pub fn load(&mut self) -> AxResult {
        self.0.load()
    }
}

impl BootImagePlatform for X86_64Arch {
    fn default_boot_firmware_load_gpa(
        config: &axvmconfig::AxVMCrateConfig,
    ) -> Option<GuestPhysAddr> {
        const BUILT_IN_BIOS_LOAD_GPA: usize = 0x8000;

        (config.kernel.boot_firmware_path().is_none()
            && config.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot)
            .then_some(GuestPhysAddr::from(BUILT_IN_BIOS_LOAD_GPA))
    }

    fn load_images_from_memory(
        loader: &mut ImageLoaderCore<'_>,
        images: StaticVmImage,
    ) -> AxResult {
        if should_direct_boot_linux(&loader.config)
            && let Some(header) = detect_linux_image(images.kernel)
        {
            return load_linux_from_memory(loader, header, images.kernel, images.ramdisk);
        }

        load_vm_image_from_memory(images.kernel, loader.kernel_load_gpa, loader.vm.clone())?;
        if let Some(ramdisk) = images.ramdisk {
            loader.load_ramdisk_from_memory(ramdisk)?;
        }
        load_boot_image_from_memory(loader, images.bios)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_images_from_filesystem(loader: &mut ImageLoaderCore<'_>) -> AxResult {
        if should_direct_boot_linux(&loader.config) {
            let probe = crate::boot::images::fs::kernel_read(
                &loader.config,
                loader.provider,
                linux::HEADER_READ_SIZE,
            );
            if let Ok(data) = probe
                && let Some(header) = detect_linux_image(&data)
            {
                let kernel = crate::boot::images::fs::read_full_image(
                    &loader.config.kernel.kernel_path,
                    loader.provider,
                )?;
                return load_linux_from_filesystem(loader, header, &kernel);
            }
        }

        crate::boot::images::fs::load_vm_image(
            &loader.config.kernel.kernel_path,
            loader.kernel_load_gpa,
            loader.vm.clone(),
            loader.provider,
        )?;
        load_boot_image_from_filesystem(loader)?;
        if let Some(ramdisk_path) = &loader.config.kernel.ramdisk_path {
            loader.load_ramdisk_from_filesystem(ramdisk_path)?;
        }
        Ok(())
    }

    fn is_x86_linux_image_config(
        config: &axvmconfig::AxVMCrateConfig,
        provider: &dyn BootImageProvider,
    ) -> bool {
        if !should_direct_boot_linux(config) {
            return false;
        }
        match config.kernel.image_location.as_deref() {
            Some("memory") => provider
                .static_vm_images()
                .iter()
                .find(|image| image.id == config.base.id)
                .and_then(|image| detect_linux_image(image.kernel))
                .is_some(),
            #[cfg(any(feature = "fs", feature = "host-fs"))]
            Some("fs") => {
                crate::boot::images::fs::kernel_read(config, provider, linux::HEADER_READ_SIZE)
                    .ok()
                    .and_then(|image| detect_linux_image(&image))
                    .is_some()
            }
            _ => false,
        }
    }
}

fn load_linux_from_memory(
    loader: &mut ImageLoaderCore<'_>,
    header: linux::X86LinuxHeader,
    kernel: &[u8],
    ramdisk: Option<&[u8]>,
) -> AxResult {
    adjust_linux_dma_identity_layout(loader);
    let payload = linux_payload(&header, kernel)?;
    let initrd = ramdisk
        .map(|image| {
            loader
                .ramdisk_load_gpa()
                .map(|gpa| linux::X86LinuxRange::new(gpa.as_usize(), image.len()))
        })
        .transpose()?;
    let layout = linux::X86LinuxLoadLayout::new(
        &header,
        loader.kernel_load_gpa.as_usize(),
        payload.len(),
        initrd,
    )
    .map_err(linux_layout_error)?;

    load_linux_layout(loader, header, layout, kernel)?;
    load_vm_image_from_memory(payload, loader.kernel_load_gpa, loader.vm.clone())?;
    if let Some(ramdisk) = ramdisk {
        loader.load_ramdisk_from_memory(ramdisk)?;
    }
    Ok(())
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
fn load_linux_from_filesystem(
    loader: &mut ImageLoaderCore<'_>,
    header: linux::X86LinuxHeader,
    kernel: &[u8],
) -> AxResult {
    adjust_linux_dma_identity_layout(loader);
    let payload = linux_payload(&header, kernel)?;
    let initrd = loader
        .config
        .kernel
        .ramdisk_path
        .as_deref()
        .map(|path| -> AxResult<_> {
            let size = crate::boot::images::fs::image_size(path, loader.provider)?;
            Ok(linux::X86LinuxRange::new(
                loader.ramdisk_load_gpa()?.as_usize(),
                size,
            ))
        })
        .transpose()?;
    let layout = linux::X86LinuxLoadLayout::new(
        &header,
        loader.kernel_load_gpa.as_usize(),
        payload.len(),
        initrd,
    )
    .map_err(linux_layout_error)?;

    load_linux_layout(loader, header, layout, kernel)?;
    load_vm_image_from_memory(payload, loader.kernel_load_gpa, loader.vm.clone())?;
    if let Some(path) = &loader.config.kernel.ramdisk_path {
        loader.load_ramdisk_from_filesystem(path)?;
    }
    Ok(())
}

fn load_boot_image_from_memory(loader: &ImageLoaderCore<'_>, bios: Option<&[u8]>) -> AxResult {
    if !loader.config.kernel.enable_bios {
        return Ok(());
    }
    if let Some(bios) = bios {
        let load_gpa = loader
            .bios_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
        load_vm_image_from_memory(bios, load_gpa, loader.vm.clone())?;
        if should_patch_multiboot_info(&loader.config) {
            load_multiboot_info(loader, bios, load_gpa)?;
        }
        return Ok(());
    }

    if loader.config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        return load_uefi_from_configured_path(loader);
    }
    if should_load_default_boot_image(loader) {
        let load_gpa = builtin_bios_load_gpa(loader.bios_load_gpa)?;
        load_vm_image_from_memory(multiboot::DEFAULT_BIOS_IMAGE, load_gpa, loader.vm.clone())?;
        load_multiboot_info(loader, multiboot::DEFAULT_BIOS_IMAGE, load_gpa)?;
    }
    Ok(())
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
fn load_boot_image_from_filesystem(loader: &ImageLoaderCore<'_>) -> AxResult {
    if !loader.config.kernel.enable_bios {
        return Ok(());
    }
    if let Some(path) = loader.config.kernel.boot_firmware_path() {
        let load_gpa = loader
            .bios_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
        if should_patch_multiboot_info(&loader.config) {
            let bios = crate::boot::images::fs::read_full_image(path, loader.provider)?;
            validate_bios_patch_region(&bios)?;
            load_vm_image_from_memory(&bios, load_gpa, loader.vm.clone())?;
            load_multiboot_info(loader, &bios, load_gpa)
        } else {
            crate::boot::images::fs::load_vm_image(
                path,
                load_gpa,
                loader.vm.clone(),
                loader.provider,
            )
        }
    } else if should_load_default_boot_image(loader) {
        let load_gpa = builtin_bios_load_gpa(loader.bios_load_gpa)?;
        load_vm_image_from_memory(multiboot::DEFAULT_BIOS_IMAGE, load_gpa, loader.vm.clone())?;
        load_multiboot_info(loader, multiboot::DEFAULT_BIOS_IMAGE, load_gpa)
    } else {
        Ok(())
    }
}

fn load_uefi_from_configured_path(loader: &ImageLoaderCore<'_>) -> AxResult {
    let path = loader
        .config
        .kernel
        .boot_firmware_path()
        .ok_or_else(|| ax_err_type!(NotFound, "UEFI firmware image path is missed"))?;
    let load_gpa = loader
        .bios_load_gpa
        .ok_or_else(|| ax_err_type!(NotFound, "UEFI firmware load addr is missed"))?;
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    {
        crate::boot::images::fs::load_vm_image(path, load_gpa, loader.vm.clone(), loader.provider)
    }
    #[cfg(not(any(feature = "fs", feature = "host-fs")))]
    {
        let _ = (path, load_gpa);
        ax_errno::ax_err!(
            Unsupported,
            "UEFI firmware path requires the fs feature when no firmware image buffer is available"
        )
    }
}

fn adjust_linux_dma_identity_layout(loader: &mut ImageLoaderCore<'_>) {
    if !loader.main_memory.is_identical() {
        return;
    }
    let memory_base = loader.main_memory.gpa.as_usize();
    loader.kernel_load_gpa =
        GuestPhysAddr::from(memory_base + loader.config.kernel.kernel_load_addr);
    if let Some(ramdisk_load_addr) = loader.config.kernel.ramdisk_load_addr {
        loader.ramdisk_load_gpa = Some(GuestPhysAddr::from(memory_base + ramdisk_load_addr));
    }
    loader.vm.with_config(|config| {
        config.image_config.kernel_load_gpa = loader.kernel_load_gpa;
        if let Some(load_gpa) = loader.ramdisk_load_gpa
            && let Some(ramdisk) = config.image_config.ramdisk.as_mut()
        {
            ramdisk.load_gpa = load_gpa;
        }
    });
}

fn load_linux_layout(
    loader: &ImageLoaderCore<'_>,
    header: linux::X86LinuxHeader,
    layout: linux::X86LinuxLoadLayout,
    kernel: &[u8],
) -> AxResult {
    let boot_params = build_boot_params(loader, header, layout, kernel)?;
    let boot_stub = linux_boot::build_boot_image(&layout).map_err(|err| {
        ax_err_type!(
            InvalidInput,
            format!("failed to build x86 Linux boot stub: {err:?}")
        )
    })?;
    load_vm_image_from_memory(
        &boot_params,
        layout.boot_params.start.into(),
        loader.vm.clone(),
    )?;
    load_vm_image_from_memory(&boot_stub, layout.boot_stub.start.into(), loader.vm.clone())?;
    load_vm_image_from_memory(
        &mptable::build(),
        mptable::MP_TABLE_GPA.into(),
        loader.vm.clone(),
    )?;
    let entry = GuestPhysAddr::from(linux_boot::DEFAULT_LINUX_BOOT_LOAD_GPA);
    loader.vm.with_config(|config| {
        config.cpu_config.bsp_entry = entry;
        config.cpu_config.ap_entry = entry;
    });
    Ok(())
}

fn build_boot_params(
    loader: &ImageLoaderCore<'_>,
    header: linux::X86LinuxHeader,
    layout: linux::X86LinuxLoadLayout,
    kernel: &[u8],
) -> AxResult<[u8; linux::BOOT_PARAMS_SIZE]> {
    let mut builder = boot_params::BootParamsBuilder::new(
        kernel,
        header,
        layout,
        linux::X86LinuxRange::new(loader.main_memory.gpa.as_usize(), loader.main_memory.size()),
    );
    let command_line = loader.config.kernel.cmdline.as_deref().ok_or_else(|| {
        ax_err_type!(
            InvalidInput,
            "x86 Linux direct boot requires kernel.cmdline in the VM config"
        )
    })?;
    builder.set_command_line(command_line).map_err(|err| {
        ax_err_type!(
            InvalidInput,
            format!("invalid x86 Linux command line: {err:?}")
        )
    })?;
    for memory in &loader.config.kernel.memory_regions {
        if memory.map_type == VmMemMappingType::MapAlloc {
            builder.add_ram_range(linux::X86LinuxRange::new(memory.gpa, memory.size));
        }
    }
    for device in &loader.config.devices.passthrough_devices {
        builder.add_reserved_range(linux::X86LinuxRange::new(device.base_gpa, device.length));
    }
    for address in &loader.config.devices.passthrough_addresses {
        builder.add_reserved_range(linux::X86LinuxRange::new(address.base_gpa, address.length));
    }
    for device in &loader.config.devices.emu_devices {
        if matches!(device.emu_type, EmulatedDeviceType::X86IoApic) {
            builder.add_reserved_range(linux::X86LinuxRange::new(device.base_gpa, device.length));
        }
    }
    builder.add_reserved_range(mptable::reserved_range());
    builder.build().map_err(|err| {
        ax_err_type!(
            InvalidInput,
            format!("failed to build x86 boot_params: {err:?}")
        )
    })
}

fn load_multiboot_info(
    loader: &ImageLoaderCore<'_>,
    bios_image: &[u8],
    bios_load_gpa: GuestPhysAddr,
) -> AxResult {
    const INFO_GPA: usize = 0x6000;
    const MMAP_GPA: usize = 0x6040;
    let mem_base = loader.main_memory.gpa.as_usize() as u64;
    let mem_size = loader.main_memory.size() as u64;
    let mut info = [0u8; 52];
    write_u32(&mut info, 0, (1 << 0) | (1 << 6));
    write_u32(&mut info, 4, 639);
    write_u32(
        &mut info,
        8,
        (mem_size.saturating_sub(0x100000) / 1024) as u32,
    );
    write_u32(&mut info, 44, 24);
    write_u32(&mut info, 48, MMAP_GPA as u32);
    let mut mmap = [0u8; 24];
    write_u32(&mut mmap, 0, 20);
    write_u64(&mut mmap, 4, mem_base);
    write_u64(&mut mmap, 12, mem_size);
    write_u32(&mut mmap, 20, 1);
    validate_bios_patch_region(bios_image)?;
    load_vm_image_from_memory(&info, INFO_GPA.into(), loader.vm.clone())?;
    load_vm_image_from_memory(&mmap, MMAP_GPA.into(), loader.vm.clone())?;
    load_vm_image_from_memory(
        &(INFO_GPA as u32).to_le_bytes(),
        (bios_load_gpa.as_usize() + multiboot::AXVM_BIOS_EBX_IMM_OFFSET).into(),
        loader.vm.clone(),
    )
}

fn should_direct_boot_linux(config: &axvmconfig::AxVMCrateConfig) -> bool {
    !config.kernel.enable_bios && config.kernel.effective_boot_protocol() == VMBootProtocol::Direct
}

fn should_patch_multiboot_info(config: &axvmconfig::AxVMCrateConfig) -> bool {
    config.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot
}

fn should_load_default_boot_image(loader: &ImageLoaderCore<'_>) -> bool {
    loader.config.kernel.enable_bios
        && loader.config.kernel.boot_firmware_path().is_none()
        && loader.config.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot
}

fn detect_linux_image(image: &[u8]) -> Option<linux::X86LinuxHeader> {
    linux::X86LinuxHeader::parse(image).ok()
}

fn linux_payload<'a>(header: &linux::X86LinuxHeader, image: &'a [u8]) -> AxResult<&'a [u8]> {
    image.get(header.payload_offset()..).ok_or_else(|| {
        ax_err_type!(
            InvalidInput,
            format!(
                "x86 Linux bzImage payload offset {:#x} exceeds image size {:#x}",
                header.payload_offset(),
                image.len()
            )
        )
    })
}

fn linux_layout_error(err: linux::X86LinuxLayoutError) -> ax_errno::AxError {
    ax_err_type!(
        InvalidInput,
        format!("invalid x86 Linux memory layout: {err:?}")
    )
}

fn builtin_bios_load_gpa(configured: Option<GuestPhysAddr>) -> AxResult<GuestPhysAddr> {
    let default = GuestPhysAddr::from(multiboot::DEFAULT_BIOS_LOAD_GPA);
    match configured {
        Some(gpa) if gpa != default => Err(ax_err_type!(
            InvalidInput,
            format!(
                "built-in x86 BIOS must be loaded at GPA {:#x}, but bios_load_addr is {:#x}",
                default.as_usize(),
                gpa.as_usize()
            )
        )),
        Some(gpa) => Ok(gpa),
        None => Ok(default),
    }
}

fn validate_bios_patch_region(bios: &[u8]) -> AxResult {
    let patch_end = multiboot::AXVM_BIOS_EBX_IMM_OFFSET + core::mem::size_of::<u32>();
    if bios.len() < patch_end
        || bios[multiboot::AXVM_BIOS_EBX_IMM_OFFSET - 1] != multiboot::MOV_EBX_IMM32_OPCODE
    {
        return Err(ax_err_type!(
            InvalidInput,
            "x86 BIOS image does not match the AxVM multiboot patch layout"
        ));
    }
    Ok(())
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_bios_uses_default_gpa_when_unspecified() {
        assert_eq!(
            builtin_bios_load_gpa(None).unwrap(),
            GuestPhysAddr::from(multiboot::DEFAULT_BIOS_LOAD_GPA)
        );
    }

    #[test]
    fn built_in_bios_accepts_explicit_default_gpa() {
        let default = GuestPhysAddr::from(multiboot::DEFAULT_BIOS_LOAD_GPA);
        assert_eq!(builtin_bios_load_gpa(Some(default)).unwrap(), default);
    }

    #[test]
    fn built_in_bios_rejects_non_default_gpa() {
        let invalid = GuestPhysAddr::from(multiboot::DEFAULT_BIOS_LOAD_GPA + 0x1000);
        assert!(builtin_bios_load_gpa(Some(invalid)).is_err());
    }

    #[test]
    fn legacy_bios_config_uses_multiboot_patch() {
        let mut config = axvmconfig::AxVMCrateConfig::default();
        config.kernel.enable_bios = true;
        assert!(should_patch_multiboot_info(&config));
    }

    #[test]
    fn uefi_config_skips_multiboot_patch() {
        let mut config = axvmconfig::AxVMCrateConfig::default();
        config.kernel.enable_bios = true;
        config.kernel.boot_protocol = Some(VMBootProtocol::Uefi);
        assert!(!should_patch_multiboot_info(&config));
    }

    #[test]
    fn linux_direct_boot_requires_direct_protocol_without_bios() {
        let mut config = axvmconfig::AxVMCrateConfig::default();
        assert!(should_direct_boot_linux(&config));

        config.kernel.enable_bios = true;
        assert!(!should_direct_boot_linux(&config));

        config.kernel.boot_protocol = Some(VMBootProtocol::Uefi);
        assert!(!should_direct_boot_linux(&config));

        config.kernel.boot_protocol = Some(VMBootProtocol::Direct);
        assert!(!should_direct_boot_linux(&config));

        config.kernel.enable_bios = false;
        assert!(should_direct_boot_linux(&config));
    }
}
