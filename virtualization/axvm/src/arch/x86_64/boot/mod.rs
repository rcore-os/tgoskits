//! x86_64 Linux, BIOS, UEFI, and MP-table image planning.

use alloc::format;

use axvm_types::GuestPhysAddr;
use axvmconfig::{EmulatedDeviceType, VMBootProtocol, VmMemMappingType};

use super::X86_64Arch;
#[cfg(not(any(feature = "fs", feature = "host-fs")))]
use crate::ax_err;
use crate::{
    AxVmError, AxVmResult,
    architecture::BootImagePlatform,
    ax_err_type,
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

const OVMF_PROFILE_NAME: &str = "qemu_x86_64_axvisor_ovmf_debug";
const OVMF_CODE_LOAD_GPA: usize = 0xffc8_4000;
const OVMF_CODE_SIZE: usize = 0x37c000;
const OVMF_RESET_VECTOR: usize = 0xffff_fff0;

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

    pub fn load(&mut self) -> AxVmResult {
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
    ) -> AxVmResult {
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
    fn load_images_from_filesystem(loader: &mut ImageLoaderCore<'_>) -> AxVmResult {
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
) -> AxVmResult {
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
) -> AxVmResult {
    adjust_linux_dma_identity_layout(loader);
    let payload = linux_payload(&header, kernel)?;
    let initrd = loader
        .config
        .kernel
        .ramdisk_path
        .as_deref()
        .map(|path| -> AxVmResult<_> {
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

fn load_boot_image_from_memory(loader: &ImageLoaderCore<'_>, bios: Option<&[u8]>) -> AxVmResult {
    if !loader.config.kernel.enable_bios {
        return Ok(());
    }
    if let Some(bios) = bios {
        let load_gpa = loader
            .bios_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
        if loader.config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
            validate_uefi_firmware_layout(load_gpa, bios.len(), loader.config.kernel.entry_point)?;
        }
        load_vm_image_from_memory(bios, load_gpa, loader.vm.clone())?;
        if loader.config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
            record_uefi_firmware_loaded(loader, "<static image>", bios.len(), load_gpa);
        }
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
fn load_boot_image_from_filesystem(loader: &ImageLoaderCore<'_>) -> AxVmResult {
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
            let size =
                query_uefi_firmware_size(loader.config.kernel.effective_boot_protocol(), || {
                    crate::boot::images::fs::image_size(path, loader.provider)
                })?;
            if let Some(size) = size {
                validate_uefi_firmware_layout(load_gpa, size, loader.config.kernel.entry_point)?;
            }
            crate::boot::images::fs::load_vm_image(
                path,
                load_gpa,
                loader.vm.clone(),
                loader.provider,
            )?;
            if let Some(size) = size {
                record_uefi_firmware_loaded(loader, path, size, load_gpa);
            }
            Ok(())
        }
    } else if should_load_default_boot_image(loader) {
        let load_gpa = builtin_bios_load_gpa(loader.bios_load_gpa)?;
        load_vm_image_from_memory(multiboot::DEFAULT_BIOS_IMAGE, load_gpa, loader.vm.clone())?;
        load_multiboot_info(loader, multiboot::DEFAULT_BIOS_IMAGE, load_gpa)
    } else {
        Ok(())
    }
}

fn load_uefi_from_configured_path(loader: &ImageLoaderCore<'_>) -> AxVmResult {
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
        let size = crate::boot::images::fs::image_size(path, loader.provider)?;
        validate_uefi_firmware_layout(load_gpa, size, loader.config.kernel.entry_point)?;
        crate::boot::images::fs::load_vm_image(path, load_gpa, loader.vm.clone(), loader.provider)?;
        record_uefi_firmware_loaded(loader, path, size, load_gpa);
        Ok(())
    }
    #[cfg(not(any(feature = "fs", feature = "host-fs")))]
    {
        let _ = (path, load_gpa);
        ax_err!(
            Unsupported,
            "UEFI firmware path requires the fs feature when no firmware image buffer is available"
        )
    }
}

#[cfg(any(feature = "fs", feature = "host-fs", test))]
fn query_uefi_firmware_size(
    protocol: VMBootProtocol,
    query_size: impl FnOnce() -> AxVmResult<usize>,
) -> AxVmResult<Option<usize>> {
    if protocol == VMBootProtocol::Uefi {
        query_size().map(Some)
    } else {
        Ok(None)
    }
}

fn validate_uefi_firmware_layout(
    load_gpa: GuestPhysAddr,
    size: usize,
    entry_point: usize,
) -> AxVmResult {
    if load_gpa.as_usize() != OVMF_CODE_LOAD_GPA {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "x86 UEFI profile {OVMF_PROFILE_NAME} requires CODE GPA {OVMF_CODE_LOAD_GPA:#x}, \
                 but configured {:#x}",
                load_gpa.as_usize()
            )
        ));
    }
    if size != OVMF_CODE_SIZE {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "x86 UEFI profile {OVMF_PROFILE_NAME} requires CODE size {OVMF_CODE_SIZE:#x}, but \
                 image size is {size:#x}"
            )
        ));
    }
    if entry_point != OVMF_RESET_VECTOR {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "x86 UEFI profile {OVMF_PROFILE_NAME} requires reset vector \
                 {OVMF_RESET_VECTOR:#x}, but entry_point is {:#x}",
                entry_point
            )
        ));
    }
    Ok(())
}

fn record_uefi_firmware_loaded(
    loader: &ImageLoaderCore<'_>,
    path: &str,
    size: usize,
    load_gpa: GuestPhysAddr,
) {
    let start = load_gpa.as_usize();
    let end = start + size - 1;
    info!(
        "VM[{}] loaded x86 UEFI firmware: profile={} path={} size={:#x} GPA={:#x}..={:#x} \
         reset_vector={:#x}",
        loader.config.base.id, OVMF_PROFILE_NAME, path, size, start, end, OVMF_RESET_VECTOR
    );
    super::guest_serial::mark_fixed_ovmf_profile_loaded(&loader.vm);
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
) -> AxVmResult {
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
) -> AxVmResult<[u8; linux::BOOT_PARAMS_SIZE]> {
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
) -> AxVmResult {
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

fn linux_payload<'a>(header: &linux::X86LinuxHeader, image: &'a [u8]) -> AxVmResult<&'a [u8]> {
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

fn linux_layout_error(err: linux::X86LinuxLayoutError) -> AxVmError {
    ax_err_type!(
        InvalidInput,
        format!("invalid x86 Linux memory layout: {err:?}")
    )
}

fn builtin_bios_load_gpa(configured: Option<GuestPhysAddr>) -> AxVmResult<GuestPhysAddr> {
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

fn validate_bios_patch_region(bios: &[u8]) -> AxVmResult {
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
    fn non_uefi_firmware_does_not_query_file_size() {
        let size = query_uefi_firmware_size(VMBootProtocol::Multiboot, || {
            panic!("non-UEFI firmware must not query file size")
        })
        .unwrap();

        assert_eq!(size, None);
    }

    #[test]
    fn uefi_firmware_queries_file_size() {
        let size = query_uefi_firmware_size(VMBootProtocol::Uefi, || Ok(OVMF_CODE_SIZE)).unwrap();

        assert_eq!(size, Some(OVMF_CODE_SIZE));
    }

    #[test]
    fn fixed_ovmf_profile_constants_cover_the_reset_vector() {
        let code_end = OVMF_CODE_LOAD_GPA + OVMF_CODE_SIZE;

        assert_eq!(code_end, 0x1_0000_0000);
        assert!(OVMF_RESET_VECTOR >= OVMF_CODE_LOAD_GPA);
        assert!(OVMF_RESET_VECTOR + 16 <= code_end);
    }

    #[test]
    fn fixed_ovmf_profile_rejects_wrong_layout_or_entry() {
        let expected_gpa = GuestPhysAddr::from(OVMF_CODE_LOAD_GPA);

        assert!(
            validate_uefi_firmware_layout(expected_gpa, OVMF_CODE_SIZE, OVMF_RESET_VECTOR).is_ok()
        );
        assert!(
            validate_uefi_firmware_layout(
                GuestPhysAddr::from(OVMF_CODE_LOAD_GPA - 0x1000),
                OVMF_CODE_SIZE,
                OVMF_RESET_VECTOR,
            )
            .is_err()
        );
        assert!(
            validate_uefi_firmware_layout(expected_gpa, OVMF_CODE_SIZE - 1, OVMF_RESET_VECTOR)
                .is_err()
        );
        assert!(
            validate_uefi_firmware_layout(expected_gpa, OVMF_CODE_SIZE, OVMF_RESET_VECTOR - 0x10)
                .is_err()
        );
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
