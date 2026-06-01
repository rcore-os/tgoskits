// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::format;

use ax_errno::{AxResult, ax_err, ax_err_type};
use axvm::config::AxVMCrateConfig;
#[cfg(target_arch = "x86_64")]
use axvm::config::{VMBootProtocol, VmMemMappingType};
use byte_unit::Byte;

use axvm::{AxVMRef, GuestPhysAddr, VMMemoryRegion};

use crate::config::{get_vm_dtb_arc, vmcfg};

mod linux;
#[cfg(target_arch = "x86_64")]
mod x86;
#[cfg(target_arch = "x86_64")]
use x86::boot_params as x86_boot_params;
#[cfg(target_arch = "x86_64")]
use x86::linux as x86_linux;
#[cfg(target_arch = "x86_64")]
use x86::linux_boot as x86_linux_boot;
#[cfg(target_arch = "x86_64")]
use x86::mptable as x86_mptable;
#[cfg(target_arch = "x86_64")]
use x86::multiboot as x86_boot;

#[cfg(target_arch = "x86_64")]
pub fn is_x86_linux_image_config(config: &AxVMCrateConfig) -> bool {
    if config.kernel.enable_bios {
        return false;
    }

    match config.kernel.image_location.as_deref() {
        Some("memory") => with_memory_image(config, detect_x86_linux_image).is_some(),
        #[cfg(feature = "fs")]
        Some("fs") => fs::kernel_read(config, x86_linux::HEADER_READ_SIZE)
            .ok()
            .and_then(|data| detect_x86_linux_image(&data))
            .is_some(),
        _ => false,
    }
}

pub fn get_image_header(config: &AxVMCrateConfig) -> Option<linux::Header> {
    match config.kernel.image_location.as_deref() {
        Some("memory") => with_memory_image(config, linux::Header::parse).flatten(),
        #[cfg(feature = "fs")]
        Some("fs") => {
            let read_size = linux::Header::hdr_size();
            let data = fs::kernel_read(config, read_size).ok()?;
            linux::Header::parse(&data)
        }
        _ => None,
    }
}

fn with_memory_image<F, R>(config: &AxVMCrateConfig, func: F) -> Option<R>
where
    F: FnOnce(&[u8]) -> R,
{
    let vm_imags = vmcfg::get_memory_images()
        .iter()
        .find(|&v| v.id == config.base.id)?;

    Some(func(vm_imags.kernel))
}

fn memory_images_for_vm(config: &AxVMCrateConfig) -> AxResult<&'static vmcfg::MemoryImage> {
    vmcfg::get_memory_images()
        .iter()
        .find(|&v| v.id == config.base.id)
        .ok_or_else(|| {
            ax_err_type!(
                NotFound,
                "VM images are missing; pass VM configs with AXVISOR_VM_CONFIGS"
            )
        })
}

pub struct ImageLoader {
    main_memory: VMMemoryRegion,
    vm: AxVMRef,
    config: AxVMCrateConfig,
    kernel_load_gpa: GuestPhysAddr,
    bios_load_gpa: Option<GuestPhysAddr>,
    dtb_load_gpa: Option<GuestPhysAddr>,
    ramdisk_load_gpa: Option<GuestPhysAddr>,
}

impl ImageLoader {
    pub fn new(main_memory: VMMemoryRegion, config: AxVMCrateConfig, vm: AxVMRef) -> Self {
        Self {
            main_memory,
            vm,
            config,
            kernel_load_gpa: GuestPhysAddr::default(),
            bios_load_gpa: None,
            dtb_load_gpa: None,
            ramdisk_load_gpa: None,
        }
    }

    pub fn load(&mut self) -> AxResult {
        self.config.kernel.validate_boot_config()?;
        info!(
            "Loading VM[{}] images into memory region: gpa={:#x}, hva={:#x}, size={:#}",
            self.vm.id(),
            self.main_memory.gpa,
            self.main_memory.hva,
            Byte::from(self.main_memory.size())
        );

        self.vm.with_config(|config| {
            self.kernel_load_gpa = config.image_config.kernel_load_gpa;
            self.dtb_load_gpa = config.image_config.dtb_load_gpa;
            self.bios_load_gpa = config.image_config.bios_load_gpa;
            self.ramdisk_load_gpa = config.image_config.ramdisk.as_ref().map(|r| r.load_gpa);
        });

        match self.config.kernel.image_location.as_deref() {
            Some("memory") => self.load_vm_images_from_memory(),
            #[cfg(feature = "fs")]
            Some("fs") => fs::load_vm_images_from_filesystem(self),
            _ => ax_err!(
                InvalidInput,
                "Unsupported image_location; use \"memory\" or enable fs feature for \"fs\""
            ),
        }
    }

    /// Load VM images from memory
    /// into the guest VM's memory space based on the VM configuration.
    fn load_vm_images_from_memory(&mut self) -> AxResult {
        info!("Loading VM[{}] images from memory", self.config.base.id);

        let vm_imags = memory_images_for_vm(&self.config)?;

        #[cfg(target_arch = "x86_64")]
        if let Some(header) = detect_x86_linux_image(vm_imags.kernel) {
            return self.load_x86_linux_images_from_memory(
                header,
                vm_imags.kernel,
                vm_imags.ramdisk,
            );
        }

        load_vm_image_from_memory(vm_imags.kernel, self.kernel_load_gpa, self.vm.clone())?;

        // Load Ramdisk image and record its size before regenerating the DTB.
        if let Some(buffer) = vm_imags.ramdisk {
            self.load_ramdisk_from_memory(buffer)?;
        }
        // Load DTB image
        let vm_config = crate::config::build_axvm_config(&self.config);

        if let Some(dtb_arc) = get_vm_dtb_arc(&vm_config) {
            let _dtb_slice: &[u8] = &dtb_arc;
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            {
                if let Some(dtb_src) = core::ptr::NonNull::new(_dtb_slice.as_ptr() as *mut u8) {
                    crate::fdt::update_fdt(
                        dtb_src,
                        _dtb_slice.len(),
                        self.vm.clone(),
                        &self.config,
                    )?;
                } else {
                    return ax_err!(InvalidData, "Guest DTB pointer is null");
                }
            }
            #[cfg(target_arch = "loongarch64")]
            {
                let dtb_load_gpa = self
                    .dtb_load_gpa
                    .ok_or_else(|| ax_err_type!(NotFound, "DTB load address is missing"))?;
                load_vm_image_from_memory(_dtb_slice, dtb_load_gpa, self.vm.clone())?;
            }
        } else {
            #[cfg(any(target_arch = "loongarch64", target_arch = "riscv64"))]
            if let Some(buffer) = vm_imags.dtb {
                let dtb_load_gpa = self
                    .dtb_load_gpa
                    .ok_or_else(|| ax_err_type!(NotFound, "DTB load address is missing"))?;
                load_vm_image_from_memory(buffer, dtb_load_gpa, self.vm.clone())?;
            } else {
                info!("dtb_load_gpa not provided");
            }

            #[cfg(not(target_arch = "riscv64"))]
            {
                info!("dtb_load_gpa not provided");
            }
        }

        self.load_boot_image_from_memory(vm_imags.bios)?;

        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn load_x86_linux_images_from_memory(
        &mut self,
        header: x86_linux::X86LinuxHeader,
        kernel: &[u8],
        ramdisk: Option<&[u8]>,
    ) -> AxResult {
        self.adjust_x86_linux_dma_identity_layout()?;
        let payload = x86_linux_payload(&header, kernel)?;
        let initrd = if let Some(ramdisk) = ramdisk {
            Some(x86_linux::X86LinuxRange::new(
                self.ramdisk_load_gpa()?.as_usize(),
                ramdisk.len(),
            ))
        } else {
            None
        };
        let layout = x86_linux::X86LinuxLoadLayout::new(
            &header,
            self.kernel_load_gpa.as_usize(),
            payload.len(),
            initrd,
        )
        .map_err(x86_linux_layout_error)?;

        self.load_x86_linux_layout(header, layout, kernel)?;
        load_vm_image_from_memory(payload, self.kernel_load_gpa, self.vm.clone())?;

        if let Some(buffer) = ramdisk {
            self.load_ramdisk_from_memory(buffer)?;
        }

        Ok(())
    }

    fn load_boot_image_from_memory(&self, bios: Option<&[u8]>) -> AxResult {
        if !self.config.kernel.enable_bios {
            return Ok(());
        }

        if let Some(buffer) = bios {
            let load_gpa = self
                .bios_load_gpa
                .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
            load_vm_image_from_memory(buffer, load_gpa, self.vm.clone())?;
            #[cfg(target_arch = "x86_64")]
            if should_patch_x86_multiboot_info(&self.config) {
                self.load_x86_multiboot_info(buffer, load_gpa)?;
            }
            return Ok(());
        }

        #[cfg(target_arch = "x86_64")]
        if self.config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
            let firmware_path = self.config.kernel.boot_firmware_path().ok_or_else(|| {
                ax_errno::ax_err_type!(NotFound, "UEFI firmware image path is missed")
            })?;
            let load_gpa = self.bios_load_gpa.ok_or_else(|| {
                ax_errno::ax_err_type!(NotFound, "UEFI firmware load addr is missed")
            })?;

            #[cfg(feature = "fs")]
            {
                info!(
                    "Loading UEFI firmware image {} at GPA {:#x}",
                    firmware_path,
                    load_gpa.as_usize()
                );
                return fs::load_vm_image(firmware_path, load_gpa, self.vm.clone());
            }

            #[cfg(not(feature = "fs"))]
            {
                return Err(ax_errno::ax_err_type!(
                    Unsupported,
                    "UEFI firmware path requires the fs feature when no firmware image buffer is available"
                ));
            }
        }

        #[cfg(target_arch = "x86_64")]
        if self.should_load_default_x86_boot_image() {
            let bios_load_gpa = builtin_x86_bios_load_gpa(self.bios_load_gpa)?;
            info!(
                "Loading built-in x86 boot image at GPA {:#x}",
                bios_load_gpa.as_usize()
            );
            load_vm_image_from_memory(
                x86_boot::DEFAULT_BIOS_IMAGE,
                bios_load_gpa,
                self.vm.clone(),
            )?;
            #[cfg(target_arch = "x86_64")]
            self.load_x86_multiboot_info(x86_boot::DEFAULT_BIOS_IMAGE, bios_load_gpa)?;
        }

        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn should_load_default_x86_boot_image(&self) -> bool {
        self.config.kernel.enable_bios
            && self.config.kernel.boot_firmware_path().is_none()
            && self.config.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot
    }

    #[cfg(target_arch = "x86_64")]
    fn load_x86_multiboot_info(&self, bios_image: &[u8], bios_load_gpa: GuestPhysAddr) -> AxResult {
        const MULTIBOOT_INFO_GPA: usize = 0x6000;
        const MULTIBOOT_MMAP_GPA: usize = 0x6040;
        const MULTIBOOT_INFO_FLAGS: u32 = (1 << 0) | (1 << 6);
        const MULTIBOOT_MEMORY_AVAILABLE: u32 = 1;

        let mem_base = self.main_memory.gpa.as_usize() as u64;
        let mem_size = self.main_memory.size() as u64;
        let mem_upper_kb = mem_size.saturating_sub(0x100000) / 1024;

        let mut mbi = [0u8; 52];
        write_u32(&mut mbi, 0, MULTIBOOT_INFO_FLAGS);
        write_u32(&mut mbi, 4, 639);
        write_u32(&mut mbi, 8, mem_upper_kb as u32);
        write_u32(&mut mbi, 44, 24);
        write_u32(&mut mbi, 48, MULTIBOOT_MMAP_GPA as u32);

        let mut mmap = [0u8; 24];
        write_u32(&mut mmap, 0, 20);
        write_u64(&mut mmap, 4, mem_base);
        write_u64(&mut mmap, 12, mem_size);
        write_u32(&mut mmap, 20, MULTIBOOT_MEMORY_AVAILABLE);

        let mbi_gpa = (MULTIBOOT_INFO_GPA as u32).to_le_bytes();
        validate_x86_bios_patch_region(bios_image)?;
        load_vm_image_from_memory(&mbi, MULTIBOOT_INFO_GPA.into(), self.vm.clone())?;
        load_vm_image_from_memory(&mmap, MULTIBOOT_MMAP_GPA.into(), self.vm.clone())?;
        load_vm_image_from_memory(
            &mbi_gpa,
            (bios_load_gpa.as_usize() + x86_boot::AXVM_BIOS_EBX_IMM_OFFSET).into(),
            self.vm.clone(),
        )?;
        Ok(())
    }

    fn load_ramdisk_from_memory(&self, ramdisk: &[u8]) -> AxResult {
        let load_gpa = self.ramdisk_load_gpa()?;
        let size = ramdisk.len();
        self.vm.with_config(|config| {
            if let Some(ref mut rd) = config.image_config.ramdisk {
                rd.size = Some(size);
            }
        });
        info!(
            "Loading ramdisk image from memory ({} bytes) into GPA @{:#x}",
            size,
            load_gpa.as_usize()
        );
        load_vm_image_from_memory(ramdisk, load_gpa, self.vm.clone())
    }

    fn ramdisk_load_gpa(&self) -> AxResult<GuestPhysAddr> {
        self.ramdisk_load_gpa
            .ok_or_else(|| ax_errno::ax_err_type!(NotFound, "Ramdisk load addr is missed"))
    }

    #[cfg(target_arch = "x86_64")]
    fn adjust_x86_linux_dma_identity_layout(&mut self) -> AxResult {
        if !self.main_memory.is_identical() {
            return Ok(());
        }

        let memory_base = self.main_memory.gpa.as_usize();
        let configured_kernel = self.config.kernel.kernel_load_addr;
        let configured_ramdisk = self.config.kernel.ramdisk_load_addr;

        self.kernel_load_gpa = GuestPhysAddr::from(memory_base + configured_kernel);
        if let Some(ramdisk_load_addr) = configured_ramdisk {
            self.ramdisk_load_gpa = Some(GuestPhysAddr::from(memory_base + ramdisk_load_addr));
        }

        self.vm.with_config(|config| {
            config.image_config.kernel_load_gpa = self.kernel_load_gpa;
            if let Some(load_gpa) = self.ramdisk_load_gpa
                && let Some(ref mut ramdisk) = config.image_config.ramdisk
            {
                ramdisk.load_gpa = load_gpa;
            }
        });

        info!(
            "Adjusted x86 Linux identity DMA layout for VM[{}]: memory_base={:#x}, \
             kernel_load_gpa={:#x}, ramdisk_load_gpa={:?}",
            self.vm.id(),
            memory_base,
            self.kernel_load_gpa.as_usize(),
            self.ramdisk_load_gpa
        );
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn load_x86_linux_layout(
        &self,
        header: x86_linux::X86LinuxHeader,
        layout: x86_linux::X86LinuxLoadLayout,
        kernel: &[u8],
    ) -> AxResult {
        info!(
            "x86 Linux layout for VM[{}]: header={:#x?}, payload_offset={:#x}, \
             boot_params=[{:#x}..{:#x}), boot_stub=[{:#x}..{:#x}), kernel=[{:#x}..{:#x}), \
             initrd={:?}",
            self.config.base.id,
            header,
            header.payload_offset(),
            layout.boot_params.start,
            layout.boot_params.end().unwrap(),
            layout.boot_stub.start,
            layout.boot_stub.end().unwrap(),
            layout.kernel.start,
            layout.kernel.end().unwrap(),
            layout.initrd
        );

        let boot_params = self.build_x86_boot_params(header, layout, kernel)?;
        let boot_stub = self.build_x86_linux_boot_stub(&layout)?;
        let mp_table = x86_mptable::build();
        load_vm_image_from_memory(
            &boot_params,
            layout.boot_params.start.into(),
            self.vm.clone(),
        )?;
        load_vm_image_from_memory(&boot_stub, layout.boot_stub.start.into(), self.vm.clone())?;
        load_vm_image_from_memory(&mp_table, x86_mptable::MP_TABLE_GPA.into(), self.vm.clone())?;
        self.install_x86_linux_boot_entry(&layout);
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn build_x86_linux_boot_stub(
        &self,
        layout: &x86_linux::X86LinuxLoadLayout,
    ) -> AxResult<[u8; x86_linux::BOOT_STUB_SIZE]> {
        x86_linux_boot::build_boot_image(layout).map_err(|err| {
            ax_errno::ax_err_type!(
                InvalidInput,
                format!("failed to build x86 Linux boot stub: {err:?}")
            )
        })
    }

    #[cfg(target_arch = "x86_64")]
    fn install_x86_linux_boot_entry(&self, layout: &x86_linux::X86LinuxLoadLayout) {
        let entry = GuestPhysAddr::from(x86_linux_boot::DEFAULT_LINUX_BOOT_LOAD_GPA);
        self.vm.with_config(|config| {
            config.cpu_config.bsp_entry = entry;
            config.cpu_config.ap_entry = entry;
        });
        info!(
            "x86 Linux direct boot entry for VM[{}]: stub={:#x}, boot_params={:#x}, \
             kernel_entry={:#x}, initrd={:?}",
            self.config.base.id,
            layout.boot_stub.start,
            layout.boot_params.start,
            layout.kernel.start,
            layout.initrd
        );
    }

    #[cfg(target_arch = "x86_64")]
    fn build_x86_boot_params(
        &self,
        header: x86_linux::X86LinuxHeader,
        layout: x86_linux::X86LinuxLoadLayout,
        kernel: &[u8],
    ) -> AxResult<[u8; x86_linux::BOOT_PARAMS_SIZE]> {
        let mut builder = x86_boot_params::BootParamsBuilder::new(
            kernel,
            header,
            layout,
            x86_linux::X86LinuxRange::new(self.main_memory.gpa.as_usize(), self.main_memory.size()),
        );
        if let Some(command_line) = self.config.kernel.cmdline.as_deref() {
            builder.set_command_line(command_line).map_err(|err| {
                ax_errno::ax_err_type!(
                    InvalidInput,
                    format!("invalid x86 Linux command line: {err:?}")
                )
            })?;
        }

        for memory in &self.config.kernel.memory_regions {
            if memory.map_type == VmMemMappingType::MapAlloc {
                builder.add_ram_range(x86_linux::X86LinuxRange::new(memory.gpa, memory.size));
            }
        }

        for device in &self.config.devices.passthrough_devices {
            builder.add_reserved_range(x86_linux::X86LinuxRange::new(
                device.base_gpa,
                device.length,
            ));
        }
        for address in &self.config.devices.passthrough_addresses {
            builder.add_reserved_range(x86_linux::X86LinuxRange::new(
                address.base_gpa,
                address.length,
            ));
        }
        builder.add_reserved_range(x86_mptable::reserved_range());

        builder.build().map_err(|err| {
            ax_errno::ax_err_type!(
                InvalidInput,
                format!("failed to build x86 boot_params: {err:?}")
            )
        })
    }

    #[cfg(feature = "fs")]
    fn load_ramdisk_from_filesystem(&self, ramdisk_path: &str) -> AxResult {
        let load_gpa = self
            .vm
            .with_config(|config| config.image_config.ramdisk.as_ref().map(|r| r.load_gpa))
            .ok_or_else(|| ax_errno::ax_err_type!(NotFound, "Ramdisk load addr is missed"))?;
        let ramdisk_size = fs::image_size(ramdisk_path)?;
        self.vm.with_config(|config| {
            if let Some(ref mut rd) = config.image_config.ramdisk {
                rd.size = Some(ramdisk_size);
            }
        });
        info!(
            "Loading ramdisk image from filesystem {} ({} bytes) into GPA @{:#x}",
            ramdisk_path,
            ramdisk_size,
            load_gpa.as_usize()
        );
        fs::load_vm_image(ramdisk_path, load_gpa, self.vm.clone())
    }
}

pub fn load_vm_image_from_memory(
    image_buffer: &[u8],
    load_addr: GuestPhysAddr,
    vm: AxVMRef,
) -> AxResult {
    let mut buffer_pos = 0;

    let image_size = image_buffer.len();

    debug!(
        "loading VM image from memory {:?} {}",
        load_addr,
        image_buffer.len()
    );

    let image_load_regions = vm.get_image_load_region(load_addr, image_size)?;

    for region in image_load_regions {
        let region_len = region.len();
        let bytes_to_write = region_len.min(image_size - buffer_pos);

        // SAFETY: `region` is valid writable guest memory obtained from
        // `vm.get_image_load_region()`; `bytes_to_write <= region.len()` is
        // guaranteed by `region_len.min(image_size - buffer_pos)`; and
        // `image_buffer[buffer_pos..]` has at least `bytes_to_write` bytes.
        // The source and destination do not overlap (guest HPA vs host image buffer).
        unsafe {
            core::ptr::copy_nonoverlapping(
                image_buffer[buffer_pos..].as_ptr(),
                region.as_mut_ptr().cast(),
                bytes_to_write,
            );
        }

        axvm::clean_dcache_range((region.as_ptr() as usize).into(), bytes_to_write);

        // Update the position of the buffer.
        buffer_pos += bytes_to_write;

        // If the buffer is fully written, exit the loop.
        if buffer_pos >= image_size {
            debug!("copy size: {bytes_to_write}");
            break;
        }
    }

    if buffer_pos == image_size {
        Ok(())
    } else {
        ax_err!(
            InvalidData,
            format!("VM image was only partially loaded: {buffer_pos}/{image_size} bytes")
        )
    }
}

#[cfg(feature = "fs")]
pub mod fs {
    use alloc::vec::Vec;

    use ax_errno::{AxResult, ax_err, ax_err_type};

    use super::*;

    pub fn kernel_read(config: &AxVMCrateConfig, read_size: usize) -> AxResult<Vec<u8>> {
        let file_name = &config.kernel.kernel_path;
        crate::manager::AxvmManager::read_file_exact(file_name, read_size)
    }

    /// Loads the VM image files from the filesystem
    /// into the guest VM's memory space based on the VM configuration.
    pub(crate) fn load_vm_images_from_filesystem(loader: &mut ImageLoader) -> AxResult {
        info!("Loading VM images from filesystem");
        #[cfg(target_arch = "x86_64")]
        {
            let kernel_probe = kernel_read(&loader.config, x86_linux::HEADER_READ_SIZE);
            match kernel_probe {
                Ok(data) => {
                    if let Some(header) = detect_x86_linux_image(&data) {
                        let kernel = read_image_file(&loader.config.kernel.kernel_path)?;
                        return loader.load_x86_linux_images_from_filesystem(header, &kernel);
                    }
                }
                Err(err) => debug!("Unable to probe x86 Linux bzImage header: {err:?}"),
            }
        }
        // Load kernel image.
        load_vm_image(
            &loader.config.kernel.kernel_path,
            loader.kernel_load_gpa,
            loader.vm.clone(),
        )?;
        // Load boot firmware image if needed.
        if loader.config.kernel.enable_bios
            && let Some(bios_path) = loader.config.kernel.boot_firmware_path()
        {
            if let Some(bios_load_addr) = loader.bios_load_gpa {
                #[cfg(target_arch = "x86_64")]
                {
                    if should_patch_x86_multiboot_info(&loader.config) {
                        let bios_image = read_image_file(bios_path)?;
                        validate_x86_bios_patch_region(&bios_image)?;
                        load_vm_image_from_memory(&bios_image, bios_load_addr, loader.vm.clone())?;
                        loader.load_x86_multiboot_info(&bios_image, bios_load_addr)?;
                    } else {
                        load_vm_image(bios_path, bios_load_addr, loader.vm.clone())?;
                    }
                }
                #[cfg(not(target_arch = "x86_64"))]
                load_vm_image(bios_path, bios_load_addr, loader.vm.clone())?;
            } else {
                return ax_err!(NotFound, "boot firmware load addr is missed");
            }
        };
        #[cfg(target_arch = "x86_64")]
        if loader.should_load_default_x86_boot_image() {
            let bios_load_gpa = builtin_x86_bios_load_gpa(loader.bios_load_gpa)?;
            info!(
                "Loading built-in x86 boot image at GPA {:#x}",
                bios_load_gpa.as_usize()
            );
            load_vm_image_from_memory(
                x86_boot::DEFAULT_BIOS_IMAGE,
                bios_load_gpa,
                loader.vm.clone(),
            )?;
            #[cfg(target_arch = "x86_64")]
            loader.load_x86_multiboot_info(x86_boot::DEFAULT_BIOS_IMAGE, bios_load_gpa)?;
        }
        // Load Ramdisk image if needed.
        if let Some(ramdisk_path) = &loader.config.kernel.ramdisk_path {
            loader.load_ramdisk_from_filesystem(ramdisk_path)?;
        };
        // Load DTB image if needed.
        let vm_config = crate::config::build_axvm_config(&loader.config);
        if let Some(dtb_arc) = get_vm_dtb_arc(&vm_config) {
            let _dtb_slice: &[u8] = &dtb_arc;
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            {
                let dtb_src = core::ptr::NonNull::new(_dtb_slice.as_ptr() as *mut u8)
                    .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
                crate::fdt::update_fdt(
                    dtb_src,
                    _dtb_slice.len(),
                    loader.vm.clone(),
                    &loader.config,
                )?;
            }
            #[cfg(target_arch = "loongarch64")]
            {
                let dtb_load_gpa = loader
                    .dtb_load_gpa
                    .ok_or_else(|| ax_err_type!(NotFound, "DTB load address is missing"))?;
                load_vm_image_from_memory(_dtb_slice, dtb_load_gpa, loader.vm.clone())?;
            }
        }

        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    impl ImageLoader {
        fn load_x86_linux_images_from_filesystem(
            &mut self,
            header: x86_linux::X86LinuxHeader,
            kernel: &[u8],
        ) -> AxResult {
            self.adjust_x86_linux_dma_identity_layout()?;
            let payload = x86_linux_payload(&header, kernel)?;
            let initrd = if let Some(ramdisk_path) = &self.config.kernel.ramdisk_path {
                let ramdisk_size = image_size(ramdisk_path)?;
                Some(x86_linux::X86LinuxRange::new(
                    self.ramdisk_load_gpa()?.as_usize(),
                    ramdisk_size,
                ))
            } else {
                None
            };
            let layout = x86_linux::X86LinuxLoadLayout::new(
                &header,
                self.kernel_load_gpa.as_usize(),
                payload.len(),
                initrd,
            )
            .map_err(x86_linux_layout_error)?;

            self.load_x86_linux_layout(header, layout, kernel)?;
            load_vm_image_from_memory(payload, self.kernel_load_gpa, self.vm.clone())?;

            if let Some(ramdisk_path) = &self.config.kernel.ramdisk_path {
                self.load_ramdisk_from_filesystem(ramdisk_path)?;
            }

            Ok(())
        }
    }

    pub(crate) fn load_vm_image(
        image_path: &str,
        image_load_gpa: GuestPhysAddr,
        vm: AxVMRef,
    ) -> AxResult {
        let image = crate::manager::AxvmManager::read_file(image_path)?;
        let image_size = image.len();

        let image_load_regions = vm.get_image_load_region(image_load_gpa, image_size)?;
        let mut offset = 0;

        for buffer in image_load_regions {
            let end = offset + buffer.len();
            let data = image.get(offset..end).ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("Image {} has an invalid load region layout", image_path)
                )
            })?;
            buffer.copy_from_slice(data);
            offset = end;

            axvm::clean_dcache_range((buffer.as_ptr() as usize).into(), buffer.len());
        }

        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn read_image_file(image_path: &str) -> AxResult<Vec<u8>> {
        crate::manager::AxvmManager::read_file(image_path)
    }

    pub fn image_size(file_name: &str) -> AxResult<usize> {
        crate::manager::AxvmManager::file_size(file_name)
    }

    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    ))]
    pub fn read_full_image(file_name: &str) -> AxResult<Vec<u8>> {
        crate::manager::AxvmManager::read_file(file_name)
    }
}

#[cfg(target_arch = "x86_64")]
fn should_patch_x86_multiboot_info(config: &AxVMCrateConfig) -> bool {
    config.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot
}

#[cfg(target_arch = "x86_64")]
fn detect_x86_linux_image(image: &[u8]) -> Option<x86_linux::X86LinuxHeader> {
    match x86_linux::X86LinuxHeader::parse(image) {
        Ok(header) => Some(header),
        Err(err) => {
            debug!("Not an x86 Linux bzImage: {err:?}");
            None
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn x86_linux_payload<'a>(
    header: &x86_linux::X86LinuxHeader,
    image: &'a [u8],
) -> AxResult<&'a [u8]> {
    let payload_offset = header.payload_offset();
    image.get(payload_offset..).ok_or_else(|| {
        ax_errno::ax_err_type!(
            InvalidInput,
            format!(
                "x86 Linux bzImage payload offset {:#x} exceeds image size {:#x}",
                payload_offset,
                image.len()
            )
        )
    })
}

#[cfg(target_arch = "x86_64")]
fn x86_linux_layout_error(err: x86_linux::X86LinuxLayoutError) -> ax_errno::AxError {
    ax_errno::ax_err_type!(
        InvalidInput,
        format!("invalid x86 Linux memory layout: {err:?}")
    )
}

#[cfg(target_arch = "x86_64")]
fn builtin_x86_bios_load_gpa(configured_gpa: Option<GuestPhysAddr>) -> AxResult<GuestPhysAddr> {
    let default_gpa = GuestPhysAddr::from(x86_boot::DEFAULT_BIOS_LOAD_GPA);
    match configured_gpa {
        Some(gpa) if gpa != default_gpa => Err(ax_errno::ax_err_type!(
            InvalidInput,
            format!(
                "built-in x86 BIOS must be loaded at GPA {:#x}, but bios_load_addr is {:#x}; set \
                 bios_path to use a relocatable external BIOS image",
                default_gpa.as_usize(),
                gpa.as_usize()
            )
        )),
        Some(gpa) => Ok(gpa),
        None => Ok(default_gpa),
    }
}

#[cfg(target_arch = "x86_64")]
fn validate_x86_bios_patch_region(bios_image: &[u8]) -> AxResult {
    let patch_end = x86_boot::AXVM_BIOS_EBX_IMM_OFFSET + core::mem::size_of::<u32>();
    if bios_image.len() < patch_end {
        return Err(ax_errno::ax_err_type!(
            InvalidInput,
            format!(
                "x86 BIOS image is too small for multiboot info patch: size {}, need at least {} \
                 bytes for EBX immediate at offset {:#x}",
                bios_image.len(),
                patch_end,
                x86_boot::AXVM_BIOS_EBX_IMM_OFFSET
            )
        ));
    }

    if bios_image[x86_boot::AXVM_BIOS_EBX_IMM_OFFSET - 1] != x86_boot::MOV_EBX_IMM32_OPCODE {
        return Err(ax_errno::ax_err_type!(
            InvalidInput,
            format!(
                "x86 BIOS image does not match axvm-bios layout: expected mov ebx, imm32 opcode \
                 at offset {:#x}",
                x86_boot::AXVM_BIOS_EBX_IMM_OFFSET - 1
            )
        ));
    }

    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(target_arch = "x86_64")]
fn write_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn built_in_x86_bios_uses_default_gpa_when_unspecified() {
        assert_eq!(
            builtin_x86_bios_load_gpa(None).unwrap(),
            GuestPhysAddr::from(x86_boot::DEFAULT_BIOS_LOAD_GPA)
        );
    }

    #[test]
    fn built_in_x86_bios_accepts_explicit_default_gpa() {
        let default_gpa = GuestPhysAddr::from(x86_boot::DEFAULT_BIOS_LOAD_GPA);

        assert_eq!(
            builtin_x86_bios_load_gpa(Some(default_gpa)).unwrap(),
            default_gpa
        );
    }

    #[test]
    fn built_in_x86_bios_rejects_non_default_gpa() {
        let invalid_gpa = GuestPhysAddr::from(x86_boot::DEFAULT_BIOS_LOAD_GPA + 0x1000);

        assert!(builtin_x86_bios_load_gpa(Some(invalid_gpa)).is_err());
    }

    #[test]
    fn legacy_x86_bios_config_uses_multiboot_patch() {
        let mut cfg = AxVMCrateConfig::default();
        cfg.kernel.enable_bios = true;

        assert!(should_patch_x86_multiboot_info(&cfg));
    }

    #[test]
    fn x86_uefi_config_skips_multiboot_patch() {
        let mut cfg = AxVMCrateConfig::default();
        cfg.kernel.enable_bios = true;
        cfg.kernel.boot_protocol = Some(VMBootProtocol::Uefi);

        assert!(!should_patch_x86_multiboot_info(&cfg));
    }
}
