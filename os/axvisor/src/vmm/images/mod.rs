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

use ax_errno::AxResult;
use axaddrspace::GuestPhysAddr;

use axvm::VMMemoryRegion;
use axvm::config::AxVMCrateConfig;
use byte_unit::Byte;

use crate::hal::CacheOp;
use crate::vmm::VMRef;
use crate::vmm::config::{config, get_vm_dtb_arc};

mod linux;
#[cfg(target_arch = "x86_64")]
mod x86_boot;

pub fn get_image_header(config: &AxVMCrateConfig) -> Option<linux::Header> {
    match config.kernel.image_location.as_deref() {
        Some("memory") => with_memory_image(config, linux::Header::parse),
        #[cfg(feature = "fs")]
        Some("fs") => {
            let read_size = linux::Header::hdr_size();
            let data = fs::kernal_read(config, read_size).ok()?;
            linux::Header::parse(&data)
        }
        _ => unimplemented!(
            "Check your \"image_location\" in config.toml, \"memory\" and \"fs\" are supported,\n NOTE: \"fs\" feature should be enabled if you want to load images from filesystem. (APP_FEATURES=fs)"
        ),
    }
}

fn with_memory_image<F, R>(config: &AxVMCrateConfig, func: F) -> R
where
    F: FnOnce(&[u8]) -> R,
{
    let vm_imags = config::get_memory_images()
        .iter()
        .find(|&v| v.id == config.base.id)
        .expect("VM images is missed, Perhaps add `VM_CONFIGS=PATH/CONFIGS/FILE` command.");

    func(vm_imags.kernel)
}

pub struct ImageLoader {
    main_memory: VMMemoryRegion,
    vm: VMRef,
    config: AxVMCrateConfig,
    kernel_load_gpa: GuestPhysAddr,
    bios_load_gpa: Option<GuestPhysAddr>,
    dtb_load_gpa: Option<GuestPhysAddr>,
}

impl ImageLoader {
    pub fn new(main_memory: VMMemoryRegion, config: AxVMCrateConfig, vm: VMRef) -> Self {
        Self {
            main_memory,
            vm,
            config,
            kernel_load_gpa: GuestPhysAddr::default(),
            bios_load_gpa: None,
            dtb_load_gpa: None,
        }
    }

    pub fn load(&mut self) -> AxResult {
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
        });
        #[cfg(target_arch = "x86_64")]
        if self.config.kernel.bios_path.is_none()
            && self.bios_load_gpa.is_none()
            && self.config.kernel.entry_point == self.default_bios_gpa().as_usize()
        {
            self.bios_load_gpa = Some(self.default_bios_gpa());
        }

        match self.config.kernel.image_location.as_deref() {
            Some("memory") => self.load_vm_images_from_memory(),
            #[cfg(feature = "fs")]
            Some("fs") => fs::load_vm_images_from_filesystem(self),
            _ => unimplemented!(
                "Check your \"image_location\" in config.toml, \"memory\" and \"fs\" are supported,\n NOTE: \"fs\" feature should be enabled if you want to load images from filesystem. (APP_FEATURES=fs)"
            ),
        }
    }

    /// Load VM images from memory
    /// into the guest VM's memory space based on the VM configuration.
    fn load_vm_images_from_memory(&self) -> AxResult {
        info!("Loading VM[{}] images from memory", self.config.base.id);

        let vm_imags = config::get_memory_images()
            .iter()
            .find(|&v| v.id == self.config.base.id)
            .expect("VM images is missed, Perhaps add `VM_CONFIGS=PATH/CONFIGS/FILE` command.");

        load_vm_image_from_memory(vm_imags.kernel, self.kernel_load_gpa, self.vm.clone())
            .expect("Failed to load VM images");

        // Load Ramdisk image and record its size before regenerating the DTB.
        if let Some(buffer) = vm_imags.ramdisk {
            self.load_ramdisk_from_memory(buffer)
                .expect("Failed to load Ramdisk images");
        }
        // Load DTB image
        let vm_config = axvm::config::AxVMConfig::from(self.config.clone());

        if let Some(dtb_arc) = get_vm_dtb_arc(&vm_config) {
            let _dtb_slice: &[u8] = &dtb_arc;
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            crate::vmm::fdt::update_fdt(
                core::ptr::NonNull::new(_dtb_slice.as_ptr() as *mut u8).unwrap(),
                _dtb_slice.len(),
                self.vm.clone(),
                &self.config,
            );
            #[cfg(target_arch = "loongarch64")]
            load_vm_image_from_memory(_dtb_slice, self.dtb_load_gpa.unwrap(), self.vm.clone())
                .expect("Failed to load DTB images");
        } else {
            #[cfg(any(target_arch = "loongarch64", target_arch = "riscv64"))]
            if let Some(buffer) = vm_imags.dtb {
                load_vm_image_from_memory(buffer, self.dtb_load_gpa.unwrap(), self.vm.clone())
                    .expect("Failed to load DTB images");
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

    fn load_boot_image_from_memory(&self, bios: Option<&[u8]>) -> AxResult {
        if let Some(buffer) = bios {
            let load_gpa = self
                .bios_load_gpa
                .expect("BIOS image present but BIOS load addr is missed");
            load_vm_image_from_memory(buffer, load_gpa, self.vm.clone())
                .expect("Failed to load BIOS images");
            #[cfg(target_arch = "x86_64")]
            self.load_x86_multiboot_info(buffer)?;
            return Ok(());
        }

        #[cfg(target_arch = "x86_64")]
        if self.should_load_default_x86_boot_image() {
            info!(
                "Loading built-in x86 boot image at GPA {:#x}",
                self.default_bios_gpa().as_usize()
            );
            load_vm_image_from_memory(
                x86_boot::DEFAULT_BIOS_IMAGE,
                self.default_bios_gpa(),
                self.vm.clone(),
            )
            .expect("Failed to load built-in x86 boot image");
            #[cfg(target_arch = "x86_64")]
            self.load_x86_multiboot_info(x86_boot::DEFAULT_BIOS_IMAGE)?;
        }

        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn should_load_default_x86_boot_image(&self) -> bool {
        self.config.kernel.bios_path.is_none()
            && self.config.kernel.entry_point == self.default_bios_gpa().as_usize()
    }

    #[cfg(target_arch = "x86_64")]
    fn default_bios_gpa(&self) -> GuestPhysAddr {
        self.bios_load_gpa
            .unwrap_or_else(|| GuestPhysAddr::from(x86_boot::DEFAULT_BIOS_LOAD_GPA))
    }

    #[cfg(target_arch = "x86_64")]
    fn load_x86_multiboot_info(&self, bios_image: &[u8]) -> AxResult {
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
        let bios_load_gpa = self.default_bios_gpa();
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
        let load_gpa = self
            .vm
            .with_config(|config| config.image_config.ramdisk.as_ref().map(|r| r.load_gpa))
            .expect("Ramdisk image present but ramdisk info is missing");
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

    #[cfg(feature = "fs")]
    fn load_ramdisk_from_filesystem(&self, ramdisk_path: &str) -> AxResult {
        let load_gpa = self
            .vm
            .with_config(|config| config.image_config.ramdisk.as_ref().map(|r| r.load_gpa))
            .ok_or_else(|| ax_errno::ax_err_type!(NotFound, "Ramdisk load addr is missed"))?;
        let (_, ramdisk_size) = fs::open_image_file(ramdisk_path)?;
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
    vm: VMRef,
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

        // copy data from memory
        unsafe {
            core::ptr::copy_nonoverlapping(
                image_buffer[buffer_pos..].as_ptr(),
                region.as_mut_ptr().cast(),
                bytes_to_write,
            );
        }

        crate::hal::arch::cache::dcache_range(
            CacheOp::Clean,
            (region.as_ptr() as usize).into(),
            region_len,
        );

        // Update the position of the buffer.
        buffer_pos += bytes_to_write;

        // If the buffer is fully written, exit the loop.
        if buffer_pos >= image_size {
            debug!("copy size: {bytes_to_write}");
            break;
        }
    }

    Ok(())
}

#[cfg(feature = "fs")]
pub mod fs {
    use super::*;
    use ax_errno::{AxResult, ax_err, ax_err_type};
    use std::{fs::File, vec::Vec};

    pub fn kernal_read(config: &AxVMCrateConfig, read_size: usize) -> AxResult<Vec<u8>> {
        use std::fs::File;
        use std::io::Read;
        let file_name = &config.kernel.kernel_path;

        let mut file = File::open(file_name).map_err(|err| {
            ax_err_type!(
                NotFound,
                format!(
                    "Failed to open {}, err {:?}, please check your disk.img",
                    file_name, err
                )
            )
        })?;

        let mut buffer = vec![0u8; read_size];

        file.read_exact(&mut buffer).map_err(|err| {
            ax_err_type!(
                NotFound,
                format!(
                    "Failed to read {}, err {:?}, please check your disk.img",
                    file_name, err
                )
            )
        })?;

        Ok(buffer)
    }

    /// Loads the VM image files from the filesystem
    /// into the guest VM's memory space based on the VM configuration.
    pub(crate) fn load_vm_images_from_filesystem(loader: &ImageLoader) -> AxResult {
        info!("Loading VM images from filesystem");
        // Load kernel image.
        load_vm_image(
            &loader.config.kernel.kernel_path,
            loader.kernel_load_gpa,
            loader.vm.clone(),
        )?;
        // Load BIOS image if needed.
        if let Some(bios_path) = &loader.config.kernel.bios_path {
            if let Some(bios_load_addr) = loader.bios_load_gpa {
                #[cfg(target_arch = "x86_64")]
                let bios_image = read_image_file(bios_path)?;
                #[cfg(target_arch = "x86_64")]
                {
                    validate_x86_bios_patch_region(&bios_image)?;
                    load_vm_image_from_memory(&bios_image, bios_load_addr, loader.vm.clone())?;
                    loader.load_x86_multiboot_info(&bios_image)?;
                }
                #[cfg(not(target_arch = "x86_64"))]
                load_vm_image(bios_path, bios_load_addr, loader.vm.clone())?;
            } else {
                return ax_err!(NotFound, "BIOS load addr is missed");
            }
        };
        #[cfg(target_arch = "x86_64")]
        if loader.config.kernel.bios_path.is_none() && loader.should_load_default_x86_boot_image() {
            info!(
                "Loading built-in x86 boot image at GPA {:#x}",
                loader.default_bios_gpa().as_usize()
            );
            load_vm_image_from_memory(
                x86_boot::DEFAULT_BIOS_IMAGE,
                loader.default_bios_gpa(),
                loader.vm.clone(),
            )
            .expect("Failed to load built-in x86 boot image");
            #[cfg(target_arch = "x86_64")]
            loader.load_x86_multiboot_info(x86_boot::DEFAULT_BIOS_IMAGE)?;
        }
        // Load Ramdisk image if needed.
        if let Some(ramdisk_path) = &loader.config.kernel.ramdisk_path {
            loader.load_ramdisk_from_filesystem(ramdisk_path)?;
        };
        // Load DTB image if needed.
        let vm_config = axvm::config::AxVMConfig::from(loader.config.clone());
        if let Some(dtb_arc) = get_vm_dtb_arc(&vm_config) {
            let _dtb_slice: &[u8] = &dtb_arc;
            #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
            crate::vmm::fdt::update_fdt(
                core::ptr::NonNull::new(_dtb_slice.as_ptr() as *mut u8).unwrap(),
                _dtb_slice.len(),
                loader.vm.clone(),
                &loader.config,
            );
            #[cfg(target_arch = "loongarch64")]
            load_vm_image_from_memory(_dtb_slice, loader.dtb_load_gpa.unwrap(), loader.vm.clone())
                .expect("Failed to load DTB images");
        }

        Ok(())
    }

    pub(crate) fn load_vm_image(
        image_path: &str,
        image_load_gpa: GuestPhysAddr,
        vm: VMRef,
    ) -> AxResult {
        use std::io::{BufReader, Read};
        let (image_file, image_size) = open_image_file(image_path)?;

        let image_load_regions = vm.get_image_load_region(image_load_gpa, image_size)?;
        let mut file = BufReader::new(image_file);

        for buffer in image_load_regions {
            file.read_exact(buffer).map_err(|err| {
                ax_err_type!(
                    Io,
                    format!("Failed in reading from file {}, err {:?}", image_path, err)
                )
            })?;

            crate::hal::arch::cache::dcache_range(
                CacheOp::Clean,
                (buffer.as_ptr() as usize).into(),
                buffer.len(),
            );
        }

        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn read_image_file(image_path: &str) -> AxResult<Vec<u8>> {
        use std::io::{BufReader, Read};
        let (image_file, image_size) = open_image_file(image_path)?;
        let mut image = vec![0; image_size];
        BufReader::new(image_file)
            .read_exact(&mut image)
            .map_err(|err| {
                ax_err_type!(
                    Io,
                    format!("Failed in reading from file {}, err {:?}", image_path, err)
                )
            })?;
        Ok(image)
    }

    pub fn open_image_file(file_name: &str) -> AxResult<(File, usize)> {
        let file = File::open(file_name).map_err(|err| {
            ax_err_type!(
                NotFound,
                format!(
                    "Failed to open {}, err {:?}, please check your disk.img",
                    file_name, err
                )
            )
        })?;
        let file_size = file
            .metadata()
            .map_err(|err| {
                ax_err_type!(
                    Io,
                    format!(
                        "Failed to get metadate of file {}, err {:?}",
                        file_name, err
                    )
                )
            })?
            .size() as usize;
        Ok((file, file_size))
    }
}

#[cfg(target_arch = "x86_64")]
fn validate_x86_bios_patch_region(bios_image: &[u8]) -> AxResult {
    let patch_end = x86_boot::AXVM_BIOS_EBX_IMM_OFFSET + core::mem::size_of::<u32>();
    if bios_image.len() < patch_end {
        return Err(ax_errno::ax_err_type!(
            InvalidInput,
            format!(
                "x86 BIOS image is too small for multiboot info patch: size {}, need at least {} bytes for EBX immediate at offset {:#x}",
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
                "x86 BIOS image does not match axvm-bios layout: expected mov ebx, imm32 opcode at offset {:#x}",
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
