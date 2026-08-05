use axtest::prelude::*;

use crate::{
    CpuId, KError,
    irq::{
        AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, IrqConfig, IrqId, Trigger,
    },
};

#[axtest]
fn rdif_def_custom_types_round_trip_and_format_raw_values() {
    let cpu = CpuId::from(0x2a_usize);
    let irq = IrqId::from(0x31_usize);

    ax_assert_eq!(cpu.raw(), 0x2a);
    ax_assert_eq!(usize::from(cpu), 0x2a);
    ax_assert_eq!(irq.raw(), 0x31);
    ax_assert_eq!(alloc::format!("{cpu:?}"), "0x2a");
    ax_assert_eq!(alloc::format!("{irq:?}"), "0x31");
}

#[axtest]
fn rdif_def_errors_and_irq_configs_are_matchable() {
    let config = IrqConfig {
        irq: IrqId::from(8),
        trigger: Trigger::LevelLow,
        is_private: true,
    };

    ax_assert_eq!(config.irq.raw(), 8);
    ax_assert_eq!(config.trigger, Trigger::LevelLow);
    ax_assert!(config.is_private);
    ax_assert_eq!(
        alloc::format!("{}", KError::InvalidArg { name: "irq" }),
        "Invalid Argument `irq`"
    );
    ax_assert_eq!(KError::BadAddr(0x1000), KError::BadAddr(0x1000));
}

#[axtest]
fn rdif_def_acpi_gsi_route_keeps_controller_metadata() {
    let route = AcpiGsiRoute {
        gsi: 32,
        vector: 0x90,
        controller: AcpiGsiController::IoApic,
        controller_id: 1,
        controller_address: 0xfec0_0000,
        controller_input: 2,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    };

    ax_assert_eq!(route.gsi, 32);
    ax_assert_eq!(route.vector, 0x90);
    ax_assert_eq!(route.controller, AcpiGsiController::IoApic);
    ax_assert_eq!(route.trigger, AcpiIrqTrigger::Level);
    ax_assert_eq!(route.polarity, AcpiIrqPolarity::ActiveLow);
}
