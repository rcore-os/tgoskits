//! Optional `rdif-intc` capability implementations.

use rdif_intc::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, ControllerIrqTranslation,
    DriverGeneric, HwIrq, Interface, IrqError, IrqTranslation, Trigger,
};

use crate::{
    EioIntcController, EioVector, IntcError, IocsrAccess, LioInput, LioIntcController, PchInput,
    PchIrqPolarity, PchIrqTrigger, PchPicController,
};

impl<A: IocsrAccess> DriverGeneric for EioIntcController<A> {
    fn name(&self) -> &str {
        "Loongson EIOINTC"
    }
}

impl<A: IocsrAccess> Interface for EioIntcController<A> {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        let raw = first_specifier_cell("EIOINTC", irq_prop)?;
        let vector = EioVector::new(raw).map_err(map_core_error)?;
        ensure_index("EIOINTC vector", vector.raw(), self.vector_count())?;
        Ok(ControllerIrqTranslation::new(HwIrq(raw as u32)))
    }

    fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        let vector = EioVector::new(translation.id.hwirq.0 as usize).map_err(map_core_error)?;
        ensure_index("EIOINTC vector", vector.raw(), self.vector_count())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        let vector = EioVector::new(hwirq.0 as usize).map_err(map_core_error)?;
        EioIntcController::set_enabled(self, vector, enabled).map_err(map_core_error)
    }
}

impl DriverGeneric for PchPicController {
    fn name(&self) -> &str {
        "Loongson PCH-PIC"
    }
}

impl Interface for PchPicController {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        let raw = first_specifier_cell("PCH-PIC", irq_prop)?;
        let input = PchInput::new(raw).map_err(map_core_error)?;
        ensure_index("PCH-PIC input", input.raw(), self.config().input_count())?;
        Ok(ControllerIrqTranslation::new(HwIrq(raw as u32)))
    }

    fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        route.controller == AcpiGsiController::PchPic
            && route.controller_id == self.config().acpi_controller_id()
            && route.controller_address == self.controller_address()
            && usize::from(route.controller_input) < self.config().input_count()
    }

    fn translate_acpi(&self, route: &AcpiGsiRoute) -> Result<ControllerIrqTranslation, IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }
        Ok(ControllerIrqTranslation::new(HwIrq(u32::from(
            route.controller_input,
        ))))
    }

    fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        let input = checked_pch_input(self, translation.id.hwirq)?;
        let Some(trigger) = translation.trigger else {
            return Ok(());
        };
        let (trigger, polarity) = pch_config_from_trigger(trigger)?;
        self.configure_input(input, trigger, polarity)
            .map_err(map_core_error)
    }

    fn configure_acpi(
        &mut self,
        translation: &IrqTranslation,
        route: &AcpiGsiRoute,
    ) -> Result<(), IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::Unsupported);
        }
        let input = checked_pch_input(self, translation.id.hwirq)?;
        if input.raw() != usize::from(route.controller_input) {
            return Err(IrqError::InvalidIrq);
        }
        self.configure_input(
            input,
            match route.trigger {
                AcpiIrqTrigger::Edge => PchIrqTrigger::Edge,
                AcpiIrqTrigger::Level => PchIrqTrigger::Level,
            },
            match route.polarity {
                AcpiIrqPolarity::ActiveHigh => PchIrqPolarity::ActiveHigh,
                AcpiIrqPolarity::ActiveLow => PchIrqPolarity::ActiveLow,
            },
        )
        .map_err(map_core_error)
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        let input = checked_pch_input(self, hwirq)?;
        PchPicController::set_enabled(self, input, enabled).map_err(map_core_error)
    }
}

impl DriverGeneric for LioIntcController {
    fn name(&self) -> &str {
        "Loongson LS2K1000 LIOINTC"
    }
}

impl Interface for LioIntcController {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        let raw = first_specifier_cell("LIOINTC", irq_prop)?;
        let input = LioInput::new(raw).map_err(map_core_error)?;
        Ok(ControllerIrqTranslation::new(HwIrq(input.raw() as u32)))
    }

    fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        let _input = LioInput::new(translation.id.hwirq.0 as usize).map_err(map_core_error)?;
        match translation.trigger {
            None | Some(Trigger::LevelHigh) => Ok(()),
            Some(_) => Err(IrqError::Unsupported),
        }
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        let input = LioInput::new(hwirq.0 as usize).map_err(map_core_error)?;
        LioIntcController::set_enabled(self, input, enabled);
        Ok(())
    }
}

fn first_specifier_cell(controller: &'static str, irq_prop: &[u32]) -> Result<usize, IrqError> {
    irq_prop
        .first()
        .copied()
        .map(|value| value as usize)
        .ok_or(IntcError::EmptySpecifier { controller })
        .map_err(map_core_error)
}

fn checked_pch_input(controller: &PchPicController, hwirq: HwIrq) -> Result<PchInput, IrqError> {
    let input = PchInput::new(hwirq.0 as usize).map_err(map_core_error)?;
    ensure_index(
        "PCH-PIC input",
        input.raw(),
        controller.config().input_count(),
    )?;
    Ok(input)
}

fn ensure_index(kind: &'static str, index: usize, count: usize) -> Result<(), IrqError> {
    if index < count {
        Ok(())
    } else {
        Err(map_core_error(IntcError::OutsideConfiguredRange {
            kind,
            index,
            count,
        }))
    }
}

fn pch_config_from_trigger(trigger: Trigger) -> Result<(PchIrqTrigger, PchIrqPolarity), IrqError> {
    match trigger {
        Trigger::LevelHigh => Ok((PchIrqTrigger::Level, PchIrqPolarity::ActiveHigh)),
        Trigger::LevelLow => Ok((PchIrqTrigger::Level, PchIrqPolarity::ActiveLow)),
        Trigger::EdgeRising => Ok((PchIrqTrigger::Edge, PchIrqPolarity::ActiveHigh)),
        Trigger::EdgeFailling => Ok((PchIrqTrigger::Edge, PchIrqPolarity::ActiveLow)),
        Trigger::EdgeBoth => Err(IrqError::Unsupported),
    }
}

fn map_core_error(_error: IntcError) -> IrqError {
    IrqError::InvalidIrq
}
