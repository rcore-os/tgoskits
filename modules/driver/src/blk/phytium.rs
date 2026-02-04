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

extern crate alloc;

use axklib::{mem::iomap, time::busy_wait};

use core::{
    cmp,
    marker::{Send, Sync},
    ptr::NonNull,
    time::Duration,
};

use log::{debug, info};
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};

use phytium_mci::sd::SdCard;
use phytium_mci::{IoPad, PAD_ADDRESS, mci_host::err::MCIHostError};
pub use phytium_mci::{Kernel, set_impl};

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use log::trace;

use rdif_block::{BlkError, IQueue, Interface, Request, RequestId};
use rdrive::{DriverGeneric, KError};

use spin::Mutex;

// pub use dma_api::{Direction, Impl as DmaImpl};
// pub use dma_api::set_impl as set_dma_impl;

const OFFSET: usize = 0x400_0000;
const BLOCK_SIZE: usize = 512;

pub struct KernelImpl;

impl Kernel for KernelImpl {
    fn sleep(us: Duration) {
        busy_wait(us);
    }
}

set_impl!(KernelImpl);

module_driver!(
    name: "Phytium SdCard",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["phytium,mci"],
            on_probe: probe_sdcard
        }
    ],
);

fn probe_sdcard(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    info!("Probing Phytium SDCard...");
    let mci_reg = info
        .node
        .reg()
        .and_then(|mut regs| regs.next())
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    info!(
        "MCI reg: addr={:#x}, size={:#x}",
        mci_reg.address as usize,
        mci_reg.size.unwrap_or(0)
    );

    let mci_reg_base = iomap(
        (mci_reg.address as usize).into(),
        mci_reg.size.unwrap_or(0x10000),
    )
    .expect("Failed to iomap mci reg");

    let iopad_reg_base =
        iomap((PAD_ADDRESS as usize).into(), 0x2000).expect("Failed to iomap iopad reg");

    info!("MCI reg base mapped at {:#x}", mci_reg_base.as_usize());

    let mci_reg =
        NonNull::new(mci_reg_base.as_usize() as *mut u8).expect("Failed to create NonNull pointer");

    let iopad_reg = NonNull::new(iopad_reg_base.as_usize() as *mut u8)
        .expect("Failed to create NonNull pointer for iopad");

    let iopad = IoPad::new(iopad_reg);

    info!("MCI reg mapped at {:p}", mci_reg);

    let sdcard = SdCardDriver::new(mci_reg, iopad);
    let dev = rdif_block::Block::new(sdcard);
    plat_dev.register(dev);

    debug!("phytium block device registered successfully");

    Ok(())
}

pub struct SdCardDriver {
    sd_card: Arc<Mutex<Box<SdCard>>>,
}

impl SdCardDriver {
    pub fn new(sd_addr: NonNull<u8>, iopad: IoPad) -> Self {
        let sd_card = Arc::new(Mutex::new(Box::new(SdCard::new(sd_addr, iopad))));
        SdCardDriver { sd_card }
    }
}

unsafe impl Send for SdCardDriver {}
unsafe impl Sync for SdCardDriver {}

unsafe impl Send for SdCardQueue {}
unsafe impl Sync for SdCardQueue {}

impl DriverGeneric for SdCardDriver {
    fn open(&mut self) -> Result<(), KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), KError> {
        Ok(())
    }
}

impl Interface for SdCardDriver {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        Some(Box::new(SdCardQueue {
            sd_card: Arc::clone(&self.sd_card),
        }))
    }

    fn enable_irq(&mut self) {
        todo!()
    }

    fn disable_irq(&mut self) {
        todo!()
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> rdif_block::Event {
        rdif_block::Event::none()
    }
}

pub struct SdCardQueue {
    sd_card: Arc<Mutex<Box<SdCard>>>,
}

impl IQueue for SdCardQueue {
    /// Returns the number of blocks on the SD card.
    fn num_blocks(&self) -> usize {
        self.sd_card.lock().block_count() as usize
    }

    /// Returns the block size in bytes.
    fn block_size(&self) -> usize {
        self.sd_card.lock().block_size() as usize
    }

    fn id(&self) -> usize {
        0
    }

    fn buff_config(&self) -> rdif_block::BuffConfig {
        rdif_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        let actual_block_id = request.block_id + OFFSET / 512;

        match request.kind {
            rdif_block::RequestKind::Read(mut buffer) => {
                trace!("read block {}", actual_block_id);

                Self::validate_buffer(&buffer)?;

                let (_, aligned_buf, _) = unsafe { buffer.align_to_mut::<u32>() };
                let mut temp_buf: Vec<u32> = Vec::with_capacity(aligned_buf.len());

                self.sd_card
                    .lock()
                    .read_blocks(&mut temp_buf, actual_block_id as u32, 1)
                    .map_err(|err| map_mci_error_to_blk_error(err))?;

                let copy_len = cmp::min(temp_buf.len(), aligned_buf.len());
                aligned_buf[..copy_len].copy_from_slice(&temp_buf[..copy_len]);

                Ok(RequestId::new(0))
            }
            rdif_block::RequestKind::Write(buffer) => {
                trace!("write block {}", actual_block_id);

                Self::validate_buffer(&buffer)?;

                let (_, aligned_buf, _) = unsafe { buffer.align_to::<u32>() };
                let mut write_buf: Vec<u32> = aligned_buf.to_vec();

                self.sd_card
                    .lock()
                    .write_blocks(&mut write_buf, actual_block_id as u32, 1)
                    .map_err(|err| map_mci_error_to_blk_error(err))?;

                Ok(RequestId::new(0))
            }
        }
    }

    fn poll_request(
        &mut self,
        _request: rdif_block::RequestId,
    ) -> Result<(), rdif_block::BlkError> {
        Ok(())
    }
}

impl SdCardQueue {
    fn validate_buffer(buffer: &[u8]) -> Result<(), BlkError> {
        if buffer.len() < BLOCK_SIZE {
            return Err(BlkError::Other(Box::new(BufferError::InvalidSize {
                expected: BLOCK_SIZE,
                actual: buffer.len(),
            })));
        }

        let (prefix, _, suffix) = unsafe { buffer.align_to::<u32>() };
        if !prefix.is_empty() || !suffix.is_empty() {
            return Err(BlkError::Other(Box::new(BufferError::InvalidAlignment)));
        }

        Ok(())
    }
}

#[derive(Debug)]
enum BufferError {
    InvalidSize { expected: usize, actual: usize },
    InvalidAlignment,
}

impl core::fmt::Display for BufferError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BufferError::InvalidSize { expected, actual } => {
                write!(
                    f,
                    "Invalid buffer size: expected at least {}, got {}",
                    expected, actual
                )
            }
            BufferError::InvalidAlignment => {
                write!(f, "Buffer is not properly aligned for u32 access")
            }
        }
    }
}

impl core::error::Error for BufferError {}

#[derive(Debug)]
struct MCIErrorWrapper(MCIHostError);

impl core::fmt::Display for MCIErrorWrapper {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "MCI Host Error: {:?}", self.0)
    }
}

impl core::error::Error for MCIErrorWrapper {}

// 错误映射函数
fn map_mci_error_to_blk_error(err: MCIHostError) -> BlkError {
    match err {
        MCIHostError::Timeout => BlkError::Retry,

        MCIHostError::OutOfRange | MCIHostError::InvalidArgument => {
            BlkError::Other(Box::new(MCIErrorWrapper(err)))
        }

        MCIHostError::CardDetectFailed | MCIHostError::CardInitFailed => BlkError::NotSupported,

        MCIHostError::InvalidVoltage
        | MCIHostError::SwitchVoltageFail
        | MCIHostError::SwitchVoltage18VFail33VSuccess => BlkError::NotSupported,

        MCIHostError::TransferFailed
        | MCIHostError::StopTransmissionFailed
        | MCIHostError::WaitWriteCompleteFailed => BlkError::Retry,

        // 其他所有错误包装为Other
        _ => BlkError::Other(Box::new(MCIErrorWrapper(err))),
    }
}
