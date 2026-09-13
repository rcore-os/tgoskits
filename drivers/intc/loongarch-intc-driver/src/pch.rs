//! Loongson PCH-PIC register core and immutable cascade mapper.

use mmio_api::MmioRaw;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

use crate::{
    EioVector, IntcError, MAX_EIO_VECTORS, MAX_PCH_INPUTS, PchInput, mmio::validate_mmio_region,
};

register_structs! {
    PchPicIdentityRegisters {
        (0x000 => id: ReadOnly<u64>),
        (0x008 => @END),
    }
}

register_structs! {
    PchPicRegisters {
        (0x000 => id: ReadOnly<u64>),
        (0x008 => _reserved0),
        (0x020 => mask: [ReadWrite<u32>; 2]),
        (0x028 => _reserved1),
        (0x060 => edge: [ReadWrite<u32>; 2]),
        (0x068 => _reserved2),
        (0x200 => htvec: [ReadWrite<u8>; MAX_PCH_INPUTS]),
        (0x240 => _reserved3),
        (0x3e0 => polarity: [ReadWrite<u32>; 2]),
        (0x3e8 => @END),
    }
}

/// PCH-PIC input trigger mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PchIrqTrigger {
    /// Edge-triggered input.
    Edge,
    /// Level-triggered input.
    Level,
}

/// PCH-PIC input polarity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PchIrqPolarity {
    /// Input is asserted high.
    ActiveHigh,
    /// Input is asserted low.
    ActiveLow,
}

/// Validated PCH-PIC vector and ACPI identity configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PchPicConfig {
    base_vector: usize,
    input_count: usize,
    acpi_controller_id: u16,
}

impl PchPicConfig {
    /// Creates a PCH-PIC configuration.
    ///
    /// `base_vector + input` is the immutable EIOINTC vector used by the
    /// cascade. `acpi_controller_id` is compared with firmware routes when the
    /// `rdif` feature is enabled.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError`] for a zero/oversized input count or a vector
    /// range that cannot be represented by EIOINTC.
    pub const fn new(
        base_vector: usize,
        input_count: usize,
        acpi_controller_id: u16,
    ) -> Result<Self, IntcError> {
        if input_count == 0 || input_count > MAX_PCH_INPUTS {
            return Err(IntcError::InvalidCount {
                controller: "PCH-PIC",
                count: input_count,
                min: 1,
                max: MAX_PCH_INPUTS,
            });
        }
        let Some(end) = base_vector.checked_add(input_count) else {
            return Err(IntcError::InvalidPchVectorRange {
                base: base_vector,
                count: input_count,
            });
        };
        if end > MAX_EIO_VECTORS {
            return Err(IntcError::InvalidPchVectorRange {
                base: base_vector,
                count: input_count,
            });
        }
        Ok(Self {
            base_vector,
            input_count,
            acpi_controller_id,
        })
    }

    /// Detects the input count from the PCH-PIC ID register.
    ///
    /// # Errors
    ///
    /// Returns an MMIO layout error when the ID register is not fully and
    /// naturally aligned in the mapping, or an invalid-count error when
    /// hardware reports more than 64 inputs.
    pub fn detect(
        mmio: &MmioRaw,
        base_vector: usize,
        acpi_controller_id: u16,
    ) -> Result<Self, IntcError> {
        validate_mmio_region::<PchPicIdentityRegisters>(mmio, "PCH-PIC identity")?;
        let input_count = (((pch_identity_registers(mmio).id.get() >> 48) & 0xff) as usize) + 1;
        Self::new(base_vector, input_count, acpi_controller_id)
    }

    /// Returns the first EIOINTC vector assigned to the PIC.
    pub const fn base_vector(self) -> usize {
        self.base_vector
    }

    /// Returns the number of PIC inputs.
    pub const fn input_count(self) -> usize {
        self.input_count
    }

    /// Returns the ACPI controller identifier expected in firmware routes.
    pub const fn acpi_controller_id(self) -> u16 {
        self.acpi_controller_id
    }

    fn validate(self) -> Result<(), IntcError> {
        Self::new(self.base_vector, self.input_count, self.acpi_controller_id).map(|_| ())
    }
}

/// Split PCH-PIC endpoints returned by [`PchPicParts::new`].
#[derive(Debug)]
pub struct PchPicParts {
    /// Task-context configuration and local mask endpoint.
    pub controller: PchPicController,
    /// Immutable EIO-vector to PCH-input mapper for hard IRQ dispatch.
    pub cpu_interface: PchPicCpuInterface,
}

impl PchPicParts {
    /// Validates the mapped register region, initializes trigger/polarity
    /// state, and returns split endpoints.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError`] when the configuration is invalid or `mmio` is
    /// too small or misaligned for the typed register block.
    pub fn new(mmio: MmioRaw, config: PchPicConfig) -> Result<Self, IntcError> {
        config.validate()?;
        validate_mmio_region::<PchPicRegisters>(&mmio, "PCH-PIC")?;
        let controller_address = mmio.phys_addr().as_usize() as u64;
        let controller = PchPicController { mmio, config };
        controller.initialize();
        Ok(Self {
            controller,
            cpu_interface: PchPicCpuInterface {
                controller_address,
                config,
            },
        })
    }
}

/// Task-context PCH-PIC control endpoint.
#[derive(Debug)]
pub struct PchPicController {
    mmio: MmioRaw,
    config: PchPicConfig,
}

impl PchPicController {
    /// Returns the immutable controller configuration.
    pub const fn config(&self) -> PchPicConfig {
        self.config
    }

    /// Returns the physical address used as the ACPI controller identity.
    pub fn controller_address(&self) -> u64 {
        self.mmio.phys_addr().as_usize() as u64
    }

    /// Programs one input's trigger, polarity, and local EIO vector route.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError::OutsideConfiguredRange`] when `input` is not
    /// implemented by this controller.
    pub fn configure_input(
        &mut self,
        input: PchInput,
        trigger: PchIrqTrigger,
        polarity: PchIrqPolarity,
    ) -> Result<(), IntcError> {
        self.check_input(input)?;
        let (register, bit) = pch_register_bit(input);
        let registers = self.registers();

        let edge = registers.edge[register].get();
        registers.edge[register].set(match trigger {
            PchIrqTrigger::Edge => edge | bit,
            PchIrqTrigger::Level => edge & !bit,
        });

        let polarity_value = registers.polarity[register].get();
        registers.polarity[register].set(match polarity {
            PchIrqPolarity::ActiveHigh => polarity_value & !bit,
            PchIrqPolarity::ActiveLow => polarity_value | bit,
        });
        self.write_vector(input)
    }

    /// Enables or disables one local PCH-PIC input.
    ///
    /// This method never changes the parent EIOINTC. Platform glue owns the
    /// parent/local sequence and rollback policy.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError::OutsideConfiguredRange`] when `input` is not
    /// implemented by this controller.
    pub fn set_enabled(&mut self, input: PchInput, enabled: bool) -> Result<(), IntcError> {
        self.check_input(input)?;
        if enabled {
            // Route while the local source is still masked, then expose it.
            self.write_vector(input)?;
        }
        let (register, bit) = pch_register_bit(input);
        let mask = self.registers().mask[register].get();
        self.registers().mask[register].set(if enabled { mask & !bit } else { mask | bit });
        Ok(())
    }

    /// Returns the EIOINTC vector assigned to a local input.
    pub fn external_vector_for_input(&self, input: PchInput) -> Result<EioVector, IntcError> {
        self.check_input(input)?;
        EioVector::new(self.config.base_vector + input.raw())
    }

    fn initialize(&self) {
        let registers = self.registers();
        for edge in &registers.edge {
            edge.set(0);
        }
        for polarity in &registers.polarity {
            polarity.set(0);
        }
    }

    fn write_vector(&self, input: PchInput) -> Result<(), IntcError> {
        let vector = self.external_vector_for_input(input)?;
        self.registers().htvec[input.raw()].set(vector.raw() as u8);
        Ok(())
    }

    fn check_input(&self, input: PchInput) -> Result<(), IntcError> {
        if input.raw() < self.config.input_count {
            Ok(())
        } else {
            Err(IntcError::OutsideConfiguredRange {
                kind: "PCH-PIC input",
                index: input.raw(),
                count: self.config.input_count,
            })
        }
    }

    fn registers(&self) -> &PchPicRegisters {
        // SAFETY: `PchPicParts::new` validates the complete register block and
        // its natural alignment. The caller's `MmioRaw` contract keeps the
        // mapping valid while this controller retains the capability handle.
        unsafe { &*self.mmio.as_ptr().cast::<PchPicRegisters>() }
    }
}

/// Immutable PCH-PIC cascade mapper used by hard IRQ dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PchPicCpuInterface {
    controller_address: u64,
    config: PchPicConfig,
}

impl PchPicCpuInterface {
    /// Maps an EIOINTC vector to the local PCH-PIC input, if this PIC owns it.
    pub fn input_for_external_vector(&self, vector: EioVector) -> Option<PchInput> {
        let input = vector.raw().checked_sub(self.config.base_vector)?;
        if input >= self.config.input_count {
            return None;
        }
        PchInput::new(input).ok()
    }

    /// Returns the EIOINTC vector assigned to a PCH-PIC input.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError::OutsideConfiguredRange`] when `input` is not
    /// implemented by this PIC.
    pub fn external_vector_for_input(&self, input: PchInput) -> Result<EioVector, IntcError> {
        if input.raw() >= self.config.input_count {
            return Err(IntcError::OutsideConfiguredRange {
                kind: "PCH-PIC input",
                index: input.raw(),
                count: self.config.input_count,
            });
        }
        EioVector::new(self.config.base_vector + input.raw())
    }

    /// Returns the immutable controller configuration.
    pub const fn config(&self) -> PchPicConfig {
        self.config
    }

    /// Returns the physical address used as the ACPI controller identity.
    pub const fn controller_address(&self) -> u64 {
        self.controller_address
    }
}

fn pch_identity_registers(mmio: &MmioRaw) -> &PchPicIdentityRegisters {
    // SAFETY: `PchPicConfig::detect` validates the identity header size and
    // natural alignment before calling this helper. The `MmioRaw` construction
    // contract keeps the mapping valid for the returned borrow.
    unsafe { &*mmio.as_ptr().cast::<PchPicIdentityRegisters>() }
}

const fn pch_register_bit(input: PchInput) -> (usize, u32) {
    let raw = input.raw();
    (raw / 32, 1u32 << (raw % 32))
}
