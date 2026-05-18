use alloc::vec::Vec;

use dma_api::{DArray, DBox, DmaDirection};
use xhci::context::{Device32Byte, Device64Byte, Input32Byte, Input64Byte, InputHandler};

use super::SlotId;
use crate::{err::*, osal::Kernel};

pub struct DeviceContextList {
    pub dcbaa: DArray<u64>,
    max_slots: usize,
}

unsafe impl Send for DeviceContextList {}
unsafe impl Sync for DeviceContextList {}

pub(crate) struct Context32 {
    out: DBox<Device32Byte>,
    input: DBox<Input32Byte>,
}

pub(crate) struct Context64 {
    out: DBox<Device64Byte>,
    input: DBox<Input64Byte>,
}
pub(crate) enum ContextData {
    Context32(Context32),
    Context64(Context64),
}

impl ContextData {
    pub fn new(is_64: bool, dma: &Kernel) -> core::result::Result<Self, HostError> {
        if is_64 {
            Ok(ContextData::Context64(Context64 {
                // out: DBox::zero_with_align(dma_mask as _, dma_api::Direction::FromDevice, 64)?,
                // input: DBox::zero_with_align(dma_mask as _, dma_api::Direction::ToDevice, 64)?,
                out: dma.box_zero_with_align(64, DmaDirection::FromDevice)?,
                input: dma.box_zero_with_align(64, DmaDirection::ToDevice)?,
            }))
        } else {
            Ok(ContextData::Context32(Context32 {
                // out: DBox::zero_with_align(dma_mask as _, dma_api::Direction::FromDevice, 64)?,
                // input: DBox::zero_with_align(dma_mask as _, dma_api::Direction::ToDevice, 64)?,
                out: dma.box_zero_with_align(64, DmaDirection::FromDevice)?,
                input: dma.box_zero_with_align(64, DmaDirection::ToDevice)?,
            }))
        }
    }

    pub fn with_empty_input<F>(&mut self, f: F)
    where
        F: FnOnce(&mut dyn InputHandler),
    {
        match self {
            ContextData::Context32(ctx) => {
                let mut input = Input32Byte::new_32byte();
                f(&mut input);
                ctx.input.write(input);
            }
            ContextData::Context64(ctx) => {
                let mut input = Input64Byte::new_64byte();
                f(&mut input);
                ctx.input.write(input);
            }
        }
    }

    pub fn with_input<F>(&mut self, f: F)
    where
        F: FnOnce(&mut dyn InputHandler),
    {
        match self {
            ContextData::Context32(ctx) => {
                let mut input = ctx.input.read();
                f(&mut input);
                ctx.input.write(input);
            }
            ContextData::Context64(ctx) => {
                let mut input = ctx.input.read();
                f(&mut input);
                ctx.input.write(input);
            }
        }
    }

    pub fn dcbaa(&self) -> u64 {
        match self {
            ContextData::Context32(ctx) => ctx.out.dma_addr(),
            ContextData::Context64(ctx) => ctx.out.dma_addr(),
        }
        .as_u64()
    }

    pub fn input_bus_addr(&self) -> u64 {
        match self {
            ContextData::Context32(ctx) => ctx.input.dma_addr(),
            ContextData::Context64(ctx) => ctx.input.dma_addr(),
        }
        .as_u64()
    }

    pub fn perper_change(&mut self) {
        self.with_input(|input| {
            let control_context = input.control_mut();
            for i in 0..32 {
                control_context.clear_add_context_flag(i);
                if i > 1 {
                    control_context.clear_drop_context_flag(i);
                }
            }
            control_context.set_add_context_flag(0);
        });
    }
}

impl DeviceContextList {
    pub fn new(max_slots: usize, dma: &Kernel) -> Result<Self> {
        // let dcbaa = DVec::zeros(dma_mask as _, 256, 0x1000, dma_api::Direction::ToDevice)
        //     .map_err(|_| USBError::NoMemory)?;
        let dcbaa = dma
            .array_zero_with_align(256, dma.page_size(), DmaDirection::ToDevice)
            .map_err(|_| USBError::NoMemory)?;
        Ok(Self { dcbaa, max_slots })
    }

    pub fn new_ctx(&mut self, slot_id: SlotId, is_64: bool, dma: &Kernel) -> Result<ContextData> {
        if slot_id.as_usize() > self.max_slots {
            Err(USBError::SlotLimitReached)?;
        }
        let ctx = ContextData::new(is_64, dma)?;
        self.dcbaa.set(slot_id.as_usize(), ctx.dcbaa());
        Ok(ctx)
    }
}

pub struct ScratchpadBufferArray {
    pub entries: DArray<u64>,
    pub _pages: Vec<DArray<u8>>,
}

impl ScratchpadBufferArray {
    pub fn new(entries: usize, dma: &Kernel) -> Result<Self> {
        // let mut entries_vec = DVec::zeros(
        //     dma_mask as _,
        //     entries,
        //     64,
        //     dma_api::Direction::Bidirectional,
        // )
        // .map_err(|_| USBError::NoMemory)?;

        let mut entries_vec = dma
            .array_zero_with_align(entries, 64, DmaDirection::Bidirectional)
            .map_err(|_| USBError::NoMemory)?;

        // let pages: Vec<DVec<u8>> = (0..entries_vec.len())
        //     .map(|_| {
        //         DVec::<u8>::zeros(
        //             dma_mask as _,
        //             0x1000,
        //             0x1000,
        //             dma_api::Direction::Bidirectional,
        //         )
        //         .map_err(|_| USBError::NoMemory)
        //     })
        //     .try_collect()?;
        let mut pages: Vec<DArray<u8>> = Vec::with_capacity(entries_vec.len());
        for _ in 0..entries_vec.len() {
            let page = dma
                .array_zero_with_align(
                    dma.page_size(),
                    dma.page_size(),
                    DmaDirection::Bidirectional,
                )
                .map_err(|_| USBError::NoMemory)?;
            pages.push(page);
        }

        // 将每个页面的地址写入到 entries 数组中
        for (i, page) in pages.iter().enumerate() {
            entries_vec.set(i, page.dma_addr().as_u64());
        }

        Ok(Self {
            entries: entries_vec,
            _pages: pages,
        })
    }

    pub fn bus_addr(&self) -> u64 {
        self.entries.dma_addr().as_u64()
    }
}
