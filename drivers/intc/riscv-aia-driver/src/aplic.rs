use core::{marker::PhantomData, num::NonZeroU32, ptr::NonNull};

use crate::Error;

const WINDOW_SIZE: usize = 0x4000;
const DOMAINCFG: usize = 0;
const SOURCECFG: usize = 4;
const CLRIE: usize = 0x1f00;
const SETIENUM: usize = 0x1edc;
const CLRIENUM: usize = 0x1fdc;
const SETIPNUM_LE: usize = 0x2000;
const TARGET: usize = 0x3004;

/// APLIC source interpretation. Detached/delegated sources are not supported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SourceMode {
    Inactive    = 0,
    EdgeRising  = 4,
    EdgeFalling = 5,
    LevelHigh   = 6,
    LevelLow    = 7,
}

impl SourceMode {
    /// Whether completion must resample the input line.
    pub const fn is_level(self) -> bool {
        matches!(self, Self::LevelHigh | Self::LevelLow)
    }
}

/// Validated MSI target with guest index zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiTarget(u32);

impl MsiTarget {
    /// Encode a combined hart/group index and a nonzero EIID.
    pub fn new(hart_index: u32, identity: NonZeroU32) -> Result<Self, Error> {
        if hart_index >= 1 << 14 || identity.get() >= 1 << 11 {
            return Err(Error::InvalidTarget);
        }
        Ok(Self((hart_index << 18) | identity.get()))
    }
}

/// Exclusive control capability for a mapped 16 KiB APLIC register window.
///
/// This object is movable between CPUs but cannot be shared without a lock.
/// A platform must serialize complete source transactions, including ownership
/// updates, and exclude local IRQ reentry when an IRQ endpoint uses that lock.
#[derive(Debug)]
pub struct Aplic<'a> {
    base: NonNull<u8>,
    sources: usize,
    mapping: PhantomData<&'a mut [u32; WINDOW_SIZE / 4]>,
}

// SAFETY: no CPU-local state is retained; control accesses require &mut self.
// Construction requires a mapping usable on any CPU receiving the capability,
// and the lifetime prevents retaining it beyond the caller's mapping lease.
unsafe impl Send for Aplic<'_> {}

impl<'a> Aplic<'a> {
    /// Acquire a mapped APLIC register window without touching the device.
    ///
    /// # Safety
    /// `base` must identify at least `size` bytes of live MMIO with 32-bit
    /// volatile access and suitable device memory attributes. The first 16 KiB
    /// must remain mapped for `'a`, on every CPU to which this object is moved.
    /// The caller must transfer sole software control of configuration, masks
    /// and compound source transactions for `'a`; it must not create another
    /// control capability or ordinary reference to this window. Platform-
    /// authorized, non-destructive volatile status reads and SETIPNUM injection
    /// for independently reserved sources may coexist on actual device MMIO.
    /// Such injection must not race source configuration or ownership changes.
    /// If backed by ordinary RAM, all outside accesses must instead wait until
    /// this capability is released. Hardware may independently update registers.
    pub unsafe fn new(base: NonNull<u8>, size: usize, sources: usize) -> Result<Self, Error> {
        if size < WINDOW_SIZE
            || base.as_ptr() as usize & 3 != 0
            || (base.as_ptr() as usize).checked_add(size).is_none()
        {
            return Err(Error::InvalidWindow);
        }
        if !(1..=1023).contains(&sources) {
            return Err(Error::InvalidSource);
        }
        Ok(Self {
            base,
            sources,
            mapping: PhantomData,
        })
    }

    /// Disable delivery, select MSI mode, mask and deactivate every source.
    pub fn reset_msi(&mut self) {
        self.write(DOMAINCFG, 1 << 2);
        for source in (0..=self.sources).step_by(32) {
            self.write(CLRIE + source / 32 * 4, u32::MAX);
        }
        for source in 1..=self.sources {
            self.write(SOURCECFG + (source - 1) * 4, SourceMode::Inactive as u32);
        }
    }

    /// Open delivery after both control and completion endpoints are published.
    pub fn enable_msi_domain(&mut self) {
        self.write(DOMAINCFG, (1 << 2) | (1 << 8));
    }

    /// Set the source interpretation. The caller chooses transaction ordering.
    pub fn set_source_mode(&mut self, source: NonZeroU32, mode: SourceMode) -> Result<(), Error> {
        let index = self.source_index(source)?;
        self.write(SOURCECFG + (index - 1) * 4, mode as u32);
        Ok(())
    }

    /// Install a validated MSI target.
    pub fn set_target(&mut self, source: NonZeroU32, target: MsiTarget) -> Result<(), Error> {
        let index = self.source_index(source)?;
        self.write(TARGET + (index - 1) * 4, target.0);
        Ok(())
    }

    /// Mask or unmask one source using the numbered command registers.
    pub fn set_enabled(&mut self, source: NonZeroU32, enabled: bool) -> Result<(), Error> {
        self.source_index(source)?;
        self.write(if enabled { SETIENUM } else { CLRIENUM }, source.get());
        Ok(())
    }

    /// Set a source pending through the little-endian numbered register.
    /// For a level source this resamples its current rectified input.
    pub fn set_pending(&mut self, source: NonZeroU32) -> Result<(), Error> {
        self.source_index(source)?;
        self.write(SETIPNUM_LE, source.get());
        Ok(())
    }

    /// Resample a level input after mode changes or handler completion.
    /// Edge inputs are left untouched.
    pub fn resample(&mut self, source: NonZeroU32) -> Result<(), Error> {
        let index = self.source_index(source)?;
        let mode = self.read(SOURCECFG + (index - 1) * 4) & 7;
        if matches!(mode, 6 | 7) {
            self.write(SETIPNUM_LE, source.get());
        }
        Ok(())
    }

    fn source_index(&self, source: NonZeroU32) -> Result<usize, Error> {
        let index = source.get() as usize;
        if index > self.sources {
            Err(Error::InvalidSource)
        } else {
            Ok(index)
        }
    }

    fn read(&mut self, offset: usize) -> u32 {
        io_fence();
        // SAFETY: construction retains sole control of an aligned window; private
        // offsets are aligned and bounded by 0x3ffc for at most 1023 sources.
        let value = unsafe { self.base.as_ptr().add(offset).cast::<u32>().read_volatile() };
        io_fence();
        value
    }

    fn write(&mut self, offset: usize, value: u32) {
        io_fence();
        // SAFETY: same window invariant as read; &mut self excludes software
        // races, and all checked source-derived offsets stay inside 16 KiB.
        unsafe {
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value)
        };
        io_fence();
    }
}

fn io_fence() {
    #[cfg(target_arch = "riscv64")]
    // SAFETY: this unprivileged fence orders I/O and memory on RVWMO.
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    #[cfg(not(target_arch = "riscv64"))]
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}
