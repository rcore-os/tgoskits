use rdif_serial::{
    IrqEndpoint, IrqSourceControl, SerialCore, SerialIrqEvent, SerialIrqFault, SerialRearmError,
};

const PORTABLE_SOURCE: &str = concat!(
    include_str!("../src/core.rs"),
    include_str!("../src/lib.rs"),
    include_str!("../src/queue.rs"),
    include_str!("../src/raw.rs"),
    include_str!("../src/types.rs"),
);
const AX_DRIVER_SERIAL_SOURCE: &str = include_str!("../../../ax-driver/src/serial/mod.rs");

fn assert_irq_contract<T>()
where
    T: IrqEndpoint<Event = SerialIrqEvent, Fault = SerialIrqFault>
        + IrqSourceControl<Error = SerialRearmError>,
{
}

#[test]
fn serial_core_is_the_unique_capture_and_rearm_owner() {
    assert_irq_contract::<SerialCore<8, 8>>();
    assert!(AX_DRIVER_SERIAL_SOURCE.contains("pub core: SerialCore"));
}

#[test]
fn portable_serial_has_no_os_execution_or_shared_core_contract() {
    for forbidden in [
        "SpinNoIrq",
        "Arc<SerialCore",
        "SerialRuntimePort",
        "OwnerId",
        "run_on_owner",
        "Deferred",
        "deferred",
        "RawWaker",
        "Waker",
        "workqueue",
        "thread::",
    ] {
        assert!(
            !PORTABLE_SOURCE.contains(forbidden) && !AX_DRIVER_SERIAL_SOURCE.contains(forbidden),
            "serial ownership boundary still contains `{forbidden}`"
        );
    }
}
