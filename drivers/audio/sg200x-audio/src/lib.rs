// SPDX-License-Identifier: GPL-2.0-or-later
//! SG200x mono S16_LE capture. Register programming follows the CV181x SDK;
//! see README.md for the pinned reference and board qualification boundary.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

pub use dma_api::{DeviceDma, DmaError};
pub use mmio_api::Mmio;

mod dma;
mod frontend;

/// Capture failures; a stop timeout permanently quarantines DMA allocations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid capture resources or parameters")]
    Invalid,
    #[error("capture hardware is already active")]
    Busy,
    #[error("unsupported audio clock configuration")]
    Clock,
    #[error("hardware did not stop or reset")]
    Timeout,
    #[error("DMA allocation failed: {0}")]
    Allocation(#[from] DmaError),
    #[error("capture overrun")]
    Overrun,
    #[error("capture DMA status {dma:#x}, FIFO status {fifo:#x}")]
    Hardware { dma: u32, fifo: u8 },
}

/// Mapped resources retained for the lifetime of both control and IRQ ports.
pub struct Resources {
    pub adc: Mmio,
    pub dac: Mmio,
    pub i2s: Mmio,
    pub mclk: Mmio,
    pub aiao: Mmio,
    pub crg: Mmio,
    pub reset: Mmio,
    pub dma: Mmio,
    pub oscillator_hz: u32,
}

/// System DMA physical channel and hardware handshake route, not an IRQ ID.
#[derive(Clone, Copy, Debug)]
pub struct DmaRoute {
    pub channel: u8,
    pub request: u8,
    pub memory_master: u8,
    pub peripheral_master: u8,
}

/// Only mono S16_LE is supported. Sizes are samples (frames), not bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub rate: u32,
    pub period_frames: u32,
    pub buffer_frames: u32,
}

impl Config {
    /// One hardware block: 32 transfers of four bytes, or 64 mono S16 frames.
    pub const DMA_BLOCK_FRAMES: u32 = 64;

    /// Supported geometries, ordered by increasing rate and period size, then
    /// decreasing buffer size. A ring must hold a period, the in-flight DMA
    /// block, and service margin before an unobserved lap becomes ambiguous.
    pub fn supported() -> impl Iterator<Item = Self> {
        [16_000, 48_000].into_iter().flat_map(|rate| {
            (Self::DMA_BLOCK_FRAMES..=8192)
                .step_by(Self::DMA_BLOCK_FRAMES as usize)
                .flat_map(move |period_frames| {
                    (2..=16).rev().filter_map(move |periods| {
                        let buffer_frames = period_frames * periods;
                        (buffer_frames <= 32768
                            && buffer_frames > period_frames + Self::DMA_BLOCK_FRAMES)
                            .then_some(Self {
                                rate,
                                period_frames,
                                buffer_frames,
                            })
                    })
                })
        })
    }
}

struct Shared {
    i2s: Mmio,
    dma: Mmio,
    route: DmaRoute,
    // Low word: armed (bit 0), FIFO faults (1..2), DMA faults (5..31).
    // High word distinguishes consecutive prepares from stale IRQs.
    faults: AtomicU64,
}

/// IRQ-only endpoint. Acknowledge and latch, then notify one OS service task.
/// It neither allocates nor locks and never calls an arbitrary user waker.
pub struct Interrupt(Arc<Shared>);

impl Interrupt {
    pub fn acknowledge(&self) -> bool {
        let epoch = self.0.faults.load(Ordering::Acquire);
        let dma = self.0.dma.read::<u64>(self.0.channel_reg(0x88));
        let fifo_status = self.0.i2s.read::<u32>(0x24) & 0x606;
        let fifo = (fifo_status | (fifo_status >> 8)) & 6;
        self.0.dma.write(self.0.channel_reg(0x98), dma);
        self.0.i2s.write(0x24, fifo_status);
        if epoch & 1 != 0 && (dma & dma::ERRORS != 0 || fifo != 0) {
            // An old IRQ must not fault a stream prepared after it took the
            // snapshot. Both physical IRQs may converge on this endpoint.
            let _ = self.0.faults.compare_exchange(
                epoch,
                epoch | (dma & dma::ERRORS) | u64::from(fifo),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        dma != 0 || fifo != 0
    }
}

impl Shared {
    fn channel_reg(&self, offset: usize) -> usize {
        0x100 * (usize::from(self.route.channel) + 1) + offset
    }
}

/// Task-owned control/queue port. Serialize its methods in the OS adapter.
pub struct Capture {
    shared: Arc<Shared>,
    frontend: frontend::Frontend,
    allocator: DeviceDma,
    ring: Option<dma::Ring>,
    cursor: dma::Cursor,
    gain: u8,
    phase: Phase,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Idle,
    Prepared,
    Running,
    Poisoned,
}

impl Capture {
    /// Construct disabled ports without resetting the shared DMA controller.
    ///
    /// # Safety
    /// The resources must map the specified SG200x peripherals with device
    /// memory attributes. The adapter exclusively owns ADC, I2S0, I2S3 clock
    /// output and the selected DMA channel/handshake, including its IRQ status.
    /// Shared CRG/reset register RMWs must be serialized with platform probing;
    /// no other driver may change audio clocks while this object exists.
    pub unsafe fn new(
        resources: Resources,
        route: DmaRoute,
        allocator: DeviceDma,
    ) -> Result<(Self, Interrupt), Error> {
        if route.channel >= 8
            || route.request >= 16
            || route.memory_master > 1
            || route.peripheral_master > 1
            || resources.dma.size() < 0x900
            || resources.i2s.size() < 0x84
        {
            return Err(Error::Invalid);
        }
        if resources.dma.read::<u64>(0x18) & (1 << route.channel) != 0
            || resources.i2s.read::<u32>(0x18) != 0
            || resources.adc.read::<u32>(0) & 3 != 0
        {
            return Err(Error::Busy);
        }
        let Resources {
            adc,
            dac,
            i2s,
            mclk,
            aiao,
            crg,
            reset,
            dma,
            oscillator_hz,
        } = resources;
        let frontend = frontend::Frontend::new(adc, dac, mclk, aiao, crg, reset, oscillator_hz)?;
        let shared = Arc::new(Shared {
            i2s,
            dma,
            route,
            faults: AtomicU64::new(0),
        });
        shared.i2s.write(0x20, 0u32);
        shared.dma.write(shared.channel_reg(0x90), 0u64);
        let interrupt = Interrupt(shared.clone());
        Ok((
            Self {
                shared,
                frontend,
                allocator,
                ring: None,
                cursor: dma::Cursor::default(),
                gain: 20,
                phase: Phase::Idle,
            },
            interrupt,
        ))
    }

    /// Allocate a stopped ring; failures leave no active transfer.
    pub fn configure(&mut self, config: Config) -> Result<(), Error> {
        if !Config::supported().any(|candidate| candidate == config) {
            return Err(Error::Invalid);
        }
        self.stop()?;
        let ring = dma::Ring::new(&self.allocator, &self.shared, config)?;
        self.ring = Some(ring);
        Ok(())
    }

    /// Reset ADC/FIFO and reapply the selected clock and gain. Task context only.
    pub fn prepare(&mut self, now_ns: impl Fn() -> u64) -> Result<(), Error> {
        self.stop()?;
        let config = self.ring.as_ref().ok_or(Error::Invalid)?.config;
        self.frontend
            .prepare(&self.shared.i2s, config.rate, self.gain, now_ns)?;
        let epoch = self.shared.faults.load(Ordering::Acquire);
        self.shared.faults.store(
            (epoch & !u64::from(u32::MAX)).wrapping_add(1 << 32),
            Ordering::Release,
        );
        self.shared.i2s.write(0x24, 0x606u32);
        self.shared
            .dma
            .write(self.shared.channel_reg(0x98), u64::MAX);
        self.cursor = dma::Cursor::default();
        self.phase = Phase::Prepared;
        Ok(())
    }

    pub fn start(&mut self, now_ns: u64) -> Result<(), Error> {
        match self.phase {
            Phase::Prepared => (),
            Phase::Running => return Err(Error::Busy),
            Phase::Poisoned => return Err(Error::Timeout),
            Phase::Idle => return Err(Error::Invalid),
        }
        self.shared.faults.fetch_or(1, Ordering::Release);
        self.ring
            .as_ref()
            .ok_or(Error::Invalid)?
            .start(&self.shared);
        self.cursor = dma::Cursor::default();
        self.cursor.last_ns = now_ns;
        self.shared.i2s.write(0x20, 0x106u32); // RX faults plus IP-wide IRQ gate.
        self.shared.i2s.write(0x18, 1u32);
        self.phase = Phase::Running;
        Ok(())
    }

    /// Disable only this channel; CH_EN clears only after outstanding AXI
    /// accesses finish. On failure retain its memory forever, never re-arm it.
    pub fn stop(&mut self) -> Result<(), Error> {
        self.shared.faults.fetch_and(!1, Ordering::AcqRel);
        self.shared.i2s.write(0x20, 0u32);
        self.shared.i2s.write(0x18, 0u32);
        let stopped = dma::stop(&self.shared);
        if !stopped {
            self.phase = Phase::Poisoned;
            if let Some(ring) = self.ring.take() {
                core::mem::forget(ring);
            }
        }
        if self.phase == Phase::Poisoned {
            Err(Error::Timeout)
        } else {
            self.phase = Phase::Idle;
            Ok(())
        }
    }

    pub fn release(&mut self) -> Result<(), Error> {
        let stopped = self.stop();
        self.frontend.disable();
        stopped?;
        self.ring = None;
        Ok(())
    }

    /// Observe completed frames. Long scheduling gaps are XRUN, since a
    /// modulo hardware pointer cannot distinguish one lap from no progress.
    pub fn progress(&mut self, now_ns: u64) -> Result<u64, Error> {
        if self.phase != Phase::Running {
            return Ok(self.cursor.frames);
        }
        let faults = self.shared.faults.load(Ordering::Acquire);
        if faults & u64::from(u32::MAX) & !1 != 0 {
            return Err(Error::Hardware {
                dma: (faults & dma::ERRORS) as u32,
                fifo: (faults & 6) as u8,
            });
        }
        let ring = self.ring.as_ref().ok_or(Error::Invalid)?;
        let position = ring.position(&self.shared)?;
        self.cursor.advance(position, now_ns, ring.config)?;
        Ok(self.cursor.frames)
    }

    /// Copy completed samples without constructing references into DMA-owned
    /// memory. The adapter must check progress again before accepting this copy.
    pub fn copy_samples(&self, frame: u64, output: &mut [i16]) -> Result<(), Error> {
        self.ring
            .as_ref()
            .ok_or(Error::Invalid)?
            .copy(frame, output);
        Ok(())
    }

    pub fn gain(&self) -> u8 {
        self.gain
    }

    /// Analog gain in 2 dB steps, 0..=24. Does not mute the input.
    pub fn set_gain(&mut self, gain: u8) -> Result<(), Error> {
        self.frontend.set_gain(gain)?;
        self.gain = gain;
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
