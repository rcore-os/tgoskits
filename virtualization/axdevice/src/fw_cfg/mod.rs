use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::cell::RefCell;

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::*;
use axvm_types::GuestPhysAddr;

use crate::*;

const FW_CFG_SIGNATURE: u16 = 0x00;
const FW_CFG_ID: u16 = 0x01;
const FW_CFG_RAM_SIZE: u16 = 0x03;
const FW_CFG_NB_CPUS: u16 = 0x05;
const FW_CFG_MAX_CPUS: u16 = 0x0f;
const FW_CFG_KERNEL_SIZE: u16 = 0x08;
const FW_CFG_INITRD_SIZE: u16 = 0x0b;
const FW_CFG_KERNEL_DATA: u16 = 0x11;
const FW_CFG_INITRD_DATA: u16 = 0x12;
const FW_CFG_CMDLINE_SIZE: u16 = 0x14;
const FW_CFG_CMDLINE_DATA: u16 = 0x15;
const FW_CFG_KERNEL_SETUP_SIZE: u16 = 0x17;
const FW_CFG_KERNEL_SETUP_DATA: u16 = 0x18;
const FW_CFG_FILE_DIR: u16 = 0x19;
const FW_CFG_FILE_FIRST: u16 = 0x20;
const FW_CFG_SMBIOS_TABLES: u16 = FW_CFG_FILE_FIRST + 1;
const FW_CFG_SMBIOS_ANCHOR: u16 = FW_CFG_FILE_FIRST + 2;
const FW_CFG_ACPI_TABLES: u16 = FW_CFG_FILE_FIRST + 3;
const FW_CFG_ACPI_RSDP: u16 = FW_CFG_FILE_FIRST + 4;
const FW_CFG_ACPI_LOADER: u16 = FW_CFG_FILE_FIRST + 5;
const FW_CFG_BOOT_KERNEL: u16 = FW_CFG_FILE_FIRST + 6;
const FW_CFG_BOOT_INITRD: u16 = FW_CFG_FILE_FIRST + 7;
const FW_CFG_BOOT_CMDLINE: u16 = FW_CFG_FILE_FIRST + 8;
const FW_CFG_FILE_NAME_SIZE: usize = 56;

const FW_CFG_VERSION: u32 = 0x01;
const FW_CFG_VERSION_DMA: u32 = 0x02;
const LOWMEM_BASE: u64 = 0;
const LOWMEM_LENGTH: u64 = 0x1000_0000;
const HIGHMEM_BASE: u64 = 0x8000_0000;
const HIGHMEM_LENGTH: u64 = 0x2400_0000;
const MEMMAP_RAM_TYPE: u32 = 1;

const FW_CFG_DATA_OFFSET: usize = 0x00;
const FW_CFG_SELECTOR_OFFSET: usize = 0x08;
const FW_CFG_DMA_OFFSET: usize = 0x10;

const ACPI_TABLE_FILE: &str = "etc/acpi/tables";
const ACPI_RSDP_FILE: &str = "etc/acpi/rsdp";
const ACPI_LOADER_FILE: &str = "etc/table-loader";
const BOOT_KERNEL_FILE: &str = "etc/boot/kernel";
const BOOT_INITRD_FILE: &str = "etc/boot/initrd";
const BOOT_CMDLINE_FILE: &str = "etc/boot/cmdline";
const FW_CFG_DMA_CTL_ERROR: u32 = 0x01;
const FW_CFG_DMA_CTL_READ: u32 = 0x02;
const FW_CFG_DMA_CTL_SKIP: u32 = 0x04;
const FW_CFG_DMA_CTL_SELECT: u32 = 0x08;
const FW_CFG_DMA_CTL_WRITE: u32 = 0x10;
const FW_CFG_DMA_DESC_SIZE: usize = 16;
const FW_CFG_DMA_SCRATCH_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub struct FwCfgRamRegion {
    pub base: u64,
    pub size: u64,
}

/// Prebuilt platform firmware blobs exposed through fw_cfg file entries.
///
/// `fw_cfg` itself is only the transport. Architecture boot code owns the
/// contents of ACPI/AML or other firmware tables and passes them in here.
#[derive(Clone, Debug, Default)]
pub struct FwCfgAcpiBlobs {
    pub tables: Vec<u8>,
    pub rsdp: Vec<u8>,
    pub loader: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct FwCfgPlatformConfig {
    pub ram_regions: Arc<[FwCfgRamRegion]>,
    pub srat_regions: Arc<[FwCfgRamRegion]>,
    pub acpi: FwCfgAcpiBlobs,
}

impl Default for FwCfgPlatformConfig {
    fn default() -> Self {
        static DEFAULT_RAM_REGIONS: [FwCfgRamRegion; 2] = [
            FwCfgRamRegion {
                base: LOWMEM_BASE,
                size: LOWMEM_LENGTH,
            },
            FwCfgRamRegion {
                base: HIGHMEM_BASE,
                size: HIGHMEM_LENGTH,
            },
        ];

        let regions: Arc<[FwCfgRamRegion]> = Arc::from(DEFAULT_RAM_REGIONS);
        Self {
            ram_regions: regions.clone(),
            srat_regions: regions,
            acpi: FwCfgAcpiBlobs::default(),
        }
    }
}

struct FwCfgState {
    selected: u16,
    offset: usize,
    dma_address: u64,
}

/// Minimal QEMU-compatible fw_cfg MMIO device.
pub struct FwCfg {
    base: GuestPhysAddr,
    size: usize,
    kernel: FwCfgKernelPayload,
    boot_kernel: Vec<u8>,
    initrd: Option<Arc<[u8]>>,
    cmdline: Vec<u8>,
    file_dir: Vec<u8>,
    memmap: Vec<u8>,
    smbios_tables: Vec<u8>,
    smbios_anchor: Vec<u8>,
    acpi_tables: Vec<u8>,
    acpi_rsdp: Vec<u8>,
    acpi_loader: Vec<u8>,
    cpu_num: u16,
    ram_size: u64,
    state: Mutex<FwCfgState>,
}

impl FwCfg {
    /// Create a fw_cfg device at `base`.
    pub fn new(
        base: GuestPhysAddr,
        size: usize,
        kernel: FwCfgKernelPayload,
        initrd: Option<Arc<[u8]>>,
        cmdline: Option<String>,
        cpu_num: u16,
        platform: FwCfgPlatformConfig,
    ) -> Self {
        let mut cmdline = cmdline.unwrap_or_default().into_bytes();
        if !cmdline.ends_with(&[0]) {
            cmdline.push(0);
        }

        let ram_size = platform
            .ram_regions
            .iter()
            .fold(0u64, |acc, region| acc.saturating_add(region.size));
        let memmap = build_memmap(&platform.ram_regions);
        let smbios_tables = build_smbios_tables();
        let smbios_anchor = build_smbios_anchor();
        let acpi = platform.acpi;
        let mut boot_kernel = Vec::with_capacity(kernel.total_len());
        boot_kernel.extend_from_slice(kernel.setup());
        boot_kernel.extend_from_slice(kernel.kernel());
        let mut files = alloc::vec![
            FwCfgFile {
                name: "etc/memmap",
                selector: FW_CFG_FILE_FIRST,
                size: memmap.len() as u32,
            },
            FwCfgFile {
                name: "etc/smbios/smbios-anchor",
                selector: FW_CFG_SMBIOS_ANCHOR,
                size: smbios_anchor.len() as u32,
            },
            FwCfgFile {
                name: "etc/smbios/smbios-tables",
                selector: FW_CFG_SMBIOS_TABLES,
                size: smbios_tables.len() as u32,
            },
            FwCfgFile {
                name: ACPI_TABLE_FILE,
                selector: FW_CFG_ACPI_TABLES,
                size: acpi.tables.len() as u32,
            },
            FwCfgFile {
                name: ACPI_RSDP_FILE,
                selector: FW_CFG_ACPI_RSDP,
                size: acpi.rsdp.len() as u32,
            },
            FwCfgFile {
                name: ACPI_LOADER_FILE,
                selector: FW_CFG_ACPI_LOADER,
                size: acpi.loader.len() as u32,
            },
        ];
        if !boot_kernel.is_empty() {
            files.push(FwCfgFile {
                name: BOOT_KERNEL_FILE,
                selector: FW_CFG_BOOT_KERNEL,
                size: boot_kernel.len() as u32,
            });
        }
        if let Some(initrd) = &initrd {
            files.push(FwCfgFile {
                name: BOOT_INITRD_FILE,
                selector: FW_CFG_BOOT_INITRD,
                size: initrd.len() as u32,
            });
        }
        if cmdline.len() > 1 {
            files.push(FwCfgFile {
                name: BOOT_CMDLINE_FILE,
                selector: FW_CFG_BOOT_CMDLINE,
                size: cmdline.len() as u32,
            });
        }
        let file_dir = build_file_dir(&files);

        Self {
            base,
            size,
            kernel,
            boot_kernel,
            initrd,
            cmdline,
            file_dir,
            memmap,
            smbios_tables,
            smbios_anchor,
            acpi_tables: acpi.tables,
            acpi_rsdp: acpi.rsdp,
            acpi_loader: acpi.loader,
            cpu_num,
            ram_size,
            state: Mutex::new(FwCfgState {
                selected: FW_CFG_SIGNATURE,
                offset: 0,
                dma_address: 0,
            }),
        }
    }

    fn selected_bytes(&self, selector: u16) -> FwCfgEntry<'_> {
        match selector {
            FW_CFG_SIGNATURE => FwCfgEntry::Bytes(b"QEMU"),
            FW_CFG_ID => {
                let version = if self.dma_enabled() {
                    FW_CFG_VERSION | FW_CFG_VERSION_DMA
                } else {
                    FW_CFG_VERSION
                };
                FwCfgEntry::Owned(version.to_le_bytes().to_vec())
            }
            FW_CFG_RAM_SIZE => FwCfgEntry::Owned(self.ram_size.to_le_bytes().to_vec()),
            FW_CFG_NB_CPUS => FwCfgEntry::Owned(self.cpu_num.to_le_bytes().to_vec()),
            FW_CFG_MAX_CPUS => FwCfgEntry::Owned(self.cpu_num.to_le_bytes().to_vec()),
            FW_CFG_KERNEL_SIZE => {
                FwCfgEntry::Owned((self.kernel.kernel().len() as u32).to_le_bytes().to_vec())
            }
            FW_CFG_KERNEL_DATA => FwCfgEntry::Bytes(self.kernel.kernel()),
            FW_CFG_INITRD_SIZE => {
                let size = self.initrd.as_ref().map_or(0, |initrd| initrd.len()) as u32;
                FwCfgEntry::Owned(size.to_le_bytes().to_vec())
            }
            FW_CFG_INITRD_DATA => FwCfgEntry::Bytes(self.initrd.as_deref().unwrap_or_default()),
            FW_CFG_CMDLINE_SIZE => {
                FwCfgEntry::Owned((self.cmdline.len() as u32).to_le_bytes().to_vec())
            }
            FW_CFG_CMDLINE_DATA => FwCfgEntry::Bytes(&self.cmdline),
            FW_CFG_KERNEL_SETUP_SIZE => {
                FwCfgEntry::Owned((self.kernel.setup().len() as u32).to_le_bytes().to_vec())
            }
            FW_CFG_KERNEL_SETUP_DATA => FwCfgEntry::Bytes(self.kernel.setup()),
            FW_CFG_FILE_DIR => FwCfgEntry::Bytes(&self.file_dir),
            FW_CFG_FILE_FIRST => FwCfgEntry::Bytes(&self.memmap),
            FW_CFG_SMBIOS_TABLES => FwCfgEntry::Bytes(&self.smbios_tables),
            FW_CFG_SMBIOS_ANCHOR => FwCfgEntry::Bytes(&self.smbios_anchor),
            FW_CFG_ACPI_TABLES => FwCfgEntry::Bytes(&self.acpi_tables),
            FW_CFG_ACPI_RSDP => FwCfgEntry::Bytes(&self.acpi_rsdp),
            FW_CFG_ACPI_LOADER => FwCfgEntry::Bytes(&self.acpi_loader),
            FW_CFG_BOOT_KERNEL => FwCfgEntry::Bytes(&self.boot_kernel),
            FW_CFG_BOOT_INITRD => FwCfgEntry::Bytes(self.initrd.as_deref().unwrap_or_default()),
            FW_CFG_BOOT_CMDLINE => FwCfgEntry::Bytes(&self.cmdline),
            _ => FwCfgEntry::Bytes(&[]),
        }
    }

    pub(crate) fn read_data(&self, width: AccessWidth) -> usize {
        let mut state = self.state.lock();
        let entry = self.selected_bytes(state.selected);
        let data = entry.as_slice();
        let mut value = 0usize;
        let mut remaining = width.size();
        let old_offset = state.offset;

        let mut shift = 0;
        while remaining > 0 && state.offset < data.len() {
            value |= (data[state.offset] as usize) << shift;
            state.offset += 1;
            remaining -= 1;
            shift += 8;
        }
        let old_mib = old_offset >> 20;
        let new_mib = state.offset >> 20;
        if state.selected == FW_CFG_KERNEL_DATA && new_mib > old_mib {
            trace!(
                "fw_cfg kernel read progress: {:#x}/{:#x}",
                state.offset,
                data.len()
            );
        }
        if matches!(state.selected, FW_CFG_CMDLINE_DATA | FW_CFG_CMDLINE_SIZE) && old_offset == 0 {
            trace!(
                "fw_cfg read selector={:#x}, width={:?}, value={:#x}, available={:#x}",
                state.selected,
                width,
                value,
                data.len()
            );
        }
        value
    }

    pub(crate) fn read_selector(&self) -> u16 {
        self.state.lock().selected
    }

    pub(crate) fn select(&self, selector: u16) {
        let mut state = self.state.lock();
        state.selected = selector;
        state.offset = 0;
    }

    fn dma_enabled(&self) -> bool {
        self.size >= FW_CFG_DMA_OFFSET + core::mem::size_of::<u64>()
    }

    /// Returns the stable MMIO resource exposed by this fw_cfg transport.
    fn mmio_resource(&self) -> Resource {
        Resource::MmioRange {
            base: self.base.as_usize() as u64,
            size: self.size as u64,
        }
    }

    fn contains(&self, addr: GuestPhysAddr) -> bool {
        let base = self.base.as_usize();
        let end = base.saturating_add(self.size);
        let addr = addr.as_usize();
        addr >= base && addr < end
    }

    /// Reads a fw_cfg MMIO register.
    fn read_register(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        match addr.as_usize() - self.base.as_usize() {
            FW_CFG_DATA_OFFSET => Ok(self.read_data(width)),
            FW_CFG_SELECTOR_OFFSET => Ok(self.state.lock().selected as usize),
            _ => Ok(0),
        }
    }

    /// Writes a fw_cfg MMIO register.
    fn write_register(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> DeviceResult {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        if offset == FW_CFG_SELECTOR_OFFSET {
            let selector = match width {
                AccessWidth::Byte => val as u16,
                AccessWidth::Word | AccessWidth::Dword | AccessWidth::Qword => {
                    ((val & 0xffff) as u16).swap_bytes()
                }
            };
            self.select(selector);
        }
        Ok(())
    }

    /// Returns whether `addr` belongs to the QEMU fw_cfg DMA address register.
    pub fn is_dma_address(&self, addr: GuestPhysAddr) -> bool {
        if !self.dma_enabled() {
            return false;
        }
        if !self.contains(addr) {
            return false;
        }

        let offset = addr.as_usize() - self.base.as_usize();
        (FW_CFG_DMA_OFFSET..FW_CFG_DMA_OFFSET + core::mem::size_of::<u64>()).contains(&offset)
    }

    /// Records a big-endian DMA descriptor pointer write.
    pub fn write_dma_address(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        value: usize,
    ) -> DeviceManagerResult<Option<GuestPhysAddr>> {
        if !self.is_dma_address(addr) {
            return Ok(None);
        }
        let offset = addr.as_usize() - self.base.as_usize();

        let mut state = self.state.lock();
        match (offset - FW_CFG_DMA_OFFSET, width) {
            (0, AccessWidth::Dword) => {
                let high = (value as u32).swap_bytes() as u64;
                state.dma_address = (high << 32) | (state.dma_address & u32::MAX as u64);
                Ok(None)
            }
            (4, AccessWidth::Dword) => {
                let low = (value as u32).swap_bytes() as u64;
                state.dma_address = (state.dma_address & !u32::MAX as u64) | low;
                Ok(Some(GuestPhysAddr::from_usize(state.dma_address as usize)))
            }
            (0, AccessWidth::Qword) => {
                state.dma_address = (value as u64).swap_bytes();
                Ok(Some(GuestPhysAddr::from_usize(state.dma_address as usize)))
            }
            _ => {
                warn!(
                    "unsupported fw_cfg DMA address write: offset={:#x}, width={:?}",
                    offset, width
                );
                Err(DeviceManagerError::InvalidInput {
                    operation: "write fw_cfg DMA address",
                    detail: format!("offset {offset:#x} does not accept width {width:?}"),
                })
            }
        }
    }

    /// Records a PIO DMA address-register write relative to the DMA window.
    pub(crate) fn write_dma_port(
        &self,
        offset: usize,
        width: AccessWidth,
        value: usize,
    ) -> DeviceManagerResult<Option<GuestPhysAddr>> {
        self.write_dma_value(offset, width, value)
    }

    fn write_dma_value(
        &self,
        offset: usize,
        width: AccessWidth,
        value: usize,
    ) -> DeviceManagerResult<Option<GuestPhysAddr>> {
        let mut state = self.state.lock();
        match (offset, width) {
            (0, AccessWidth::Dword) => {
                let high = (value as u32).swap_bytes() as u64;
                state.dma_address = (high << 32) | (state.dma_address & u32::MAX as u64);
                Ok(None)
            }
            (4, AccessWidth::Dword) => {
                let low = (value as u32).swap_bytes() as u64;
                state.dma_address = (state.dma_address & !u32::MAX as u64) | low;
                Ok(Some(GuestPhysAddr::from_usize(state.dma_address as usize)))
            }
            (0, AccessWidth::Qword) => {
                state.dma_address = (value as u64).swap_bytes();
                Ok(Some(GuestPhysAddr::from_usize(state.dma_address as usize)))
            }
            _ => Err(DeviceManagerError::InvalidInput {
                operation: "write fw_cfg DMA address",
                detail: format!("offset {offset:#x} does not accept width {width:?}"),
            }),
        }
    }

    /// Processes a QEMU fw_cfg DMA descriptor stored in guest physical memory.
    pub fn process_dma<R, W>(
        &self,
        desc_addr: GuestPhysAddr,
        mut read_guest: R,
        mut write_guest: W,
    ) -> DeviceManagerResult
    where
        R: FnMut(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
        W: FnMut(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
    {
        let mut desc = [0u8; FW_CFG_DMA_DESC_SIZE];
        read_guest(desc_addr, &mut desc)?;

        let mut control = u32::from_be_bytes(desc[0..4].try_into().unwrap());
        let length = u32::from_be_bytes(desc[4..8].try_into().unwrap()) as usize;
        let buffer_addr =
            GuestPhysAddr::from_usize(u64::from_be_bytes(desc[8..16].try_into().unwrap()) as usize);
        let result = self.process_dma_command(
            control,
            length,
            buffer_addr,
            &mut read_guest,
            &mut write_guest,
        );
        control = if result.is_ok() {
            0
        } else {
            FW_CFG_DMA_CTL_ERROR
        };
        write_guest(desc_addr, &control.to_be_bytes())?;
        result
    }

    fn process_dma_command<R, W>(
        &self,
        control: u32,
        length: usize,
        buffer_addr: GuestPhysAddr,
        read_guest: &mut R,
        write_guest: &mut W,
    ) -> DeviceManagerResult
    where
        R: FnMut(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
        W: FnMut(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
    {
        validate_dma_buffer(buffer_addr, length)?;

        let mut state = self.state.lock();
        if control & FW_CFG_DMA_CTL_SELECT != 0 {
            state.selected = (control >> 16) as u16;
            state.offset = 0;
        }

        if control & FW_CFG_DMA_CTL_SKIP != 0 {
            state.offset = state.offset.saturating_add(length);
        }

        match control & (FW_CFG_DMA_CTL_READ | FW_CFG_DMA_CTL_WRITE) {
            0 => Ok(()),
            FW_CFG_DMA_CTL_READ => {
                trace!(
                    "fw_cfg DMA read selector={:#x}, offset={:#x}, length={:#x}, target={:#x}",
                    state.selected,
                    state.offset,
                    length,
                    buffer_addr.as_usize()
                );
                let entry = self.selected_bytes(state.selected);
                let data = entry.as_slice();
                let start = state.offset;
                state.offset = state.offset.saturating_add(length);
                drop(state);
                dma_read_entry(data, start, length, buffer_addr, write_guest)
            }
            FW_CFG_DMA_CTL_WRITE => {
                state.offset = state.offset.saturating_add(length);
                drop(state);
                dma_discard_guest_write(length, buffer_addr, read_guest)
            }
            _ => {
                warn!("invalid fw_cfg DMA control {:#x}", control);
                Err(DeviceManagerError::InvalidInput {
                    operation: "process fw_cfg DMA command",
                    detail: format!("invalid control value {control:#x}"),
                })
            }
        }
    }
}

mod data;
mod dma;
mod factory;
mod payload;
mod pio;
#[cfg(test)]
mod tests;

use data::*;
use dma::{dma_discard_guest_write, dma_read_entry, validate_dma_buffer};
pub use factory::{
    FwCfgBuildConfig, FwCfgDeviceFactory, FwCfgDmaDevice, FwCfgPayloadConfig, FwCfgPayloadFactory,
    FwCfgPayloadSlot,
};
pub use payload::FwCfgKernelPayload;
pub use pio::FwCfgPioDevice;
