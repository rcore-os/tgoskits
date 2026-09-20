//! Loongson EIOINTC register core and hard-IRQ CPU interface.

use core::fmt::Debug;

use crate::{EioVector, IntcError, MAX_EIO_VECTORS};

const MIN_EIO_VECTORS: usize = 128;
const EIO_VECTOR_GRANULARITY: usize = 128;
const VECTORS_PER_U64: usize = 64;

const IOCSR_MISC_FUNC: usize = 0x420;
const MISC_FUNC_EXT_IOI_ENABLE: u64 = 1 << 48;

const REG_NODEMAP: usize = 0x14a0;
const REG_IPMAP: usize = 0x14c0;
const REG_ENABLE: usize = 0x1600;
const REG_BOUNCE: usize = 0x1680;
const REG_ISR: usize = 0x1800;
const REG_ROUTE: usize = 0x1c00;

/// Minimal capability required to access EIOINTC IOCSR registers.
///
/// Platform code supplies an implementation with shutdown lifetime. The
/// controller and CPU interface each own a clone of the capability, while
/// synchronization of task-context controller operations remains the
/// caller's responsibility.
///
/// The CPU interface calls `read_u64` and `write_u64` from hard-IRQ context.
/// Production implementations must therefore be IRQ-safe and bounded: they
/// must not sleep, allocate, acquire a blocking lock, or call back into OS
/// services. Host tests may use a fake backend because they do not execute in
/// hard-IRQ context.
pub trait IocsrAccess: Clone + Debug + Send + Sync + 'static {
    /// Reads a 64-bit IOCSR register.
    fn read_u64(&self, offset: usize) -> u64;
    /// Writes a 64-bit IOCSR register.
    fn write_u64(&self, offset: usize, value: u64);
    /// Writes a 32-bit IOCSR register.
    fn write_u32(&self, offset: usize, value: u32);
}

/// Native IOCSR access for LoongArch targets.
#[cfg(target_arch = "loongarch64")]
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeIocsr;

#[cfg(target_arch = "loongarch64")]
impl IocsrAccess for NativeIocsr {
    fn read_u64(&self, offset: usize) -> u64 {
        loongArch64::iocsr::iocsr_read_d(offset)
    }

    fn write_u64(&self, offset: usize, value: u64) {
        loongArch64::iocsr::iocsr_write_d(offset, value);
    }

    fn write_u32(&self, offset: usize, value: u32) {
        loongArch64::iocsr::iocsr_write_w(offset, value);
    }
}

/// Validated EIOINTC configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EioIntcConfig {
    vector_count: usize,
}

impl EioIntcConfig {
    /// Creates a CPU0/single-node EIOINTC configuration.
    ///
    /// # Errors
    ///
    /// The supported hardware layouts contain 128 or 256 vectors. Other
    /// counts are rejected instead of partially programming register groups.
    pub const fn new(vector_count: usize) -> Result<Self, IntcError> {
        if vector_count < MIN_EIO_VECTORS || vector_count > MAX_EIO_VECTORS {
            return Err(IntcError::InvalidCount {
                controller: "EIOINTC",
                count: vector_count,
                min: MIN_EIO_VECTORS,
                max: MAX_EIO_VECTORS,
            });
        }
        if !vector_count.is_multiple_of(EIO_VECTOR_GRANULARITY) {
            return Err(IntcError::InvalidCountGranularity {
                controller: "EIOINTC",
                count: vector_count,
                granularity: EIO_VECTOR_GRANULARITY,
            });
        }
        Ok(Self { vector_count })
    }

    /// Returns the configured vector count.
    pub const fn vector_count(self) -> usize {
        self.vector_count
    }

    fn validate(self) -> Result<(), IntcError> {
        Self::new(self.vector_count).map(|_| ())
    }
}

/// Split EIOINTC endpoints returned by [`EioIntcParts::new`].
#[derive(Debug)]
pub struct EioIntcParts<A: IocsrAccess> {
    /// Task-context initialization and enable control endpoint.
    pub controller: EioIntcController<A>,
    /// Lock-free hard-IRQ claim/complete endpoint.
    pub cpu_interface: EioIntcCpuInterface<A>,
}

impl<A: IocsrAccess> EioIntcParts<A> {
    /// Validates and initializes one EIOINTC, returning split ownership.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError`] when `config` does not describe a supported
    /// register layout.
    pub fn new(access: A, config: EioIntcConfig) -> Result<Self, IntcError> {
        config.validate()?;
        let controller = EioIntcController {
            access: access.clone(),
            config,
        };
        controller.initialize();
        Ok(Self {
            controller,
            cpu_interface: EioIntcCpuInterface { access, config },
        })
    }
}

/// Task-context EIOINTC control endpoint.
#[derive(Debug)]
pub struct EioIntcController<A: IocsrAccess> {
    access: A,
    config: EioIntcConfig,
}

impl<A: IocsrAccess> EioIntcController<A> {
    /// Returns the number of vectors implemented by this controller.
    pub const fn vector_count(&self) -> usize {
        self.config.vector_count
    }

    /// Enables or disables one EIOINTC vector.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError::OutsideConfiguredRange`] when `vector` belongs to
    /// the EIO family but not this controller instance.
    pub fn set_enabled(&mut self, vector: EioVector, enabled: bool) -> Result<(), IntcError> {
        self.check_vector(vector)?;
        let (offset, bit) = eio_register_bit(vector);
        let enable_offset = REG_ENABLE + offset;
        let current = self.access.read_u64(enable_offset);
        self.access.write_u64(
            enable_offset,
            if enabled {
                current | bit
            } else {
                current & !bit
            },
        );
        if enabled {
            let bounce_offset = REG_BOUNCE + offset;
            let current = self.access.read_u64(bounce_offset);
            self.access.write_u64(bounce_offset, current | bit);
        }
        Ok(())
    }

    fn initialize(&self) {
        let misc = self.access.read_u64(IOCSR_MISC_FUNC);
        self.access
            .write_u64(IOCSR_MISC_FUNC, misc | MISC_FUNC_EXT_IOI_ENABLE);

        for group in 0..self.config.vector_count / 32 {
            let local = 1u32 << (group * 2);
            let node = 1u32 << (group * 2 + 1);
            self.access
                .write_u32(REG_NODEMAP + group * 4, local | (node << 16));
        }
        for group in 0..self.config.vector_count / EIO_VECTOR_GRANULARITY {
            self.access.write_u32(REG_IPMAP + group * 4, 0x0202_0202);
        }
        for group in 0..self.config.vector_count / 4 {
            self.access.write_u32(REG_ROUTE + group * 4, 0x0101_0101);
        }
        for group in 0..self.config.vector_count / 32 {
            self.access.write_u32(REG_BOUNCE + group * 4, u32::MAX);
        }
    }

    fn check_vector(&self, vector: EioVector) -> Result<(), IntcError> {
        if vector.raw() < self.config.vector_count {
            Ok(())
        } else {
            Err(IntcError::OutsideConfiguredRange {
                kind: "EIOINTC vector",
                index: vector.raw(),
                count: self.config.vector_count,
            })
        }
    }
}

/// Shutdown-lifetime EIOINTC CPU interface used by hard IRQ dispatch.
#[derive(Debug)]
pub struct EioIntcCpuInterface<A: IocsrAccess> {
    access: A,
    config: EioIntcConfig,
}

impl<A: IocsrAccess> EioIntcCpuInterface<A> {
    /// Claims the lowest pending EIOINTC vector without taking a controller
    /// lock.
    pub fn claim(&self) -> Option<EioVector> {
        for group in 0..self.config.vector_count.div_ceil(VECTORS_PER_U64) {
            let first_vector = group * VECTORS_PER_U64;
            let mut pending = self.access.read_u64(REG_ISR + group * 8);
            let remaining = self.config.vector_count - first_vector;
            if remaining < VECTORS_PER_U64 {
                pending &= (1u64 << remaining) - 1;
            }
            if pending == 0 {
                continue;
            }
            let raw = first_vector + pending.trailing_zeros() as usize;
            return EioVector::new(raw).ok();
        }
        None
    }

    /// Completes one vector with the EIOINTC ISR write-one-to-clear protocol.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError::OutsideConfiguredRange`] for a vector not owned by
    /// this controller instance.
    pub fn complete(&self, vector: EioVector) -> Result<(), IntcError> {
        if vector.raw() >= self.config.vector_count {
            return Err(IntcError::OutsideConfiguredRange {
                kind: "EIOINTC vector",
                index: vector.raw(),
                count: self.config.vector_count,
            });
        }
        let (offset, bit) = eio_register_bit(vector);
        self.access.write_u64(REG_ISR + offset, bit);
        Ok(())
    }

    /// Returns the configured vector count.
    pub const fn vector_count(&self) -> usize {
        self.config.vector_count
    }
}

const fn eio_register_bit(vector: EioVector) -> (usize, u64) {
    let raw = vector.raw();
    (raw / VECTORS_PER_U64 * 8, 1u64 << (raw % VECTORS_PER_U64))
}
