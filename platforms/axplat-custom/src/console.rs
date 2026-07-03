use ax_plat::console::{ConsoleDeviceIdError, ConsoleDeviceIdResult, ConsoleIf};

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    fn write_bytes(_bytes: &[u8]) {}

    fn read_bytes(_bytes: &mut [u8]) -> usize {
        0
    }

    fn device_id() -> ConsoleDeviceIdResult {
        Err(ConsoleDeviceIdError::NotSpecified)
    }

    fn claim_runtime_output() {}

    #[cfg(feature = "irq")]
    fn irq_num() -> Option<ax_plat::irq::IrqId> {
        None
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(_enabled: bool) {}

    #[cfg(feature = "irq")]
    fn handle_irq() -> ax_plat::console::ConsoleIrqEvent {
        ax_plat::console::ConsoleIrqEvent::empty()
    }
}
