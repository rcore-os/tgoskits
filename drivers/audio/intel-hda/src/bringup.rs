//! HDA controller setup, CORB/RIRB codec transport, and bounded PCM playback.
use alloc::collections::VecDeque;
use core::{
    mem::ManuallyDrop,
    sync::atomic::{Ordering, fence},
};

use dma_api::CoherentArray;

use crate::{
    DeviceDma, Error, HdaDevice, HdaIo, RegisterWidth, Result,
    codec::{self, Route, Verbs},
    desc::{BufferDescriptor, FORMAT, PERIOD, PERIODS, verb},
    regs::{self},
};

const RING_ENTRIES: usize = 256;
const DMA_ALIGN: usize = 4096;
const FIRST_STREAM_DESCRIPTOR: usize = 0x80;
const STREAM_DESCRIPTOR_SIZE: usize = 0x20;

struct DmaBuffers {
    corb: CoherentArray<u32>,
    rirb: CoherentArray<u64>,
    bdl: CoherentArray<BufferDescriptor>,
    audio: CoherentArray<u8>,
}

impl DmaBuffers {
    fn new(dma: &DeviceDma, gcap: u32) -> Result<Self> {
        let mut constraints = dma.info().constraints();
        constraints.align = constraints.align.max(DMA_ALIGN);
        if gcap & 1 == 0 {
            constraints.addr_mask &= u64::from(u32::MAX);
        }
        if constraints.addr_mask == 0 || !constraints.align.is_power_of_two() {
            return Err(Error::DmaAddress);
        }
        let dma = dma.with_constraints(constraints);
        let corb = dma.coherent_array_zero_with_align::<u32>(RING_ENTRIES, DMA_ALIGN)?;
        let rirb = dma.coherent_array_zero_with_align::<u64>(RING_ENTRIES, DMA_ALIGN)?;
        let bdl = dma.coherent_array_zero_with_align::<BufferDescriptor>(PERIODS, DMA_ALIGN)?;
        let audio = dma.coherent_array_zero_with_align::<u8>(PERIOD * PERIODS, DMA_ALIGN)?;
        for (address, size) in [
            (corb.dma_addr().as_u64(), corb.bytes_len()),
            (rirb.dma_addr().as_u64(), rirb.bytes_len()),
            (bdl.dma_addr().as_u64(), bdl.bytes_len()),
            (audio.dma_addr().as_u64(), audio.bytes_len()),
        ] {
            let end = address
                .checked_add(u64::try_from(size.saturating_sub(1)).map_err(|_| Error::DmaAddress)?)
                .ok_or(Error::DmaAddress)?;
            if address & 0x7f != 0 || end > constraints.addr_mask {
                return Err(Error::DmaAddress);
            }
        }
        Ok(Self {
            corb,
            rirb,
            bdl,
            audio,
        })
    }

    fn corb_address(&self) -> u64 {
        self.corb.dma_addr().as_u64()
    }

    fn rirb_address(&self) -> u64 {
        self.rirb.dma_addr().as_u64()
    }

    fn bdl_address(&self) -> u64 {
        self.bdl.dma_addr().as_u64()
    }

    fn audio_address(&self) -> u64 {
        self.audio.dma_addr().as_u64()
    }
}

/// Returns the first output stream after the input stream descriptors.
pub(crate) fn first_playback_stream_offset(gcap: u32) -> Result<usize> {
    let output_streams = (gcap >> 12) & 0x0f;
    if output_streams == 0 {
        return Err(Error::Unsupported);
    }
    let input_streams = ((gcap >> 8) & 0x0f) as usize;
    FIRST_STREAM_DESCRIPTOR
        .checked_add(input_streams * STREAM_DESCRIPTOR_SIZE)
        .ok_or(Error::RegisterRange)
}

pub struct Controller<B: HdaIo> {
    bus: B,
    dma: ManuallyDrop<DmaBuffers>,
    dma_published: bool,
    dma_released: bool,
    wp: u16,
    rp: u16,
    stream: usize,
    route: Option<Route>,
    present: u16,
    pending: VecDeque<(u16, usize)>,
    retired: VecDeque<u16>,
    tail: usize,
    position: usize,
    running: bool,
    prepared: bool,
    live: bool,
    sequence: u16,
    last_poll: u64,
}

impl<B: HdaIo> Controller<B> {
    /// Resets an admitted Intel HDA controller, allocates DMA rings, discovers
    /// codecs, and configures one supported analog playback route.
    ///
    /// This performs device I/O. The caller must first validate the PCI
    /// function using identify, own the BAR mapping for the supplied HdaIo,
    /// provide exclusive controller access, and pass a DMA capability for this
    /// device. The DMA capability must support coherent CPU/device memory.
    pub fn new(mut bus: B, device: HdaDevice, dma: DeviceDma) -> Result<Self> {
        if device.vendor_id() != 0x8086 || bus.mapped_len() < 2 {
            return Err(Error::Unsupported);
        }
        let gcap = bus.read(regs::GCAP, RegisterWidth::Word);
        let stream = first_playback_stream_offset(gcap)?;
        let stream_end = stream
            .checked_add(STREAM_DESCRIPTOR_SIZE)
            .ok_or(Error::RegisterRange)?;
        if bus.mapped_len() < stream_end {
            return Err(Error::RegisterRange);
        }
        let dma = DmaBuffers::new(&dma, gcap)?;
        let mut controller = Self {
            bus,
            dma: ManuallyDrop::new(dma),
            dma_published: false,
            dma_released: false,
            wp: 0,
            rp: 0,
            stream,
            route: None,
            present: 0,
            pending: VecDeque::new(),
            retired: VecDeque::new(),
            tail: 0,
            position: 0,
            running: false,
            prepared: false,
            live: true,
            sequence: 0,
            last_poll: 0,
        };
        controller.initialize()?;
        Ok(controller)
    }

    fn initialize(&mut self) -> Result {
        self.bus.write(regs::GCTL, RegisterWidth::Dword, 0);
        wait(&mut self.bus, regs::GCTL, RegisterWidth::Dword, 1, 0)?;
        self.bus.delay_us(100);
        self.bus.write(regs::GCTL, RegisterWidth::Dword, 1);
        wait(&mut self.bus, regs::GCTL, RegisterWidth::Dword, 1, 1)?;
        self.bus.delay_us(521);

        self.bus.write(regs::CORBCTL, RegisterWidth::Byte, 0);
        self.bus.write(regs::RIRBCTL, RegisterWidth::Byte, 0);
        wait(&mut self.bus, regs::CORBCTL, RegisterWidth::Byte, 2, 0)?;
        wait(&mut self.bus, regs::RIRBCTL, RegisterWidth::Byte, 2, 0)?;
        if self.bus.read(regs::CORBSIZE, RegisterWidth::Byte) & 64 == 0
            || self.bus.read(regs::RIRBSIZE, RegisterWidth::Byte) & 64 == 0
        {
            return Err(Error::Unsupported);
        }

        self.bus.write(regs::CORBSIZE, RegisterWidth::Byte, 2);
        self.bus.write(regs::RIRBSIZE, RegisterWidth::Byte, 2);
        self.dma_published = true;
        self.address(regs::CORB, self.dma.corb_address());
        self.address(regs::RIRB, self.dma.rirb_address());
        self.bus.write(regs::CORBWP, RegisterWidth::Word, 0);
        self.bus.write(regs::CORBRP, RegisterWidth::Word, 0x8000);
        let reset = self.bus.read(regs::CORBRP, RegisterWidth::Word);
        if reset & 0x8000 != 0 {
            wait(
                &mut self.bus,
                regs::CORBRP,
                RegisterWidth::Word,
                0x8000,
                0x8000,
            )?;
            self.bus.write(regs::CORBRP, RegisterWidth::Word, 0);
        }
        wait(&mut self.bus, regs::CORBRP, RegisterWidth::Word, 0xffff, 0)?;
        self.bus.write(regs::RIRBWP, RegisterWidth::Word, 0x8000);
        self.bus.write(regs::RINTCNT, RegisterWidth::Word, 1);
        self.bus.write(regs::RIRBSTS, RegisterWidth::Byte, 5);
        // Keep global interrupt delivery off; the owner may register IRQ policy
        // in its runtime adapter after this synchronous core is initialized.
        self.bus.write(0x20, RegisterWidth::Dword, 0);
        enable_command_rings(&mut self.bus)?;

        self.present = self.bus.read(regs::STATESTS, RegisterWidth::Word) as u16;
        let route = codec::enumerate(self, self.present)?;
        let converter = route.path.last().ok_or(Error::Unsupported)?;
        if converter.caps & 1 == 0 {
            return Err(Error::Unsupported);
        }
        let format_node = if converter.caps & 16 != 0 {
            converter.node
        } else {
            route.function
        };
        let pcm = self.verb(route.codec, format_node, 0xf00, 0xa)?;
        if pcm & (1 << 6) == 0
            || pcm & (1 << 17) == 0
            || self.verb(route.codec, format_node, 0xf00, 0xb)? & 1 == 0
        {
            return Err(Error::Unsupported);
        }
        codec::configure(self, &route)?;
        self.route = Some(route);
        Ok(())
    }

    fn address(&mut self, offset: usize, address: u64) {
        self.bus.write(offset, RegisterWidth::Dword, address as u32);
        self.bus
            .write(offset + 4, RegisterWidth::Dword, (address >> 32) as u32);
    }

    /// The selected analog codec path.
    pub fn route(&self) -> &Route {
        self.route
            .as_ref()
            .expect("route exists after successful controller construction")
    }

    /// Programs the fixed 16-bit, stereo, 48-kHz playback stream layout.
    /// Returns `ResourceBusy` while submitted periods or completion tokens remain;
    /// use `abort` to cancel them explicitly.
    pub fn prepare(&mut self, period_bytes: u32, periods: u32) -> Result {
        if !self.live {
            return Err(Error::BadState);
        }
        if period_bytes as usize != PERIOD || periods as usize != PERIODS {
            return Err(Error::InvalidParam);
        }
        if !self.pending.is_empty() || !self.retired.is_empty() {
            return Err(Error::ResourceBusy);
        }
        self.stop_stream()?;
        self.bus.write(self.stream, RegisterWidth::Byte, 1);
        self.wait_or_poison(self.stream, RegisterWidth::Byte, 1, 1)?;
        self.bus.write(self.stream, RegisterWidth::Byte, 0);
        self.wait_or_poison(self.stream, RegisterWidth::Byte, 1, 0)?;
        self.tail = 0;
        self.position = 0;
        self.retired.clear();
        self.dma.audio.write_with_cpu(PERIOD * PERIODS, |audio| {
            audio.fill(0);
        });
        let audio_address = self.dma.audio_address();
        self.dma.bdl.write_with_cpu(PERIODS, |bdl| {
            for (index, descriptor) in bdl.iter_mut().enumerate() {
                *descriptor = BufferDescriptor {
                    address: audio_address + (index * PERIOD) as u64,
                    length: PERIOD as u32,
                    flags: 1,
                };
            }
        });
        self.address(self.stream + 0x18, self.dma.bdl_address());
        self.bus.write(
            self.stream + 8,
            RegisterWidth::Dword,
            (PERIOD * PERIODS) as u32,
        );
        self.bus
            .write(self.stream + 0xc, RegisterWidth::Word, (PERIODS - 1) as u32);
        self.bus
            .write(self.stream + 0x12, RegisterWidth::Word, u32::from(FORMAT));
        self.bus.write(self.stream + 2, RegisterWidth::Byte, 0x10);
        self.bus.write(self.stream + 3, RegisterWidth::Byte, 0x1c);
        self.prepared = true;
        self.dma_published = true;
        Ok(())
    }

    /// Submits one fixed-size PCM period and returns its completion token.
    ///
    /// `Again` means no safe slot is currently available, or a completion token
    /// must be collected with `complete` before another period can be accepted.
    pub fn submit(&mut self, bytes: &[u8]) -> Result<u16> {
        if !self.live {
            return Err(Error::BadState);
        }
        if !self.prepared || bytes.len() != PERIOD {
            return Err(Error::InvalidParam);
        }
        if self.running {
            self.poll_stream_position()?;
            if !self.retired.is_empty() {
                return Err(Error::Again);
            }
        }
        if self.pending.len() == PERIODS {
            return Err(Error::Again);
        }
        if self.pending.is_empty() && self.running {
            self.prepare(PERIOD as u32, PERIODS as u32)?;
        }
        let slot = self.tail;
        if self.running
            && (self.bus.read(self.stream + 4, RegisterWidth::Dword) as usize / PERIOD) % PERIODS
                == slot
        {
            return Err(Error::Again);
        }
        self.sequence = self.sequence.wrapping_add(1);
        let token = self.sequence;
        let start = slot * PERIOD;
        self.dma.audio.write_with_cpu(PERIOD * PERIODS, |audio| {
            audio[start..start + PERIOD].copy_from_slice(bytes);
        });
        fence(Ordering::SeqCst);
        self.pending.push_back((token, slot));
        self.tail = (slot + 1) % PERIODS;
        if !self.running {
            self.bus.write(self.stream, RegisterWidth::Byte, 2);
            self.running = true;
            self.last_poll = self.bus.now_ns();
        }
        Ok(token)
    }

    /// Reclaims the next completed period token, if any.
    pub fn complete(&mut self) -> Result<Option<u16>> {
        if !self.live {
            return Err(Error::BadState);
        }
        self.poll_stream_position()?;
        Ok(self.retired.pop_front())
    }

    fn poll_stream_position(&mut self) -> Result {
        if self.running {
            let now = self.bus.now_ns();
            if now.saturating_sub(self.last_poll) >= 80_000_000 {
                self.live = false;
                self.stop_stream()?;
                return Err(Error::Io);
            }
            self.last_poll = now;
            if self.bus.read(self.stream + 3, RegisterWidth::Byte) & 0x18 != 0 {
                self.live = false;
                self.stop_stream()?;
                return Err(Error::Io);
            }
            let position =
                (self.bus.read(self.stream + 4, RegisterWidth::Dword) as usize / PERIOD) % PERIODS;
            while self.position != position {
                if let Some((token, slot)) = self.pending.front().copied() {
                    if slot != self.position {
                        self.live = false;
                        self.stop_stream()?;
                        return Err(Error::BadState);
                    }
                    self.pending.pop_front();
                    self.retired.push_back(token);
                    let start = self.position * PERIOD;
                    self.dma.audio.write_with_cpu(PERIOD * PERIODS, |audio| {
                        audio[start..start + PERIOD].fill(0);
                    });
                }
                self.position = (self.position + 1) % PERIODS;
            }
        }
        Ok(())
    }

    fn stop_stream(&mut self) -> Result {
        self.bus.write(self.stream, RegisterWidth::Byte, 0);
        self.wait_or_poison(self.stream, RegisterWidth::Byte, 2, 0)?;
        self.running = false;
        Ok(())
    }

    fn wait_or_poison(
        &mut self,
        offset: usize,
        width: RegisterWidth,
        mask: u32,
        value: u32,
    ) -> Result {
        match wait(&mut self.bus, offset, width, mask, value) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.live = false;
                Err(error)
            }
        }
    }

    /// Cancels playback only after RUN readback proves the stream stopped.
    pub fn abort(&mut self) -> Result {
        if !self.live {
            return Err(Error::BadState);
        }
        if let Err(error) = self.stop_stream() {
            self.live = false;
            return Err(error);
        }
        self.pending.clear();
        self.retired.clear();
        self.dma.audio.write_with_cpu(PERIOD * PERIODS, |audio| {
            audio.fill(0);
        });
        self.prepared = false;
        Ok(())
    }

    /// Stops after the queued periods have been retired and the output FIFO drained.
    pub fn release(&mut self) -> Result {
        if !self.live {
            return Err(Error::BadState);
        }
        if !self.pending.is_empty() {
            return Err(Error::ResourceBusy);
        }
        if self.running {
            let fifo = self
                .bus
                .read(self.stream + 0x10, RegisterWidth::Word)
                .saturating_add(1);
            let bytes = fifo.max((PERIOD * 2) as u32);
            let micros = (u64::from(bytes) * 1_000_000).div_ceil(192_000) as u32;
            self.bus.delay_us(micros);
        }
        self.stop_stream()?;
        self.prepared = false;
        Ok(())
    }

    /// Resets the controller before releasing any memory that it could DMA to.
    ///
    /// If reset readback fails, the DMA allocations remain retained until the
    /// object is dropped, at which point the core intentionally leaks them
    /// rather than return memory that the device may still access.
    pub fn shutdown(&mut self) -> Result {
        if self.dma_released {
            return Ok(());
        }
        if self.dma_published && !self.reset_hardware() {
            self.live = false;
            return Err(Error::Io);
        }
        self.dma_published = false;
        self.live = false;
        self.release_dma();
        Ok(())
    }

    fn reset_hardware(&mut self) -> bool {
        let stream_stopped = self.stop_stream().is_ok();
        self.bus.write(regs::CORBCTL, RegisterWidth::Byte, 0);
        self.bus.write(regs::RIRBCTL, RegisterWidth::Byte, 0);
        self.bus.write(regs::GCTL, RegisterWidth::Dword, 0);
        let controller_reset = wait(&mut self.bus, regs::GCTL, RegisterWidth::Dword, 1, 0).is_ok();
        stream_stopped && controller_reset
    }

    fn release_dma(&mut self) {
        if !self.dma_released {
            // SAFETY: The allocation set is manually dropped exactly once, and
            // only after it was never published or the device reset was read back.
            unsafe { ManuallyDrop::drop(&mut self.dma) };
            self.dma_released = true;
        }
    }
}

impl<B: HdaIo> Verbs for Controller<B> {
    fn verb(&mut self, codec: u8, node: u8, operation: u16, payload: u16) -> Result<u32> {
        if codec >= 15 || node >= 128 || operation > 0xfff {
            return Err(Error::InvalidParam);
        }
        if !self.live {
            return Err(Error::BadState);
        }
        self.wp = (self.wp + 1) & 255;
        self.dma
            .corb
            .set_cpu(usize::from(self.wp), verb(codec, node, operation, payload));
        fence(Ordering::SeqCst);
        self.bus
            .write(regs::CORBWP, RegisterWidth::Word, u32::from(self.wp));
        for _ in 0..25_000 {
            let end = self.bus.read(regs::RIRBWP, RegisterWidth::Word) as u16 & 255;
            while self.rp != end {
                self.rp = (self.rp + 1) & 255;
                fence(Ordering::SeqCst);
                let response = self
                    .dma
                    .rirb
                    .read_cpu(usize::from(self.rp))
                    .ok_or(Error::BadState)?;
                let extra = (response >> 32) as u32;
                if extra & 16 == 0 && extra & 15 == u32::from(codec) {
                    self.bus.write(regs::RIRBSTS, RegisterWidth::Byte, 1);
                    return Ok(response as u32);
                }
            }
            if self.bus.read(regs::RIRBSTS, RegisterWidth::Byte) & 4 != 0 {
                self.live = false;
                return Err(Error::Io);
            }
            self.bus.delay_us(10);
        }
        self.live = false;
        Err(Error::Io)
    }
}

fn wait(
    bus: &mut impl HdaIo,
    offset: usize,
    width: RegisterWidth,
    mask: u32,
    value: u32,
) -> Result {
    for _ in 0..10_000 {
        if bus.read(offset, width) & mask == value {
            return Ok(());
        }
        bus.delay_us(10);
    }
    Err(Error::Io)
}

fn enable_command_rings(bus: &mut impl HdaIo) -> Result {
    // The response ring must be ready before CORB starts issuing commands.
    bus.write(regs::RIRBCTL, RegisterWidth::Byte, 3);
    // Verify the DMA engines accepted their run bits before the first verb.
    wait(bus, regs::RIRBCTL, RegisterWidth::Byte, 3, 3)?;
    bus.write(regs::CORBCTL, RegisterWidth::Byte, 2);
    wait(bus, regs::CORBCTL, RegisterWidth::Byte, 2, 2)
}

impl<B: HdaIo> Drop for Controller<B> {
    fn drop(&mut self) {
        if self.dma_released {
            return;
        }
        if !self.dma_published || self.reset_hardware() {
            self.release_dma();
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::VecDeque;
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
    use std::collections::BTreeMap;

    use dma_api::{
        DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
        DmaDomainId, DmaError, DmaMapHandle, DmaOp,
    };

    use super::*;

    struct FakeDma;

    fn allocate(constraints: DmaConstraints, layout: Layout) -> Option<DmaAllocHandle> {
        // SAFETY: layout was validated by the DMA API and is non-zero-sized.
        let ptr = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })?;
        let address = ptr.as_ptr() as u64;
        let last = address.checked_add(u64::try_from(layout.size().checked_sub(1)?).ok()?)?;
        let within_mask = address & !constraints.addr_mask == 0 && last <= constraints.addr_mask;
        let aligned = address.is_multiple_of(constraints.align.max(1) as u64);
        let within_boundary = constraints.boundary.is_none_or(|boundary| {
            boundary != 0 && address / boundary as u64 == last / boundary as u64
        });
        let within_segment = constraints
            .max_segment_size
            .is_none_or(|max_size| layout.size() <= max_size);
        if !within_mask || !aligned || !within_boundary || !within_segment {
            // SAFETY: ptr was allocated above with this exact layout and has not
            // been published to any device.
            unsafe { std::alloc::dealloc(ptr.as_ptr(), layout) };
            return None;
        }
        // SAFETY: the allocation is live, satisfies the constraints, and its
        // identity pointer is the fake device-visible address.
        Some(unsafe { DmaAllocHandle::new(ptr, ptr, dma_api::DmaAddr::from(address), layout) })
    }

    impl DmaOp for FakeDma {
        fn page_size(&self) -> usize {
            DMA_ALIGN
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            allocate(_constraints, layout)
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            // SAFETY: the handle came from allocate with the same pointer/layout
            // and the controller test has confirmed DMA retirement before drop.
            unsafe { std::alloc::dealloc(handle.allocation_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: the same aligned fake allocation contract is used by both
            // coherent and contiguous test buffers.
            unsafe { self.alloc_contiguous(constraints, layout) }
        }

        unsafe fn dealloc_coherent(
            &self,
            handle: DmaAllocHandle,
        ) -> core::result::Result<(), DmaError> {
            // SAFETY: the caller's matching allocation is retired.
            unsafe { self.dealloc_contiguous(handle) };
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> core::result::Result<DmaMapHandle, DmaError> {
            Err(DmaError::MappingFailed)
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    #[derive(Default)]
    struct FakeBus {
        registers: BTreeMap<usize, u32>,
        stop_readback_failures: usize,
        ignored_write: Option<usize>,
        writes: usize,
        now_ns: u64,
    }

    impl HdaIo for FakeBus {
        fn mapped_len(&self) -> usize {
            0x200
        }

        fn read(&mut self, offset: usize, _width: RegisterWidth) -> u32 {
            if offset == 0x80
                && self.stop_readback_failures > 0
                && self.registers.get(&offset) == Some(&0)
            {
                self.stop_readback_failures -= 1;
                return 2;
            }
            self.registers.get(&offset).copied().unwrap_or_default()
        }

        fn write(&mut self, offset: usize, _width: RegisterWidth, value: u32) {
            self.writes += 1;
            if self.ignored_write == Some(offset) {
                return;
            }
            if offset == 0x83 {
                *self.registers.entry(offset).or_default() &= !value;
                return;
            }
            self.registers.insert(offset, value);
            if offset == 0x80 && value & 1 != 0 {
                self.registers.insert(0x84, 0);
            }
        }

        fn delay_us(&mut self, _micros: u32) {}

        fn now_ns(&self) -> u64 {
            self.now_ns
        }
    }

    #[test]
    fn unconfirmed_stop_poisoning_blocks_prepare_and_submit_until_reset() {
        static DMA: FakeDma = FakeDma;
        let info = DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        );
        let dma = DeviceDma::new(info, &DMA);
        let dma = DmaBuffers::new(&dma, 1).unwrap();
        let mut bus = FakeBus {
            stop_readback_failures: 10_000,
            ..FakeBus::default()
        };
        bus.registers.insert(0x80, 2);
        let mut controller = Controller {
            bus,
            dma: ManuallyDrop::new(dma),
            dma_published: true,
            dma_released: false,
            wp: 0,
            rp: 0,
            stream: 0x80,
            route: None,
            present: 0,
            pending: VecDeque::new(),
            retired: VecDeque::new(),
            tail: 0,
            position: 0,
            running: true,
            prepared: true,
            live: true,
            sequence: 0,
            last_poll: 0,
        };

        assert_eq!(controller.release(), Err(Error::Io));
        assert!(
            controller.running,
            "failed readback must not claim DMA stopped"
        );
        assert!(
            !controller.live,
            "failed readback poisons future operations"
        );
        let writes_after_failed_stop = controller.bus.writes;
        assert_eq!(
            controller.prepare(PERIOD as u32, PERIODS as u32),
            Err(Error::BadState)
        );
        assert_eq!(controller.submit(&[0; PERIOD]), Err(Error::BadState));
        assert_eq!(controller.bus.writes, writes_after_failed_stop);

        controller.shutdown().unwrap();
        assert!(controller.dma_released);
    }

    #[test]
    fn command_ring_start_requires_dma_enable_readback() {
        static DMA: FakeDma = FakeDma;
        let info = DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        );

        for ignored_write in [regs::RIRBCTL, regs::CORBCTL] {
            let dma = DeviceDma::new(info, &DMA);
            let dma = DmaBuffers::new(&dma, 1).unwrap();
            let mut bus = FakeBus {
                ignored_write: Some(ignored_write),
                ..FakeBus::default()
            };
            bus.registers.insert(regs::CORBSIZE, 64);
            bus.registers.insert(regs::RIRBSIZE, 64);
            let mut controller = Controller {
                bus,
                dma: ManuallyDrop::new(dma),
                dma_published: false,
                dma_released: false,
                wp: 0,
                rp: 0,
                stream: 0x80,
                route: None,
                present: 0,
                pending: VecDeque::new(),
                retired: VecDeque::new(),
                tail: 0,
                position: 0,
                running: false,
                prepared: false,
                live: true,
                sequence: 0,
                last_poll: 0,
            };

            assert_eq!(controller.initialize(), Err(Error::Io));
        }
    }

    #[test]
    fn submit_synchronizes_old_period_completions_before_queueing() {
        static DMA: FakeDma = FakeDma;
        let info = DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        );
        let dma = DeviceDma::new(info, &DMA);
        let dma = DmaBuffers::new(&dma, 1).unwrap();
        let mut bus = FakeBus {
            now_ns: 50_000_000,
            ..FakeBus::default()
        };
        bus.registers.insert(0x84, (2 * PERIOD) as u32);
        let mut controller = Controller {
            bus,
            dma: ManuallyDrop::new(dma),
            dma_published: true,
            dma_released: false,
            wp: 0,
            rp: 0,
            stream: 0x80,
            route: None,
            present: 0,
            pending: VecDeque::from([(0, 0)]),
            retired: VecDeque::new(),
            tail: 1,
            position: 0,
            running: true,
            prepared: true,
            live: true,
            sequence: 0,
            last_poll: 0,
        };

        assert_eq!(controller.submit(&[0x5a; PERIOD]), Err(Error::Again));
        assert_eq!(
            controller.prepare(PERIOD as u32, PERIODS as u32),
            Err(Error::ResourceBusy)
        );
        assert_eq!(controller.complete(), Ok(Some(0)));
        assert_eq!(controller.submit(&[0x5a; PERIOD]), Ok(1));
        assert_eq!(controller.complete(), Ok(None));
        controller.shutdown().unwrap();
    }
}
