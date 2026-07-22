use alloc::{format, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceResult, EmuDeviceType};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

use crate::{DeviceManagerError, DeviceManagerResult};

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
const FW_CFG_FILE_DIR: u16 = 0x19;
const FW_CFG_FILE_FIRST: u16 = 0x20;
const FW_CFG_SMBIOS_TABLES: u16 = FW_CFG_FILE_FIRST + 1;
const FW_CFG_SMBIOS_ANCHOR: u16 = FW_CFG_FILE_FIRST + 2;
const FW_CFG_ACPI_TABLES: u16 = FW_CFG_FILE_FIRST + 3;
const FW_CFG_ACPI_RSDP: u16 = FW_CFG_FILE_FIRST + 4;
const FW_CFG_ACPI_LOADER: u16 = FW_CFG_FILE_FIRST + 5;
const FW_CFG_FILE_NAME_SIZE: usize = 56;

const FW_CFG_VERSION: u32 = 0x01;
const FW_CFG_VERSION_DMA: u32 = 0x02;
const MEMMAP_RAM_TYPE: u32 = 1;

const FW_CFG_DATA_OFFSET: usize = 0x00;
const FW_CFG_SELECTOR_OFFSET: usize = 0x08;
const FW_CFG_DMA_OFFSET: usize = 0x10;

const ACPI_TABLE_FILE: &str = "etc/acpi/tables";
const ACPI_RSDP_FILE: &str = "etc/acpi/rsdp";
const ACPI_LOADER_FILE: &str = "etc/table-loader";
const QEMU_LOADER_ENTRY_SIZE: usize = 128;

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

/// Guest RAM metadata exposed through fw_cfg's legacy selectors and memmap.
#[derive(Clone, Debug)]
pub struct FwCfgMemoryConfig {
    pub ram_regions: Vec<FwCfgRamRegion>,
}

/// Immutable resources used to construct one fw_cfg device.
pub struct FwCfgConfig {
    /// Guest MMIO base of the fw_cfg register window.
    pub base: GuestPhysAddr,
    /// Size of the guest MMIO register window.
    pub size: usize,
    /// Guest kernel bytes exposed through fw_cfg selectors.
    pub kernel: &'static [u8],
    /// Optional guest initrd bytes.
    pub initrd: Option<&'static [u8]>,
    /// Optional kernel command line without a required trailing NUL.
    pub cmdline: Option<alloc::string::String>,
    /// Number of guest CPUs reported by fw_cfg.
    pub cpu_num: u16,
    /// Guest RAM metadata used for the legacy selectors and memmap file.
    pub memory: FwCfgMemoryConfig,
    /// ACPI files exposed through the fw_cfg directory.
    pub acpi: FwCfgAcpiFiles,
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
    kernel: &'static [u8],
    initrd: Option<&'static [u8]>,
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
    /// Creates a fw_cfg device from one immutable resource description.
    pub fn new(config: FwCfgConfig) -> Self {
        let FwCfgConfig {
            base,
            size,
            kernel,
            initrd,
            cmdline,
            cpu_num,
            memory,
            acpi,
        } = config;
        let mut cmdline = cmdline.as_deref().unwrap_or("").as_bytes().to_vec();
        if !cmdline.ends_with(&[0]) {
            cmdline.push(0);
        }

        let ram_size = memory
            .ram_regions
            .iter()
            .fold(0u64, |acc, region| acc.saturating_add(region.size));
        let memmap = build_memmap(&memory.ram_regions);
        let smbios_tables = build_smbios_tables();
        let smbios_anchor = build_smbios_anchor();
        let file_dir = build_file_dir(&[
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
        ]);

        Self {
            base,
            size,
            kernel,
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
                FwCfgEntry::Owned((self.kernel.len() as u32).to_le_bytes().to_vec())
            }
            FW_CFG_KERNEL_DATA => FwCfgEntry::Bytes(self.kernel),
            FW_CFG_INITRD_SIZE => {
                let size = self.initrd.map_or(0, |initrd| initrd.len()) as u32;
                FwCfgEntry::Owned(size.to_le_bytes().to_vec())
            }
            FW_CFG_INITRD_DATA => FwCfgEntry::Bytes(self.initrd.unwrap_or(&[])),
            FW_CFG_CMDLINE_SIZE => {
                FwCfgEntry::Owned((self.cmdline.len() as u32).to_le_bytes().to_vec())
            }
            FW_CFG_CMDLINE_DATA => FwCfgEntry::Bytes(&self.cmdline),
            FW_CFG_FILE_DIR => FwCfgEntry::Bytes(&self.file_dir),
            FW_CFG_FILE_FIRST => FwCfgEntry::Bytes(&self.memmap),
            FW_CFG_SMBIOS_TABLES => FwCfgEntry::Bytes(&self.smbios_tables),
            FW_CFG_SMBIOS_ANCHOR => FwCfgEntry::Bytes(&self.smbios_anchor),
            FW_CFG_ACPI_TABLES => FwCfgEntry::Bytes(&self.acpi_tables),
            FW_CFG_ACPI_RSDP => FwCfgEntry::Bytes(&self.acpi_rsdp),
            FW_CFG_ACPI_LOADER => FwCfgEntry::Bytes(&self.acpi_loader),
            _ => FwCfgEntry::Bytes(&[]),
        }
    }

    fn read_data(&self, width: AccessWidth) -> usize {
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

    fn write_selector(&self, width: AccessWidth, value: usize) {
        let mut state = self.state.lock();
        state.selected = match width {
            AccessWidth::Byte => value as u16,
            AccessWidth::Word | AccessWidth::Dword | AccessWidth::Qword => {
                ((value & 0xffff) as u16).swap_bytes()
            }
        };
        state.offset = 0;
        if matches!(
            state.selected,
            FW_CFG_KERNEL_SIZE
                | FW_CFG_KERNEL_DATA
                | FW_CFG_INITRD_SIZE
                | FW_CFG_INITRD_DATA
                | FW_CFG_CMDLINE_SIZE
                | FW_CFG_CMDLINE_DATA
                | FW_CFG_FILE_DIR
                | FW_CFG_ACPI_TABLES
                | FW_CFG_ACPI_RSDP
                | FW_CFG_ACPI_LOADER
        ) {
            trace!("fw_cfg select {:#x}", state.selected);
        }
    }

    fn dma_enabled(&self) -> bool {
        self.size >= FW_CFG_DMA_OFFSET + core::mem::size_of::<u64>()
    }

    /// Returns whether `addr` belongs to the QEMU fw_cfg DMA address register.
    pub fn is_dma_address(&self, addr: GuestPhysAddr) -> bool {
        if !self.dma_enabled() {
            return false;
        }

        let offset = addr.as_usize().saturating_sub(self.base.as_usize());
        (FW_CFG_DMA_OFFSET..FW_CFG_DMA_OFFSET + core::mem::size_of::<u64>()).contains(&offset)
    }

    /// Records a big-endian DMA descriptor pointer write.
    pub fn write_dma_address(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        value: usize,
    ) -> DeviceManagerResult<Option<GuestPhysAddr>> {
        let offset = addr.as_usize() - self.base.as_usize();
        if !self.is_dma_address(addr) {
            return Ok(None);
        }

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

        let mut control = u32::from_be_bytes([desc[0], desc[1], desc[2], desc[3]]);
        let length = u32::from_be_bytes([desc[4], desc[5], desc[6], desc[7]]) as usize;
        let buffer_addr = GuestPhysAddr::from_usize(u64::from_be_bytes([
            desc[8], desc[9], desc[10], desc[11], desc[12], desc[13], desc[14], desc[15],
        ]) as usize);

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

fn validate_dma_buffer(buffer_addr: GuestPhysAddr, length: usize) -> DeviceManagerResult {
    buffer_addr
        .as_usize()
        .checked_add(length)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation: "validate fw_cfg DMA buffer",
            detail: format!(
                "buffer at {:#x} with length {length:#x} overflows the guest address space",
                buffer_addr.as_usize()
            ),
        })?;
    Ok(())
}

fn dma_read_entry<W>(
    data: &[u8],
    start: usize,
    length: usize,
    buffer_addr: GuestPhysAddr,
    write_guest: &mut W,
) -> DeviceManagerResult
where
    W: FnMut(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
{
    let mut remaining = length;
    let mut guest_offset = 0usize;
    let mut data_offset = start.min(data.len());
    let zeroes = [0u8; FW_CFG_DMA_SCRATCH_SIZE];

    while remaining != 0 {
        let chunk_len = remaining.min(FW_CFG_DMA_SCRATCH_SIZE);
        let guest_addr = add_guest_offset(buffer_addr, guest_offset)?;
        let available = data.len().saturating_sub(data_offset).min(chunk_len);
        if available == chunk_len {
            write_guest(guest_addr, &data[data_offset..data_offset + chunk_len])?;
        } else {
            if available != 0 {
                write_guest(guest_addr, &data[data_offset..data_offset + available])?;
            }
            let zero_addr = add_guest_offset(buffer_addr, guest_offset + available)?;
            write_guest(zero_addr, &zeroes[..chunk_len - available])?;
        }

        remaining -= chunk_len;
        guest_offset += chunk_len;
        data_offset = data_offset.saturating_add(chunk_len);
    }

    Ok(())
}

fn dma_discard_guest_write<R>(
    length: usize,
    buffer_addr: GuestPhysAddr,
    read_guest: &mut R,
) -> DeviceManagerResult
where
    R: FnMut(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
{
    let mut scratch = [0u8; FW_CFG_DMA_SCRATCH_SIZE];
    let mut remaining = length;
    let mut guest_offset = 0usize;
    while remaining != 0 {
        let chunk_len = remaining.min(scratch.len());
        let guest_addr = add_guest_offset(buffer_addr, guest_offset)?;
        read_guest(guest_addr, &mut scratch[..chunk_len])?;
        remaining -= chunk_len;
        guest_offset += chunk_len;
    }
    Ok(())
}

fn add_guest_offset(base: GuestPhysAddr, offset: usize) -> DeviceManagerResult<GuestPhysAddr> {
    base.as_usize()
        .checked_add(offset)
        .map(GuestPhysAddr::from_usize)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation: "advance fw_cfg DMA buffer",
            detail: format!(
                "buffer at {:#x} with offset {offset:#x} overflows the guest address space",
                base.as_usize()
            ),
        })
}

struct FwCfgFile<'a> {
    name: &'a str,
    selector: u16,
    size: u32,
}

fn build_file_dir(files: &[FwCfgFile<'_>]) -> Vec<u8> {
    let mut dir = Vec::with_capacity(4 + files.len() * (4 + 2 + 2 + FW_CFG_FILE_NAME_SIZE));
    dir.extend_from_slice(&(files.len() as u32).to_be_bytes());
    for file in files {
        dir.extend_from_slice(&file.size.to_be_bytes());
        dir.extend_from_slice(&file.selector.to_be_bytes());
        dir.extend_from_slice(&0u16.to_be_bytes());
        let name = file.name.as_bytes();
        let name_len = core::cmp::min(name.len(), FW_CFG_FILE_NAME_SIZE);
        dir.extend_from_slice(&name[..name_len]);
        dir.resize(dir.len() + FW_CFG_FILE_NAME_SIZE - name_len, 0);
    }
    dir
}

fn build_memmap(regions: &[FwCfgRamRegion]) -> Vec<u8> {
    let mut memmap = Vec::with_capacity(regions.len() * 24);
    for region in regions {
        if region.size != 0 {
            push_memmap_entry(&mut memmap, region.base, region.size);
        }
    }
    memmap
}

fn push_memmap_entry(memmap: &mut Vec<u8>, base: u64, length: u64) {
    memmap.extend_from_slice(&base.to_le_bytes());
    memmap.extend_from_slice(&length.to_le_bytes());
    memmap.extend_from_slice(&MEMMAP_RAM_TYPE.to_le_bytes());
    memmap.extend_from_slice(&0u32.to_le_bytes());
}

fn build_smbios_tables() -> Vec<u8> {
    let mut table = Vec::with_capacity(6);
    table.push(127);
    table.push(4);
    table.extend_from_slice(&0x7f00u16.to_le_bytes());
    table.extend_from_slice(&[0, 0]);
    table
}

fn build_smbios_anchor() -> Vec<u8> {
    let table = build_smbios_tables();
    let mut anchor = Vec::with_capacity(24);
    anchor.extend_from_slice(b"_SM3_");
    anchor.push(0);
    anchor.push(24);
    anchor.push(3);
    anchor.push(0);
    anchor.push(0);
    anchor.push(1);
    anchor.push(0);
    anchor.extend_from_slice(&(table.len() as u32).to_le_bytes());
    anchor.extend_from_slice(&0u64.to_le_bytes());
    let checksum = (0u8).wrapping_sub(anchor.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    anchor[5] = checksum;
    anchor
}

/// Relocatable ACPI files consumed by QEMU's fw_cfg table loader.
#[derive(Clone, Debug)]
pub struct FwCfgAcpiFiles {
    tables: Vec<u8>,
    rsdp: Vec<u8>,
    loader: Vec<u8>,
}

impl FwCfgAcpiFiles {
    /// Creates validated fw_cfg ACPI files generated by the machine planner.
    pub fn new(tables: Vec<u8>, rsdp: Vec<u8>, loader: Vec<u8>) -> DeviceManagerResult<Self> {
        if tables.len() < 36 || rsdp.get(..8) != Some(b"RSD PTR ") {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "create fw_cfg ACPI files",
                detail: "missing ACPI tables or RSDP signature".into(),
            });
        }
        if loader.is_empty() || !loader.len().is_multiple_of(QEMU_LOADER_ENTRY_SIZE) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "create fw_cfg ACPI files",
                detail: format!(
                    "table-loader length {} is not a non-zero multiple of {}",
                    loader.len(),
                    QEMU_LOADER_ENTRY_SIZE
                ),
            });
        }
        Ok(Self {
            tables,
            rsdp,
            loader,
        })
    }

    /// Returns the concatenated ACPI SDT file.
    pub fn tables(&self) -> &[u8] {
        &self.tables
    }

    /// Returns the relocatable RSDP file.
    pub fn rsdp(&self) -> &[u8] {
        &self.rsdp
    }

    /// Returns QEMU table-loader commands for both files.
    pub fn loader(&self) -> &[u8] {
        &self.loader
    }
}

/// Generates LoongArch ACPI files from an already resolved platform.
enum FwCfgEntry<'a> {
    Bytes(&'a [u8]),
    Owned(Vec<u8>),
}

impl<'a> FwCfgEntry<'a> {
    fn as_slice(&'a self) -> &'a [u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for FwCfg {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::FwCfg
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        match addr.as_usize() - self.base.as_usize() {
            FW_CFG_DATA_OFFSET => Ok(self.read_data(width)),
            FW_CFG_SELECTOR_OFFSET => Ok(self.state.lock().selected as usize),
            _ => Ok(0),
        }
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> DeviceResult {
        let offset = addr.as_usize() - self.base.as_usize();
        if offset == FW_CFG_SELECTOR_OFFSET {
            self.write_selector(width, val);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn dma_read_uses_bounded_chunks_for_large_guest_length() {
        let writes = Cell::new(0usize);
        dma_read_entry(
            b"abc",
            0,
            FW_CFG_DMA_SCRATCH_SIZE * 2 + 17,
            GuestPhysAddr::from_usize(0x8000),
            &mut |_addr, buffer| {
                assert!(buffer.len() <= FW_CFG_DMA_SCRATCH_SIZE);
                writes.set(writes.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(writes.get() > 1);
    }

    #[test]
    fn dma_write_discard_uses_bounded_chunks_for_large_guest_length() {
        let reads = Cell::new(0usize);
        dma_discard_guest_write(
            FW_CFG_DMA_SCRATCH_SIZE * 2 + 17,
            GuestPhysAddr::from_usize(0x8000),
            &mut |_addr, buffer| {
                assert!(buffer.len() <= FW_CFG_DMA_SCRATCH_SIZE);
                buffer.fill(0xaa);
                reads.set(reads.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert!(reads.get() > 1);
    }

    #[test]
    fn dma_rejects_buffer_address_overflow() {
        assert!(validate_dma_buffer(GuestPhysAddr::from_usize(usize::MAX), 2).is_err());
    }
}
