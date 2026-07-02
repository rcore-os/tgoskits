use alloc::{boxed::Box, format, vec};
use core::{
    mem::size_of,
    ptr::addr_of_mut,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    time::Duration,
};

use axklib::time::busy_wait;
use log::{debug, info, trace, warn};
use rdif_block::{
    BlkError, DeviceInfo, DriverGeneric, Event, IQueue, IdList, Interface, IrqHandler,
    IrqSourceInfo, IrqSourceList, QueueInfo, QueueLimits, Request, RequestFlags, RequestId,
    RequestOp, RequestStatus, Segment, validate_request,
};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};

use super::PlatformDeviceBlock;
use crate::{
    BindingInfo, binding_info_from_fdt,
    mmio::{firmware_addr_to_phys, iomap_firmware_device},
};

const REG_CAP: usize = 0x00;
const REG_GHC: usize = 0x04;
const REG_IS: usize = 0x08;
const REG_PI: usize = 0x0c;
const REG_VS: usize = 0x10;
const REG_CAP2: usize = 0x24;
const REG_BOHC: usize = 0x28;

const HOST_CAP_SSS: u32 = 0x1 << 27;
const HOST_CAP_MPS: u32 = 0x1 << 28;
const HOST_CTL_RESET: u32 = 0x1 << 0;
const HOST_CTL_IRQ_EN: u32 = 0x1 << 1;
const HOST_CTL_AHCI_EN: u32 = 0x1 << 31;

const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;
const PORT_CLB: usize = 0x00;
const PORT_CLBU: usize = 0x04;
const PORT_FB: usize = 0x08;
const PORT_FBU: usize = 0x0c;
const PORT_IS: usize = 0x10;
const PORT_IE: usize = 0x14;
const PORT_CMD: usize = 0x18;
const PORT_TFD: usize = 0x20;
const PORT_SIG: usize = 0x24;
const PORT_SSTS: usize = 0x28;
const PORT_SCTL: usize = 0x2c;
const PORT_SERR: usize = 0x30;
const PORT_CI: usize = 0x38;

const PORT_IRQ_DHRS: u32 = 1 << 0;
const PORT_IRQ_PSS: u32 = 1 << 1;
const PORT_IRQ_DSS: u32 = 1 << 2;
const PORT_IRQ_SDBS: u32 = 1 << 3;
const PORT_IRQ_DPS: u32 = 1 << 5;
const PORT_IRQ_PCS: u32 = 1 << 6;
const PORT_IRQ_PRCS: u32 = 1 << 22;
const PORT_IRQ_INFS: u32 = 1 << 26;
const PORT_IRQ_IFS: u32 = 1 << 27;
const PORT_IRQ_HBDS: u32 = 1 << 28;
const PORT_IRQ_HBFS: u32 = 1 << 29;
const PORT_IRQ_TFES: u32 = 1 << 30;
const PORT_IRQ_COMPLETION: u32 =
    PORT_IRQ_DHRS | PORT_IRQ_PSS | PORT_IRQ_DSS | PORT_IRQ_SDBS | PORT_IRQ_DPS;
const PORT_IRQ_ERROR: u32 = PORT_IRQ_PCS
    | PORT_IRQ_PRCS
    | PORT_IRQ_INFS
    | PORT_IRQ_IFS
    | PORT_IRQ_HBDS
    | PORT_IRQ_HBFS
    | PORT_IRQ_TFES;
const PORT_IRQ_ENABLE_MASK: u32 = PORT_IRQ_COMPLETION | PORT_IRQ_ERROR;

const PORT_CMD_ICC_MASK: u32 = 0xf << 28;
const PORT_CMD_ICC_ACTIVE: u32 = 0x1 << 28;
const PORT_CMD_LIST_ON: u32 = 0x1 << 15;
const PORT_CMD_FIS_ON: u32 = 0x1 << 14;
const PORT_CMD_FIS_RX: u32 = 0x1 << 4;
const PORT_CMD_POWER_ON: u32 = 0x1 << 2;
const PORT_CMD_SPIN_UP: u32 = 0x1 << 1;
const PORT_CMD_START: u32 = 0x1 << 0;

const PORT_SCTL_DET_MASK: u32 = 0x0f;
const PORT_SCTL_DET_NONE: u32 = 0x0;
const PORT_SCTL_DET_INIT: u32 = 0x1;

const PORT_TFD_ERR: u32 = 0x1 << 0;
const PORT_TFD_DRQ: u32 = 0x1 << 3;
const PORT_TFD_BSY: u32 = 0x1 << 7;

const AHCI_COMRESET_ASSERT_MILLIS: u64 = 10;
const AHCI_DEVICE_READY_TIMEOUT_MILLIS: usize = 5000;
const AHCI_HBA_RESET_TIMEOUT_MILLIS: usize = 1000;
const AHCI_COMMAND_TIMEOUT_MILLIS: usize = 5000;
const AHCI_LINK_TIMEOUT_MILLIS: usize = 1000;

const AHCI_CMD_LIST_SIZE: usize = 1024;
const AHCI_RX_FIS_SIZE: usize = 256;
const AHCI_IDENTIFY_SIZE: usize = 512;
const AHCI_SECTOR_SIZE: usize = 512;

// Keep request data behind a single bounce buffer until LS2K1000 AHCI direct
// DMA to caller buffers has validated cache-maintenance semantics. The block
// queue limits below mirror this temporary 64 KiB transfer window.
const AHCI_MAX_TRANSFER_SECTORS: usize = 128;
const AHCI_TRANSFER_BUFFER_SIZE: usize = AHCI_SECTOR_SIZE * AHCI_MAX_TRANSFER_SECTORS;
const AHCI_CMD_TABLE_PRDT_OFFSET: usize = 128;
const AHCI_PRDT_ENTRY_SIZE: usize = 16;
const AHCI_MAX_PRDT_ENTRIES: usize = AHCI_MAX_TRANSFER_SECTORS;
const AHCI_PRDT_BYTE_COUNT_MASK: u32 = 0x3f_ffff;
const AHCI_PRDT_INTERRUPT_ON_COMPLETION: u32 = 1 << 31;
const AHCI_PRDT_MAX_BYTES: usize = AHCI_PRDT_BYTE_COUNT_MASK as usize + 1;
const AHCI_CMD_TABLE_SIZE: usize =
    AHCI_CMD_TABLE_PRDT_OFFSET + AHCI_MAX_PRDT_ENTRIES * AHCI_PRDT_ENTRY_SIZE;

const SATA_FIS_TYPE_REGISTER_H2D: u8 = 0x27;
const SATA_FIS_H2D_COMMAND: u8 = 0x80;
const ATA_CMD_IDENTIFY_DEVICE: u8 = 0xec;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xea;
const ATA_DEVICE_LBA: u8 = 0x40;
const AHCI_CMD_SLOT0: u32 = 1;
const DEVICE_NAME: &str = "ls2k1000-ahci";
const DEFAULT_MMIO_SIZE: usize = 0x10_000;
const DEFAULT_PORTS_IMPLEMENTED: u32 = 0x1;

crate::model_register!(
    name: "LS2K1000 AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,ls-ahci",
            "loongson,ls2k1000-ahci",
            "loongson,2k1000-ahci",
            "generic-ahci",
            "snps,dwc-ahci",
        ],
        on_probe: probe_fdt,
    }],
);

#[repr(C, align(1024))]
struct AhciDma {
    cmd_list: [u8; AHCI_CMD_LIST_SIZE],
    rx_fis: [u8; AHCI_RX_FIS_SIZE],
    cmd_table: [u8; AHCI_CMD_TABLE_SIZE],
    identify: [u8; AHCI_IDENTIFY_SIZE],
    buffer: [u8; AHCI_TRANSFER_BUFFER_SIZE],
}

#[repr(C)]
struct AhciCmdHeader {
    opts: u32,
    status: u32,
    tbl_addr_lo: u32,
    tbl_addr_hi: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct AhciPrdtEntry {
    addr_lo: u32,
    addr_hi: u32,
    reserved: u32,
    flags_size: u32,
}

struct DmaPtrs {
    cmd_list: *mut u8,
    rx_fis: *mut u8,
    cmd_table: *mut u8,
    identify: *mut u8,
    buffer: *mut u8,
}

#[derive(Clone, Copy)]
struct AhciDmaSegment {
    bus: u64,
    len: usize,
}

impl AhciDmaSegment {
    const fn new(bus: u64, len: usize) -> Self {
        Self { bus, len }
    }
}

struct AtaDmaCommand<'a> {
    command: u8,
    segments: &'a [AhciDmaSegment],
    lba: u64,
    sectors: u16,
    device: u8,
    write: bool,
    label: &'a str,
}

struct AtaNoDataCommand<'a> {
    command: u8,
    label: &'a str,
}

struct AhciController;

#[derive(Clone, Copy)]
struct AhciPort {
    index: usize,
}

struct AhciBlock {
    port: AhciPort,
    capacity_blocks: u64,
    queue_created: bool,
    irq_enabled: AtomicBool,
    irq_handler_taken: bool,
}

struct AhciQueue {
    id: usize,
    port: AhciPort,
    capacity_blocks: u64,
}

struct AhciIrqHandler {
    port: AhciPort,
}

#[derive(Debug)]
enum AhciError {
    InvalidBufferSize,
    LbaOutOfRange,
    CommandFailed,
}

static mut AHCI_DMA: AhciDma = AhciDma {
    cmd_list: [0; AHCI_CMD_LIST_SIZE],
    rx_fis: [0; AHCI_RX_FIS_SIZE],
    cmd_table: [0; AHCI_CMD_TABLE_SIZE],
    identify: [0; AHCI_IDENTIFY_SIZE],
    buffer: [0; AHCI_TRANSFER_BUFFER_SIZE],
};

static AHCI_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
static AHCI_PADDR: AtomicUsize = AtomicUsize::new(0);
static AHCI_PORTS_IMPLEMENTED: AtomicU32 = AtomicU32::new(DEFAULT_PORTS_IMPLEMENTED);

fn ahci_base() -> *mut u8 {
    AHCI_MMIO_BASE.load(Ordering::Acquire) as *mut u8
}

fn read_reg_u32(offset: usize) -> u32 {
    unsafe { ahci_base().add(offset).cast::<u32>().read_volatile() }
}

fn write_reg_u32(offset: usize, value: u32) {
    unsafe { ahci_base().add(offset).cast::<u32>().write_volatile(value) }
}

fn read_port_reg_u32(port: usize, offset: usize) -> u32 {
    read_reg_u32(PORT_BASE + port * PORT_STRIDE + offset)
}

fn write_port_reg_u32(port: usize, offset: usize, value: u32) {
    write_reg_u32(PORT_BASE + port * PORT_STRIDE + offset, value)
}

fn dma_barrier() {
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("dbar 0");
    }

    #[cfg(not(target_arch = "loongarch64"))]
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

fn dma_ptrs() -> DmaPtrs {
    unsafe {
        let dma = addr_of_mut!(AHCI_DMA);
        DmaPtrs {
            cmd_list: addr_of_mut!((*dma).cmd_list).cast::<u8>(),
            rx_fis: addr_of_mut!((*dma).rx_fis).cast::<u8>(),
            cmd_table: addr_of_mut!((*dma).cmd_table).cast::<u8>(),
            identify: addr_of_mut!((*dma).identify).cast::<u8>(),
            buffer: addr_of_mut!((*dma).buffer).cast::<u8>(),
        }
    }
}

fn clear_dma() -> DmaPtrs {
    let ptrs = dma_ptrs();
    unsafe {
        (addr_of_mut!(AHCI_DMA) as *mut u8).write_bytes(0, size_of::<AhciDma>());
    }
    ptrs
}

fn dma_paddr(ptr: *const u8) -> u64 {
    axklib::mem::virt_to_phys((ptr as usize).into()).as_usize() as u64
}

fn write_addr_pair(port: usize, lo_offset: usize, hi_offset: usize, paddr: u64) {
    write_port_reg_u32(port, lo_offset, paddr as u32);
    write_port_reg_u32(port, hi_offset, (paddr >> 32) as u32);
}

fn port_count(cap: u32) -> usize {
    ((cap & 0x1f) + 1) as usize
}

fn ssts_det(ssts: u32) -> u32 {
    ssts & 0x0f
}

fn ssts_spd(ssts: u32) -> u32 {
    (ssts >> 4) & 0x0f
}

fn ssts_ipm(ssts: u32) -> u32 {
    (ssts >> 8) & 0x0f
}

fn wait_hba_reset_done() -> bool {
    for _ in 0..AHCI_HBA_RESET_TIMEOUT_MILLIS {
        if read_reg_u32(REG_GHC) & HOST_CTL_RESET == 0 {
            return true;
        }
        busy_wait(Duration::from_millis(1));
    }
    false
}

fn reset_hba() {
    let ghc = read_reg_u32(REG_GHC);
    trace!("AHCI HBA reset: ghc={ghc:#010x}");

    write_reg_u32(REG_GHC, ghc | HOST_CTL_RESET);
    if !wait_hba_reset_done() {
        warn!("AHCI HBA reset did not complete");
        return;
    }

    let ghc = read_reg_u32(REG_GHC);
    write_reg_u32(REG_GHC, ghc | HOST_CTL_AHCI_EN);
    busy_wait(Duration::from_millis(1));

    trace!("AHCI HBA reset done: ghc={:#010x}", read_reg_u32(REG_GHC));
}

fn configure_hba_cap() {
    let cap = read_reg_u32(REG_CAP);
    let new_cap = cap | HOST_CAP_MPS | HOST_CAP_SSS;
    if new_cap == cap {
        return;
    }

    trace!("AHCI CAP update: {cap:#010x} -> {new_cap:#010x}");
    write_reg_u32(REG_CAP, new_cap);
    trace!("AHCI CAP after update: {:#010x}", read_reg_u32(REG_CAP));
}

fn log_port(port: usize, stage: &str) {
    let cmd = read_port_reg_u32(port, PORT_CMD);
    let tfd = read_port_reg_u32(port, PORT_TFD);
    let sig = read_port_reg_u32(port, PORT_SIG);
    let ssts = read_port_reg_u32(port, PORT_SSTS);
    let sctl = read_port_reg_u32(port, PORT_SCTL);
    let serr = read_port_reg_u32(port, PORT_SERR);

    trace!(
        "AHCI port{port} {stage}: cmd={cmd:#010x}, tfd={tfd:#010x}, sig={sig:#010x}, \
         ssts={ssts:#010x}, sctl={sctl:#010x}, serr={serr:#010x}, det={}, spd={}, ipm={}",
        ssts_det(ssts),
        ssts_spd(ssts),
        ssts_ipm(ssts),
    );
}

fn power_up_port(port: usize) {
    let cmd = read_port_reg_u32(port, PORT_CMD);
    if cmd & (PORT_CMD_LIST_ON | PORT_CMD_FIS_ON | PORT_CMD_FIS_RX | PORT_CMD_START) != 0 {
        warn!("AHCI port{port} command engine is already active: cmd={cmd:#010x}");
    }

    let new_cmd =
        (cmd & !PORT_CMD_ICC_MASK) | PORT_CMD_ICC_ACTIVE | PORT_CMD_POWER_ON | PORT_CMD_SPIN_UP;
    write_port_reg_u32(port, PORT_CMD, new_cmd);
}

fn wait_port_link(port: usize, stage: &str, warn_on_timeout: bool) -> bool {
    for _ in 0..AHCI_LINK_TIMEOUT_MILLIS {
        let ssts = read_port_reg_u32(port, PORT_SSTS);
        if ssts_det(ssts) == 0x3 {
            trace!(
                "AHCI port{port} link up after {stage}: ssts={ssts:#010x}, spd={}, ipm={}",
                ssts_spd(ssts),
                ssts_ipm(ssts),
            );
            return true;
        }
        busy_wait(Duration::from_millis(1));
    }

    let ssts = read_port_reg_u32(port, PORT_SSTS);
    if warn_on_timeout {
        warn!(
            "AHCI port{port} link not up after {stage} and {AHCI_LINK_TIMEOUT_MILLIS}ms: \
             ssts={ssts:#010x}, det={}, spd={}, ipm={}",
            ssts_det(ssts),
            ssts_spd(ssts),
            ssts_ipm(ssts),
        );
    } else {
        trace!(
            "AHCI port{port} link not up after {stage} and {AHCI_LINK_TIMEOUT_MILLIS}ms: \
             ssts={ssts:#010x}, det={}, spd={}, ipm={}",
            ssts_det(ssts),
            ssts_spd(ssts),
            ssts_ipm(ssts),
        );
    }
    false
}

fn clear_port_errors(port: usize, stage: &str) {
    let serr = read_port_reg_u32(port, PORT_SERR);
    if serr == 0 {
        return;
    }

    write_port_reg_u32(port, PORT_SERR, serr);
    trace!(
        "AHCI port{port} clear SERR after {stage}: {serr:#010x} -> {:#010x}",
        read_port_reg_u32(port, PORT_SERR),
    );
}

fn wait_port_ready(port: usize) -> bool {
    for _ in 0..AHCI_DEVICE_READY_TIMEOUT_MILLIS {
        let tfd = read_port_reg_u32(port, PORT_TFD);
        if tfd & (PORT_TFD_BSY | PORT_TFD_DRQ) == 0 {
            trace!("AHCI port{port} device ready: tfd={tfd:#010x}");
            return true;
        }
        busy_wait(Duration::from_millis(1));
    }

    let tfd = read_port_reg_u32(port, PORT_TFD);
    warn!(
        "AHCI port{port} device not ready after {AHCI_DEVICE_READY_TIMEOUT_MILLIS}ms: \
         tfd={tfd:#010x}"
    );
    false
}

fn start_command_engine(port: usize, ptrs: &DmaPtrs) -> bool {
    let cmd_list_paddr = dma_paddr(ptrs.cmd_list);
    let rx_fis_paddr = dma_paddr(ptrs.rx_fis);

    write_addr_pair(port, PORT_CLB, PORT_CLBU, cmd_list_paddr);
    write_addr_pair(port, PORT_FB, PORT_FBU, rx_fis_paddr);
    write_port_reg_u32(port, PORT_IE, 0);
    write_port_reg_u32(port, PORT_IS, u32::MAX);
    write_reg_u32(REG_IS, 1u32 << port);

    let cmd = read_port_reg_u32(port, PORT_CMD);
    let new_cmd = (cmd & !PORT_CMD_ICC_MASK)
        | PORT_CMD_ICC_ACTIVE
        | PORT_CMD_FIS_RX
        | PORT_CMD_POWER_ON
        | PORT_CMD_SPIN_UP
        | PORT_CMD_START;
    write_port_reg_u32(port, PORT_CMD, new_cmd);
    dma_barrier();

    trace!(
        "AHCI port{port} command engine started: clb={cmd_list_paddr:#x}, fb={rx_fis_paddr:#x}, \
         cmd={:#010x}",
        read_port_reg_u32(port, PORT_CMD),
    );

    wait_port_ready(port)
}

fn setup_ata_dma_command(
    port: usize,
    ptrs: &DmaPtrs,
    command: AtaDmaCommand<'_>,
) -> Result<(), AhciError> {
    if command.segments.is_empty() || command.segments.len() > AHCI_MAX_PRDT_ENTRIES {
        return Err(AhciError::InvalidBufferSize);
    }
    if command
        .segments
        .iter()
        .any(|segment| segment.len == 0 || segment.len > AHCI_PRDT_MAX_BYTES)
    {
        return Err(AhciError::InvalidBufferSize);
    }

    let cmd_table_paddr = dma_paddr(ptrs.cmd_table);
    let bytes = command
        .segments
        .iter()
        .try_fold(0usize, |total, segment| total.checked_add(segment.len))
        .ok_or(AhciError::InvalidBufferSize)?;
    let first_bus = command.segments[0].bus;

    unsafe {
        ptrs.cmd_table.write_bytes(0, AHCI_CMD_TABLE_SIZE);

        let cfis = ptrs.cmd_table;
        cfis.add(0).write(SATA_FIS_TYPE_REGISTER_H2D);
        cfis.add(1).write(SATA_FIS_H2D_COMMAND);
        cfis.add(2).write(command.command);
        cfis.add(4).write(command.lba as u8);
        cfis.add(5).write((command.lba >> 8) as u8);
        cfis.add(6).write((command.lba >> 16) as u8);
        cfis.add(7).write(command.device);
        cfis.add(8).write((command.lba >> 24) as u8);
        cfis.add(9).write((command.lba >> 32) as u8);
        cfis.add(10).write((command.lba >> 40) as u8);
        cfis.add(12).write(command.sectors as u8);
        cfis.add(13).write((command.sectors >> 8) as u8);

        let write_flag = if command.write { 1 << 6 } else { 0 };
        ptrs.cmd_list.cast::<AhciCmdHeader>().write(AhciCmdHeader {
            opts: ((size_of::<[u8; 20]>() / 4) as u32)
                | write_flag
                | ((command.segments.len() as u32) << 16),
            status: 0,
            tbl_addr_lo: cmd_table_paddr as u32,
            tbl_addr_hi: (cmd_table_paddr >> 32) as u32,
            reserved: [0; 4],
        });

        let prdt = ptrs
            .cmd_table
            .add(AHCI_CMD_TABLE_PRDT_OFFSET)
            .cast::<AhciPrdtEntry>();
        for (index, segment) in command.segments.iter().enumerate() {
            let mut flags_size = (segment.len as u32 - 1) & AHCI_PRDT_BYTE_COUNT_MASK;
            if index + 1 == command.segments.len() {
                flags_size |= AHCI_PRDT_INTERRUPT_ON_COMPLETION;
            }
            prdt.add(index).write(AhciPrdtEntry {
                addr_lo: segment.bus as u32,
                addr_hi: (segment.bus >> 32) as u32,
                reserved: 0,
                flags_size,
            });
        }
    }

    write_port_reg_u32(port, PORT_IS, u32::MAX);
    write_reg_u32(REG_IS, 1u32 << port);
    dma_barrier();

    let label = command.label;
    trace!(
        "AHCI port{port} {label} setup: lba={}, sectors={}, ctba={cmd_table_paddr:#x}, prdt={}, \
         bytes={}, buf={first_bus:#x}",
        command.lba,
        command.sectors,
        command.segments.len(),
        bytes,
    );

    Ok(())
}

fn setup_ata_nodata_command(
    port: usize,
    ptrs: &DmaPtrs,
    command: AtaNoDataCommand<'_>,
) -> Result<(), AhciError> {
    let cmd_table_paddr = dma_paddr(ptrs.cmd_table);

    unsafe {
        ptrs.cmd_table.write_bytes(0, AHCI_CMD_TABLE_SIZE);

        let cfis = ptrs.cmd_table;
        cfis.add(0).write(SATA_FIS_TYPE_REGISTER_H2D);
        cfis.add(1).write(SATA_FIS_H2D_COMMAND);
        cfis.add(2).write(command.command);

        ptrs.cmd_list.cast::<AhciCmdHeader>().write(AhciCmdHeader {
            opts: (size_of::<[u8; 20]>() / 4) as u32,
            status: 0,
            tbl_addr_lo: cmd_table_paddr as u32,
            tbl_addr_hi: (cmd_table_paddr >> 32) as u32,
            reserved: [0; 4],
        });
    }

    write_port_reg_u32(port, PORT_IS, u32::MAX);
    write_reg_u32(REG_IS, 1u32 << port);
    dma_barrier();

    let label = command.label;
    trace!("AHCI port{port} {label} setup: ctba={cmd_table_paddr:#x}");
    Ok(())
}

fn setup_identify_command(port: usize, ptrs: &DmaPtrs) -> Result<(), AhciError> {
    let segments = [AhciDmaSegment {
        bus: dma_paddr(ptrs.identify),
        len: AHCI_IDENTIFY_SIZE,
    }];
    setup_ata_dma_command(
        port,
        ptrs,
        AtaDmaCommand {
            command: ATA_CMD_IDENTIFY_DEVICE,
            segments: &segments,
            lba: 0,
            sectors: 0,
            device: 0,
            write: false,
            label: "IDENTIFY",
        },
    )
}

fn wait_command_done(port: usize) -> bool {
    for _ in 0..AHCI_COMMAND_TIMEOUT_MILLIS {
        if read_port_reg_u32(port, PORT_CI) & AHCI_CMD_SLOT0 == 0 {
            dma_barrier();
            let is = read_port_reg_u32(port, PORT_IS);
            let tfd = read_port_reg_u32(port, PORT_TFD);
            trace!("AHCI port{port} command done: is={is:#010x}, tfd={tfd:#010x}");
            return tfd & (PORT_TFD_BSY | PORT_TFD_DRQ | PORT_TFD_ERR) == 0;
        }
        busy_wait(Duration::from_millis(1));
    }

    warn!(
        "AHCI port{port} command timeout: ci={:#010x}, is={:#010x}, tfd={:#010x}",
        read_port_reg_u32(port, PORT_CI),
        read_port_reg_u32(port, PORT_IS),
        read_port_reg_u32(port, PORT_TFD),
    );
    false
}

fn read_identify_word(ptrs: &DmaPtrs, word: usize) -> u16 {
    unsafe { ptrs.identify.cast::<u16>().add(word).read_volatile() }
}

fn read_identify_string<const N: usize>(ptrs: &DmaPtrs, first_word: usize) -> [u8; N] {
    let mut out = [0; N];
    for i in 0..N / 2 {
        let word = read_identify_word(ptrs, first_word + i);
        out[i * 2] = (word >> 8) as u8;
        out[i * 2 + 1] = word as u8;
    }
    out
}

fn identify_lba28(ptrs: &DmaPtrs) -> u32 {
    read_identify_word(ptrs, 60) as u32 | ((read_identify_word(ptrs, 61) as u32) << 16)
}

fn identify_lba48(ptrs: &DmaPtrs) -> u64 {
    read_identify_word(ptrs, 100) as u64
        | ((read_identify_word(ptrs, 101) as u64) << 16)
        | ((read_identify_word(ptrs, 102) as u64) << 32)
        | ((read_identify_word(ptrs, 103) as u64) << 48)
}

fn identify_capacity(ptrs: &DmaPtrs) -> u64 {
    let lba48 = identify_lba48(ptrs);
    if lba48 != 0 {
        lba48
    } else {
        identify_lba28(ptrs) as u64
    }
}

fn log_identify_data(ptrs: &DmaPtrs) {
    let model = read_identify_string::<40>(ptrs, 27);
    let serial = read_identify_string::<20>(ptrs, 10);
    let model = core::str::from_utf8(&model).unwrap_or("<invalid>");
    let serial = core::str::from_utf8(&serial).unwrap_or("<invalid>");
    let lba28 = identify_lba28(ptrs);
    let lba48 = identify_lba48(ptrs);

    trace!(
        "AHCI IDENTIFY: model='{model}', serial='{serial}', lba28={lba28}, lba48={lba48}, \
         word0={:#06x}, word83={:#06x}",
        read_identify_word(ptrs, 0),
        read_identify_word(ptrs, 83),
    );
}

fn identify_device(port: usize, ptrs: &DmaPtrs) -> Option<u64> {
    if let Err(err) = setup_identify_command(port, ptrs) {
        warn!("AHCI port{port} failed to setup IDENTIFY: {err:?}");
        return None;
    }
    write_port_reg_u32(port, PORT_CI, AHCI_CMD_SLOT0);

    if !wait_command_done(port) {
        return None;
    }

    log_identify_data(ptrs);
    Some(identify_capacity(ptrs))
}

fn ahci_queue_limits() -> QueueLimits {
    QueueLimits {
        supports_flush: true,
        supported_flags: RequestFlags::PREFLUSH | RequestFlags::FUA,
        max_blocks_per_request: AHCI_MAX_TRANSFER_SECTORS as u32,
        max_segments: AHCI_MAX_TRANSFER_SECTORS,
        max_segment_size: AHCI_TRANSFER_BUFFER_SIZE,
        ..QueueLimits::simple(AHCI_SECTOR_SIZE, u64::MAX)
    }
}

fn request_segments_len(segments: &[Segment<'_>]) -> usize {
    segments.iter().map(|segment| segment.len).sum()
}

fn copy_from_request_segments(segments: &[Segment<'_>], offset: usize, dst: &mut [u8]) {
    let mut skipped = offset;
    let mut copied = 0;

    for segment in segments {
        if copied == dst.len() {
            break;
        }
        if skipped >= segment.len {
            skipped -= segment.len;
            continue;
        }

        let start = skipped;
        let len = (segment.len - start).min(dst.len() - copied);
        dst[copied..copied + len].copy_from_slice(&segment[start..start + len]);
        copied += len;
        skipped = 0;
    }
}

fn copy_to_request_segments(segments: &mut [Segment<'_>], offset: usize, src: &[u8]) {
    let mut skipped = offset;
    let mut copied = 0;

    for segment in segments {
        if copied == src.len() {
            break;
        }
        if skipped >= segment.len {
            skipped -= segment.len;
            continue;
        }

        let start = skipped;
        let len = (segment.len - start).min(src.len() - copied);
        segment[start..start + len].copy_from_slice(&src[copied..copied + len]);
        copied += len;
        skipped = 0;
    }
}

fn comreset_port(port: usize) {
    let serr = read_port_reg_u32(port, PORT_SERR);
    if serr != 0 {
        write_port_reg_u32(port, PORT_SERR, serr);
    }

    let sctl = read_port_reg_u32(port, PORT_SCTL);
    trace!("AHCI port{port} COMRESET: sctl={sctl:#010x}, serr={serr:#010x}");

    write_port_reg_u32(
        port,
        PORT_SCTL,
        (sctl & !PORT_SCTL_DET_MASK) | PORT_SCTL_DET_INIT,
    );
    busy_wait(Duration::from_millis(AHCI_COMRESET_ASSERT_MILLIS));
    write_port_reg_u32(
        port,
        PORT_SCTL,
        (sctl & !PORT_SCTL_DET_MASK) | PORT_SCTL_DET_NONE,
    );
}

fn bring_up_port_link(port: usize) -> bool {
    power_up_port(port);
    if wait_port_link(port, "power-up", false) {
        return true;
    }

    comreset_port(port);
    power_up_port(port);
    wait_port_link(port, "COMRESET", true)
}

fn effective_port_map(raw_pi: u32) -> u32 {
    let board_pi = AHCI_PORTS_IMPLEMENTED.load(Ordering::Acquire);
    if raw_pi != 0 || board_pi == 0 {
        return raw_pi;
    }

    trace!("AHCI PI is zero; applying board ports-implemented={board_pi:#010x}");
    write_reg_u32(REG_PI, board_pi);

    let new_pi = read_reg_u32(REG_PI);
    if new_pi == 0 {
        trace!("AHCI PI write did not latch; probing board port map in software");
        board_pi
    } else {
        trace!("AHCI PI after board port map write: {new_pi:#010x}");
        new_pi
    }
}

impl AhciController {
    fn probe() -> Option<AhciBlock> {
        let (cap, pi) = Self::init_hba()?;

        for port in 0..port_count(cap).min(32) {
            if pi & (1u32 << port) != 0
                && let Some(block) = AhciPort::new(port).init_block()
            {
                return Some(block);
            }
        }

        warn!("AHCI HBA has no usable ports");
        None
    }

    fn init_hba() -> Option<(u32, u32)> {
        reset_hba();
        configure_hba_cap();

        let cap = read_reg_u32(REG_CAP);
        let ghc = read_reg_u32(REG_GHC);
        let is = read_reg_u32(REG_IS);
        let raw_pi = read_reg_u32(REG_PI);
        let pi = effective_port_map(raw_pi);
        let vs = read_reg_u32(REG_VS);
        let cap2 = read_reg_u32(REG_CAP2);
        let bohc = read_reg_u32(REG_BOHC);
        let ahci_paddr = AHCI_PADDR.load(Ordering::Acquire);

        debug!(
            "AHCI HBA: base={ahci_paddr:#x}, cap={cap:#010x}, ghc={ghc:#010x}, is={is:#010x}, \
             pi={raw_pi:#010x}, effective_pi={pi:#010x}, vs={vs:#010x}, cap2={cap2:#010x}, \
             bohc={bohc:#010x}"
        );

        if cap == 0 && ghc == 0 && is == 0 && raw_pi == 0 && vs == 0 {
            warn!("AHCI HBA registers read as zero; check AHCI clock/reset and MMIO base");
            return None;
        }
        if cap == u32::MAX
            && ghc == u32::MAX
            && is == u32::MAX
            && raw_pi == u32::MAX
            && vs == u32::MAX
        {
            warn!("AHCI HBA registers read as all ones; check AHCI MMIO mapping");
            return None;
        }
        if pi == 0 {
            warn!("AHCI HBA reports no implemented ports");
            return None;
        }

        Some((cap, pi))
    }
}

impl AhciPort {
    const fn new(index: usize) -> Self {
        Self { index }
    }

    fn init_block(&self) -> Option<AhciBlock> {
        let port = self.index;
        let mut block = None;

        log_port(port, "before-link");
        if bring_up_port_link(port) {
            clear_port_errors(port, "link-up");
            let ptrs = clear_dma();
            if start_command_engine(port, &ptrs)
                && let Some(capacity_blocks) = identify_device(port, &ptrs)
            {
                block = Some(AhciBlock::new(*self, capacity_blocks));
            }
        }
        log_port(port, "after-link");
        block
    }

    fn issue_dma_command(
        &self,
        ptrs: &DmaPtrs,
        command: AtaDmaCommand<'_>,
    ) -> Result<(), AhciError> {
        setup_ata_dma_command(self.index, ptrs, command)?;
        write_port_reg_u32(self.index, PORT_CI, AHCI_CMD_SLOT0);

        if !wait_command_done(self.index) {
            return Err(AhciError::CommandFailed);
        }

        Ok(())
    }

    fn read_request(
        &self,
        ptrs: &DmaPtrs,
        lba: u64,
        sectors: u16,
        segments: &mut [Segment<'_>],
    ) -> Result<(), AhciError> {
        let total_len = sectors as usize * AHCI_SECTOR_SIZE;
        if request_segments_len(segments) != total_len {
            return Err(AhciError::InvalidBufferSize);
        }

        let mut sector_offset = 0usize;
        let mut byte_offset = 0usize;
        while sector_offset < sectors as usize {
            let chunk_sectors = (sectors as usize - sector_offset).min(AHCI_MAX_TRANSFER_SECTORS);
            let chunk_len = chunk_sectors * AHCI_SECTOR_SIZE;
            let chunk_lba = lba
                .checked_add(sector_offset as u64)
                .ok_or(AhciError::LbaOutOfRange)?;
            let dma_segments = [AhciDmaSegment::new(dma_paddr(ptrs.buffer), chunk_len)];

            self.issue_dma_command(
                ptrs,
                AtaDmaCommand {
                    command: ATA_CMD_READ_DMA_EXT,
                    segments: &dma_segments,
                    lba: chunk_lba,
                    sectors: chunk_sectors as u16,
                    device: ATA_DEVICE_LBA,
                    write: false,
                    label: "READ",
                },
            )?;

            let dma_buf =
                unsafe { core::slice::from_raw_parts(ptrs.buffer as *const u8, chunk_len) };
            copy_to_request_segments(segments, byte_offset, dma_buf);
            sector_offset += chunk_sectors;
            byte_offset += chunk_len;
        }

        Ok(())
    }

    fn write_request(
        &self,
        ptrs: &DmaPtrs,
        lba: u64,
        sectors: u16,
        segments: &[Segment<'_>],
    ) -> Result<(), AhciError> {
        let total_len = sectors as usize * AHCI_SECTOR_SIZE;
        if request_segments_len(segments) != total_len {
            return Err(AhciError::InvalidBufferSize);
        }

        let mut sector_offset = 0usize;
        let mut byte_offset = 0usize;
        while sector_offset < sectors as usize {
            let chunk_sectors = (sectors as usize - sector_offset).min(AHCI_MAX_TRANSFER_SECTORS);
            let chunk_len = chunk_sectors * AHCI_SECTOR_SIZE;
            let chunk_lba = lba
                .checked_add(sector_offset as u64)
                .ok_or(AhciError::LbaOutOfRange)?;
            let dma_buf = unsafe { core::slice::from_raw_parts_mut(ptrs.buffer, chunk_len) };
            copy_from_request_segments(segments, byte_offset, dma_buf);
            let dma_segments = [AhciDmaSegment::new(dma_paddr(ptrs.buffer), chunk_len)];

            self.issue_dma_command(
                ptrs,
                AtaDmaCommand {
                    command: ATA_CMD_WRITE_DMA_EXT,
                    segments: &dma_segments,
                    lba: chunk_lba,
                    sectors: chunk_sectors as u16,
                    device: ATA_DEVICE_LBA,
                    write: true,
                    label: "WRITE",
                },
            )?;

            sector_offset += chunk_sectors;
            byte_offset += chunk_len;
        }

        Ok(())
    }

    fn flush_cache(&self, ptrs: &DmaPtrs) -> Result<(), AhciError> {
        setup_ata_nodata_command(
            self.index,
            ptrs,
            AtaNoDataCommand {
                command: ATA_CMD_FLUSH_CACHE_EXT,
                label: "FLUSH",
            },
        )?;
        write_port_reg_u32(self.index, PORT_CI, AHCI_CMD_SLOT0);

        if !wait_command_done(self.index) {
            return Err(AhciError::CommandFailed);
        }

        trace!("AHCI port{} FLUSH done", self.index);
        Ok(())
    }

    fn enable_irq(&self) {
        write_port_reg_u32(self.index, PORT_IS, u32::MAX);
        write_reg_u32(REG_IS, 1u32 << self.index);
        write_port_reg_u32(self.index, PORT_IE, PORT_IRQ_ENABLE_MASK);

        let ghc = read_reg_u32(REG_GHC);
        write_reg_u32(REG_GHC, ghc | HOST_CTL_AHCI_EN | HOST_CTL_IRQ_EN);
        trace!(
            "AHCI port{} IRQ enabled: ghc={:#010x}, ie={:#010x}",
            self.index,
            read_reg_u32(REG_GHC),
            read_port_reg_u32(self.index, PORT_IE),
        );
    }

    fn disable_irq(&self) {
        write_port_reg_u32(self.index, PORT_IE, 0);
        write_port_reg_u32(self.index, PORT_IS, u32::MAX);
        write_reg_u32(REG_IS, 1u32 << self.index);
        trace!("AHCI port{} IRQ disabled", self.index);
    }

    fn irq_enabled(&self) -> bool {
        read_reg_u32(REG_GHC) & HOST_CTL_IRQ_EN != 0
            && read_port_reg_u32(self.index, PORT_IE) & PORT_IRQ_ENABLE_MASK != 0
    }

    fn handle_irq(&self) -> Event {
        let port_bit = 1u32 << self.index;
        let global_status = read_reg_u32(REG_IS);
        let port_status = read_port_reg_u32(self.index, PORT_IS);

        if port_status != 0 {
            write_port_reg_u32(self.index, PORT_IS, port_status);
        }
        if global_status & port_bit != 0 {
            write_reg_u32(REG_IS, port_bit);
        }

        let mut event = Event::none();
        if port_status & PORT_IRQ_ENABLE_MASK != 0 {
            event.push_queue(0);
        }
        event
    }
}

impl AhciBlock {
    const fn new(port: AhciPort, capacity_blocks: u64) -> Self {
        Self {
            port,
            capacity_blocks,
            queue_created: false,
            irq_enabled: AtomicBool::new(false),
            irq_handler_taken: false,
        }
    }

    fn make_device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some(DEVICE_NAME),
            ..DeviceInfo::new(self.capacity_blocks, AHCI_SECTOR_SIZE)
        }
    }
}

impl DriverGeneric for AhciBlock {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl Interface for AhciBlock {
    fn device_info(&self) -> DeviceInfo {
        self.make_device_info()
    }

    fn queue_limits(&self) -> QueueLimits {
        ahci_queue_limits()
    }

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.queue_created = true;
        Some(Box::new(AhciQueue {
            id: 0,
            port: self.port,
            capacity_blocks: self.capacity_blocks,
        }))
    }

    fn enable_irq(&self) {
        self.port.enable_irq();
        self.irq_enabled
            .store(self.port.irq_enabled(), Ordering::Release);
    }

    fn disable_irq(&self) {
        self.port.disable_irq();
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> IrqSourceList {
        vec![IrqSourceInfo::legacy(IdList::from_bits(1))]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        if source_id != 0 || self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        Some(Box::new(AhciIrqHandler { port: self.port }))
    }
}

impl IrqHandler for AhciIrqHandler {
    fn handle_irq(&mut self) -> Event {
        self.port.handle_irq()
    }
}

impl AhciQueue {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some(DEVICE_NAME),
            ..DeviceInfo::new(self.capacity_blocks, AHCI_SECTOR_SIZE)
        }
    }

    fn limits(&self) -> QueueLimits {
        ahci_queue_limits()
    }
}

// SAFETY: AHCI commands complete synchronously in `submit_request`; the queue
// does not retain request segment pointers after the call returns.
unsafe impl IQueue for AhciQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: self.device_info(),
            limits: self.limits(),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        validate_request(self.info(), &request)?;
        let ptrs = dma_ptrs();
        let preflush = request.flags.contains(RequestFlags::PREFLUSH);
        let fua = request.flags.contains(RequestFlags::FUA);

        if preflush {
            self.port
                .flush_cache(&ptrs)
                .map_err(|_| BlkError::Other("AHCI preflush failed"))?;
        }

        match request.op {
            RequestOp::Read => {
                self.port
                    .read_request(
                        &ptrs,
                        request.lba,
                        request.block_count as u16,
                        request.segments,
                    )
                    .map_err(|_| BlkError::Other("AHCI read failed"))?;
            }
            RequestOp::Write => {
                self.port
                    .write_request(
                        &ptrs,
                        request.lba,
                        request.block_count as u16,
                        request.segments,
                    )
                    .map_err(|_| BlkError::Other("AHCI write failed"))?;
            }
            RequestOp::Flush => {
                self.port
                    .flush_cache(&ptrs)
                    .map_err(|_| BlkError::Other("AHCI flush failed"))?;
            }
            RequestOp::Discard | RequestOp::WriteZeroes => {
                return Err(BlkError::NotSupported);
            }
        }

        if fua && !matches!(request.op, RequestOp::Flush) {
            self.port
                .flush_cache(&ptrs)
                .map_err(|_| BlkError::Other("AHCI FUA flush failed"))?;
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}

fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let fw_addr = reg.address as usize;
    let paddr = firmware_addr_to_phys(fw_addr);
    let size = reg.size.unwrap_or(DEFAULT_MMIO_SIZE as u64) as usize;
    let ports_implemented = ports_implemented(&info);
    let mmio = iomap_firmware_device(DEVICE_NAME, fw_addr, size)?;
    let vaddr = mmio.as_ptr() as usize;

    AHCI_MMIO_BASE.store(mmio.as_ptr() as usize, Ordering::Release);
    AHCI_PADDR.store(paddr, Ordering::Release);
    AHCI_PORTS_IMPLEMENTED.store(ports_implemented, Ordering::Release);

    debug!(
        "probing {DEVICE_NAME}: node={}, reg={fw_addr:#x}, paddr={paddr:#x}, vaddr={vaddr:#x}, \
         size={size:#x}, ports_implemented={ports_implemented:#010x}",
        info.node.name(),
    );

    let Some(block) = AhciController::probe() else {
        return Err(OnProbeError::NotMatch);
    };

    let capacity_blocks = block.capacity_blocks;
    let binding_info = ahci_binding_info(&info);
    let irq = binding_info.irq_num();
    plat_dev.register_block_with_info(block, binding_info);
    info!(
        "registered {DEVICE_NAME} block device: blocks={capacity_blocks}, \
         block_size={AHCI_SECTOR_SIZE}, irq={irq:?}",
    );
    Ok(())
}

fn ahci_binding_info(info: &FdtInfo<'_>) -> BindingInfo {
    binding_info_from_fdt(info).unwrap_or_else(|err| {
        warn!(
            "{DEVICE_NAME}: failed to resolve FDT IRQ for {}; continuing without IRQ: {err:?}",
            info.node.path(),
        );
        BindingInfo::empty()
    })
}

fn ports_implemented(info: &FdtInfo<'_>) -> u32 {
    info.node
        .as_node()
        .get_property("ports-implemented")
        .and_then(|prop| prop.get_u32_iter().next())
        .filter(|ports| *ports != 0)
        .unwrap_or(DEFAULT_PORTS_IMPLEMENTED)
}
