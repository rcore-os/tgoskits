use rdif_block::{BlockController, ControllerEvent, ControllerState};

use super::*;
use crate::rdif::{BlockConfig, BlockDevice};

#[test]
fn controller_teardown_is_idempotent_after_watchdog_shutdown() {
    let host = MockHost::new(Vec::new());
    let config = BlockConfig::dma("sdmmc-test", 1, test_device_dma());
    let mut controller = BlockDevice::new(SdioSdmmc::new(host), config);

    let start = controller
        .advance(ControllerEvent::Start { target_queues: 1 })
        .unwrap();
    assert_eq!(start.controller_state(), ControllerState::Ready);
    assert_eq!(
        controller
            .advance(ControllerEvent::Watchdog { queue_id: 0 })
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );

    assert_eq!(
        controller
            .advance(ControllerEvent::QuiesceIrqs)
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );
    assert_eq!(
        controller
            .advance(ControllerEvent::Shutdown)
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );
}
