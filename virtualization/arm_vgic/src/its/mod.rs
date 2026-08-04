//! Per-VM software Interrupt Translation Service.

mod command;

use alloc::{collections::BTreeMap, string::String, vec::Vec};

use crate::{
    CollectionId, EventId, GicVcpuId, ItsDeviceId, LpiId, VgicError, VgicResult,
    register::GITS_BASER_COUNT,
};

const COMMAND_SIZE: u64 = 32;
const MAX_QUEUE_SIZE: u64 = 0x10_0000;
const BASER_ENTRY_SIZE_SHIFT: u32 = 48;
const BASER_TYPE_SHIFT: u32 = 56;
const BASER_READ_ONLY_MASK: u64 = (0x1f << BASER_ENTRY_SIZE_SHIFT) | (0x7 << BASER_TYPE_SHIFT);
const DEVICE_BASER_DESCRIPTOR: u64 = (7 << BASER_ENTRY_SIZE_SHIFT) | (1 << BASER_TYPE_SHIFT);
const COLLECTION_BASER_DESCRIPTOR: u64 = (7 << BASER_ENTRY_SIZE_SHIFT) | (4 << BASER_TYPE_SHIFT);

/// Guest-memory capability failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("guest-memory operation {operation} failed: {detail}")]
pub struct GuestMemoryError {
    operation: &'static str,
    detail: String,
}

impl GuestMemoryError {
    /// Creates an adapter-owned guest-memory failure.
    pub fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

/// Checked access to memory owned by one guest VM.
pub trait GuestMemory: Send + Sync {
    /// Reads exactly `destination.len()` bytes from a guest physical address.
    fn read(&self, address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError>;
}

#[derive(Clone, Copy)]
struct ItsDevice {
    event_bits: u8,
}

#[derive(Clone, Copy)]
struct Translation {
    lpi: LpiId,
    collection: CollectionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ItsAction {
    SetPending {
        target: GicVcpuId,
        lpi: LpiId,
        pending: bool,
    },
}

pub(crate) struct ItsState {
    enabled: bool,
    cbaser: u64,
    creadr: u64,
    cwriter: u64,
    basers: [u64; GITS_BASER_COUNT],
    devices: BTreeMap<ItsDeviceId, ItsDevice>,
    collections: BTreeMap<CollectionId, GicVcpuId>,
    translations: BTreeMap<(ItsDeviceId, EventId), Translation>,
}

impl ItsState {
    pub(crate) const fn new() -> Self {
        let mut basers = [0; GITS_BASER_COUNT];
        basers[0] = DEVICE_BASER_DESCRIPTOR;
        basers[1] = COLLECTION_BASER_DESCRIPTOR;
        Self {
            enabled: false,
            cbaser: 0,
            creadr: 0,
            cwriter: 0,
            basers,
            devices: BTreeMap::new(),
            collections: BTreeMap::new(),
            translations: BTreeMap::new(),
        }
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(crate) const fn cbaser(&self) -> u64 {
        self.cbaser
    }

    pub(crate) fn set_cbaser(&mut self, value: u64) -> VgicResult {
        let size = queue_size(value)?;
        let base = queue_base(value);
        if base == 0 || !base.is_multiple_of(0x1000) || size > MAX_QUEUE_SIZE {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "ITS command queue base {base:#x} or size {size:#x} is invalid"
                ),
            });
        }
        self.cbaser = value;
        self.creadr = 0;
        self.cwriter = 0;
        Ok(())
    }

    pub(crate) const fn creadr(&self) -> u64 {
        self.creadr
    }

    pub(crate) const fn cwriter(&self) -> u64 {
        self.cwriter
    }

    pub(crate) fn baser(&self, index: usize) -> u64 {
        self.basers[index]
    }

    pub(crate) fn set_baser(&mut self, index: usize, value: u64) {
        let descriptor = self.basers[index] & BASER_READ_ONLY_MASK;
        self.basers[index] = (value & !BASER_READ_ONLY_MASK) | descriptor;
    }

    pub(crate) fn set_cwriter(&mut self, writer: u64) -> VgicResult {
        if self.cbaser == 0 {
            if writer == 0 {
                self.cwriter = 0;
                return Ok(());
            }
            return Err(VgicError::InvalidItsCommand {
                opcode: 0,
                offset: writer,
                detail: "CWRITER was updated before CBASER was configured".into(),
            });
        }

        let size = queue_size(self.cbaser)?;
        let writer = writer & (MAX_QUEUE_SIZE - 1);
        if writer >= size || !writer.is_multiple_of(COMMAND_SIZE) {
            return Err(VgicError::InvalidItsCommand {
                opcode: 0,
                offset: writer,
                detail: alloc::format!(
                    "writer must be a 32-byte-aligned offset below queue size {size:#x}"
                ),
            });
        }
        self.cwriter = writer;
        Ok(())
    }

    pub(crate) const fn has_pending_commands(&self) -> bool {
        self.creadr != self.cwriter
    }

    pub(crate) fn process_commands(
        &mut self,
        memory: &dyn GuestMemory,
        budget: usize,
        lpi_limit: u32,
        processor_targets: &[GicVcpuId],
    ) -> VgicResult<Vec<ItsAction>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        if self.cbaser == 0 {
            return Err(VgicError::InvalidItsCommand {
                opcode: 0,
                offset: self.cwriter,
                detail: "ITS was enabled before CBASER was configured".into(),
            });
        }
        let size = queue_size(self.cbaser)?;
        let command_count = queue_distance(self.creadr, self.cwriter, size) / COMMAND_SIZE;
        if command_count as usize > budget {
            return Err(VgicError::ItsCommandBudgetExceeded {
                budget,
                offset: self.creadr,
            });
        }

        let mut actions = Vec::new();
        let base = queue_base(self.cbaser);
        while self.creadr != self.cwriter {
            let offset = self.creadr;
            let mut bytes = [0u8; COMMAND_SIZE as usize];
            memory
                .read(base + offset, &mut bytes)
                .map_err(|error| VgicError::GuestMemory {
                    operation: "read command",
                    address: base + offset,
                    length: bytes.len(),
                    detail: alloc::format!("{error}"),
                })?;
            self.execute(
                decode_words(&bytes),
                offset,
                lpi_limit,
                processor_targets,
                &mut actions,
            )?;
            self.creadr = (self.creadr + COMMAND_SIZE) % size;
        }
        Ok(actions)
    }

    pub(crate) fn translate(
        &self,
        device: ItsDeviceId,
        event: EventId,
    ) -> VgicResult<(LpiId, GicVcpuId)> {
        let translation =
            self.translations
                .get(&(device, event))
                .ok_or_else(|| VgicError::ResourceNotFound {
                    resource: alloc::format!("ITS translation ({}, {})", device.raw(), event.raw()),
                    operation: "signal MSI",
                })?;
        let target = self
            .collections
            .get(&translation.collection)
            .copied()
            .ok_or_else(|| VgicError::ResourceNotFound {
                resource: alloc::format!("ITS collection {}", translation.collection.raw()),
                operation: "signal MSI",
            })?;
        Ok((translation.lpi, target))
    }
}

fn queue_distance(reader: u64, writer: u64, size: u64) -> u64 {
    if writer >= reader {
        writer - reader
    } else {
        size - reader + writer
    }
}

fn decode_words(bytes: &[u8; COMMAND_SIZE as usize]) -> [u64; 4] {
    let mut words = [0; 4];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    words
}

fn queue_base(cbaser: u64) -> u64 {
    cbaser & 0x000f_ffff_ffff_f000
}

fn queue_size(cbaser: u64) -> VgicResult<u64> {
    let pages = (cbaser & 0xff) + 1;
    pages
        .checked_mul(0x1000)
        .ok_or_else(|| VgicError::InvalidConfig {
            detail: "ITS command queue size overflows".into(),
        })
}
