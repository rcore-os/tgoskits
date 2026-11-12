//! Loongson LIOINTC register core and lock-free CPU interface.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use mmio_api::MmioRaw;
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite, WriteOnly},
};

use crate::{CpuIrqLine, IntcError, LIO_INPUT_COUNT, LioInput, mmio::validate_mmio_region};

/// Number of LIOINTC parent CPU lines (INT0 through INT3).
pub const LIO_PARENT_COUNT: usize = 4;
/// CPU interrupt line corresponding to LIOINTC parent INT0.
pub const LIO_PARENT_FIRST_CPU_LINE: usize = 2;

register_structs! {
    LioIntcRegisters {
        (0x00 => route: [ReadWrite<u8>; LIO_INPUT_COUNT]),
        (0x20 => _reserved0),
        (0x28 => enable: WriteOnly<u32>),
        (0x2c => disable: WriteOnly<u32>),
        (0x30 => polarity: ReadWrite<u32>),
        (0x34 => edge: ReadWrite<u32>),
        (0x38 => @END),
    }
}

register_structs! {
    LioIntcIsrRegisters {
        (0x00 => pending: ReadOnly<u32>),
        (0x04 => @END),
    }
}

const ROUTE_CPU0: u8 = 1 << 0;
const ROUTE_INT_SHIFT: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LioRoutePolicy {
    FdtFallback,
    FirmwareExplicit,
}

/// Validated LIOINTC parent-line and input-route configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LioIntcConfig {
    parent_lines: [Option<CpuIrqLine>; LIO_PARENT_COUNT],
    parent_input_maps: [u32; LIO_PARENT_COUNT],
    effective_parent_input_maps: [u32; LIO_PARENT_COUNT],
    route_policy: LioRoutePolicy,
}

impl LioIntcConfig {
    /// Creates a LIOINTC configuration.
    ///
    /// Array slot zero is INT0/CPU line 2, slot one is INT1/CPU line 3, and so
    /// on. When multiple bitmaps select one input, the lowest parent slot wins.
    /// An input not selected by any bitmap falls back to the first present
    /// parent, preserving the hardware's existing CPU0/single-node behavior.
    ///
    /// # Errors
    ///
    /// Rejects a missing parent, a parent in the wrong INT slot, or a bitmap
    /// associated with an absent parent.
    pub const fn new(
        parent_lines: [Option<CpuIrqLine>; LIO_PARENT_COUNT],
        parent_input_maps: [u32; LIO_PARENT_COUNT],
    ) -> Result<Self, IntcError> {
        Self::from_routes(parent_lines, parent_input_maps, LioRoutePolicy::FdtFallback)
    }

    /// Creates routes using only the explicitly mapped firmware inputs.
    ///
    /// Slot N must name CPU line 2+N and have a nonzero, nonoverlapping input
    /// bitmap. Absent slots must have zero maps. Uncovered inputs remain
    /// disconnected, including the CPU selection bits in their route bytes.
    ///
    /// # Errors
    ///
    /// Rejects an empty route set, a parent in the wrong slot, a map without
    /// a parent, a parent without a map, or overlapping input maps.
    pub const fn from_firmware_routes(
        parent_lines: [Option<CpuIrqLine>; LIO_PARENT_COUNT],
        parent_input_maps: [u32; LIO_PARENT_COUNT],
    ) -> Result<Self, IntcError> {
        Self::from_routes(
            parent_lines,
            parent_input_maps,
            LioRoutePolicy::FirmwareExplicit,
        )
    }

    const fn from_routes(
        parent_lines: [Option<CpuIrqLine>; LIO_PARENT_COUNT],
        parent_input_maps: [u32; LIO_PARENT_COUNT],
        route_policy: LioRoutePolicy,
    ) -> Result<Self, IntcError> {
        let mut slot = 0;
        let mut first_parent_slot = LIO_PARENT_COUNT;
        while slot < LIO_PARENT_COUNT {
            match parent_lines[slot] {
                Some(line) => {
                    if first_parent_slot == LIO_PARENT_COUNT {
                        first_parent_slot = slot;
                    }
                    let expected = LIO_PARENT_FIRST_CPU_LINE + slot;
                    if line.raw() != expected {
                        return Err(IntcError::InvalidLioParentSlot {
                            slot,
                            expected,
                            actual: line.raw(),
                        });
                    }
                    if matches!(route_policy, LioRoutePolicy::FirmwareExplicit)
                        && parent_input_maps[slot] == 0
                    {
                        return Err(IntcError::LioParentWithoutMap { slot });
                    }
                }
                None if parent_input_maps[slot] != 0 => {
                    return Err(IntcError::LioMapWithoutParent {
                        slot,
                        map: parent_input_maps[slot],
                    });
                }
                None => {}
            }
            slot += 1;
        }
        if first_parent_slot == LIO_PARENT_COUNT {
            return Err(IntcError::MissingLioParent);
        }

        let mut effective_parent_input_maps = [0; LIO_PARENT_COUNT];
        let mut routed_inputs = 0;
        slot = 0;
        while slot < LIO_PARENT_COUNT {
            if parent_lines[slot].is_some() {
                let overlap = parent_input_maps[slot] & routed_inputs;
                if matches!(route_policy, LioRoutePolicy::FirmwareExplicit) && overlap != 0 {
                    return Err(IntcError::OverlappingLioMaps {
                        slot,
                        inputs: overlap,
                    });
                }
                let selected_inputs = parent_input_maps[slot] & !routed_inputs;
                effective_parent_input_maps[slot] = selected_inputs;
                routed_inputs |= selected_inputs;
            }
            slot += 1;
        }
        if matches!(route_policy, LioRoutePolicy::FdtFallback) {
            effective_parent_input_maps[first_parent_slot] |= !routed_inputs;
        }

        Ok(Self {
            parent_lines,
            parent_input_maps,
            effective_parent_input_maps,
            route_policy,
        })
    }

    /// Returns the four INT0..INT3 parent-line slots.
    pub const fn parent_lines(self) -> [Option<CpuIrqLine>; LIO_PARENT_COUNT] {
        self.parent_lines
    }

    /// Returns the firmware-provided input bitmap for each parent slot.
    ///
    /// [`Self::new`] resolves overlaps and fallback inputs; strict firmware
    /// routes retain these maps exactly for [`LioIntcCpuInterface::claim`].
    pub const fn parent_input_maps(self) -> [u32; LIO_PARENT_COUNT] {
        self.parent_input_maps
    }

    /// Returns whether an input has an effective parent route.
    pub fn is_input_routed(self, input: LioInput) -> bool {
        self.parent_slot_for_input(input).is_some()
    }

    /// Returns the CPU0 route byte, or zero for a disconnected input.
    pub fn route_value(self, input: LioInput) -> u8 {
        self.parent_slot_for_input(input)
            .map_or(0, |parent| ROUTE_CPU0 | (1 << (ROUTE_INT_SHIFT + parent)))
    }

    fn parent_slot_for_input(self, input: LioInput) -> Option<usize> {
        let mask = 1u32 << input.raw();
        self.effective_parent_input_maps
            .iter()
            .enumerate()
            .find(|(_, map)| (**map & mask) != 0)
            .map(|(slot, _)| slot)
    }

    fn effective_parent_input_map(self, line: CpuIrqLine) -> Option<u32> {
        self.parent_lines
            .iter()
            .position(|parent| *parent == Some(line))
            .map(|slot| self.effective_parent_input_maps[slot])
    }

    fn validate(self) -> Result<(), IntcError> {
        Self::from_routes(self.parent_lines, self.parent_input_maps, self.route_policy).map(|_| ())
    }
}

/// Split LIOINTC endpoints returned by [`LioIntcParts::new`].
#[derive(Debug)]
pub struct LioIntcParts {
    /// Task-context route and enable/disable endpoint.
    pub controller: LioIntcController,
    /// Lock-free hard-IRQ claim/complete endpoint.
    pub cpu_interface: LioIntcCpuInterface,
}

impl LioIntcParts {
    /// Validates both mappings, initializes the controller, and returns split
    /// endpoints sharing only a preallocated atomic enabled snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError`] for an invalid parent configuration or a mapping
    /// too small or misaligned for the typed register blocks.
    pub fn new(regs: MmioRaw, isr: MmioRaw, config: LioIntcConfig) -> Result<Self, IntcError> {
        config.validate()?;
        validate_mmio_region::<LioIntcRegisters>(&regs, "LIOINTC register")?;
        validate_mmio_region::<LioIntcIsrRegisters>(&isr, "LIOINTC ISR")?;

        let enabled = Arc::new(AtomicU32::new(0));
        let controller = LioIntcController {
            regs,
            config,
            enabled: Arc::clone(&enabled),
        };
        controller.initialize();
        Ok(Self {
            controller,
            cpu_interface: LioIntcCpuInterface {
                isr,
                config,
                enabled,
            },
        })
    }
}

/// Task-context LIOINTC control endpoint.
#[derive(Debug)]
pub struct LioIntcController {
    regs: MmioRaw,
    config: LioIntcConfig,
    enabled: Arc<AtomicU32>,
}

impl LioIntcController {
    /// Returns the immutable controller configuration.
    pub const fn config(&self) -> LioIntcConfig {
        self.config
    }

    /// Returns the physical address used as the ACPI controller identity.
    pub fn controller_address(&self) -> u64 {
        self.regs.phys_addr().as_usize() as u64
    }

    /// Enables or disables one input using the LIOINTC write-one protocol.
    ///
    /// Enable writes hardware before Release-publishing the input. Disable
    /// removes it from the CPU snapshot with AcqRel before writing hardware,
    /// so hard IRQ never observes a task-owned controller transition through
    /// a lock.
    ///
    /// This register-level operation leaves route validation to the caller.
    /// The CPU interface always excludes inputs without an effective route.
    pub fn set_enabled(&mut self, input: LioInput, enabled: bool) {
        let mask = 1u32 << input.raw();
        if enabled {
            self.registers().enable.set(mask);
            self.enabled.fetch_or(mask, Ordering::Release);
        } else {
            self.enabled.fetch_and(!mask, Ordering::AcqRel);
            self.registers().disable.set(mask);
        }
    }

    fn initialize(&self) {
        let registers = self.registers();
        for (raw, route) in registers.route.iter().enumerate() {
            // The loop bound guarantees construction succeeds.
            if let Ok(input) = LioInput::new(raw) {
                route.set(self.config.route_value(input));
            }
        }
        registers.disable.set(u32::MAX);
        registers.edge.set(0);
        // POL=0 selects active-high level inputs.
        registers.polarity.set(0);
    }

    fn registers(&self) -> &LioIntcRegisters {
        // SAFETY: `LioIntcParts::new` validates the complete register block and
        // its natural alignment. The caller's `MmioRaw` contract keeps the
        // mapping valid while this controller retains the capability handle.
        unsafe { &*self.regs.as_ptr().cast::<LioIntcRegisters>() }
    }
}

/// Shutdown-lifetime LIOINTC CPU interface used by hard IRQ dispatch.
#[derive(Debug)]
pub struct LioIntcCpuInterface {
    isr: MmioRaw,
    config: LioIntcConfig,
    enabled: Arc<AtomicU32>,
}

impl LioIntcCpuInterface {
    /// Returns whether `line` is one of this controller's parent cascades.
    pub fn is_parent(&self, line: CpuIrqLine) -> bool {
        self.config.effective_parent_input_map(line).is_some()
    }

    /// Claims the lowest pending enabled input for a matching parent line.
    pub fn claim(&self, line: CpuIrqLine) -> Option<LioInput> {
        let parent_input_map = self.config.effective_parent_input_map(line)?;
        // Acquire observes controller publication after the W1 enable write.
        let pending = self.registers().pending.get()
            & parent_input_map
            & self.enabled.load(Ordering::Acquire);
        if pending == 0 {
            return None;
        }
        LioInput::new(pending.trailing_zeros() as usize).ok()
    }

    /// Completes a level-triggered LIOINTC input.
    ///
    /// LIOINTC has no distinct hardware EOI register; the device handler
    /// deasserts the source. The typed argument documents which input reached
    /// the OS completion boundary.
    pub fn complete(&self, _input: LioInput) {}

    /// Returns the four INT0..INT3 parent-line slots.
    pub const fn parent_lines(&self) -> [Option<CpuIrqLine>; LIO_PARENT_COUNT] {
        self.config.parent_lines
    }

    fn registers(&self) -> &LioIntcIsrRegisters {
        // SAFETY: `LioIntcParts::new` validates the ISR register block and its
        // natural alignment. The caller's `MmioRaw` contract keeps the mapping
        // valid while this CPU interface retains the capability handle.
        unsafe { &*self.isr.as_ptr().cast::<LioIntcIsrRegisters>() }
    }
}
