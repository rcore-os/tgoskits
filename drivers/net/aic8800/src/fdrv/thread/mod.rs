//! Owner-CPU Wi-Fi queue progression.

pub mod ap;
pub mod rx;
pub mod tx;

use crate::fdrv::core::bus::WifiBus;

/// Advances command/data TX and card RX without running AP command handlers.
///
/// Control-command waits use this function so their CFM can be received by the
/// same queue executor without recursively entering another control command.
pub fn progress_io(bus: &WifiBus) -> bool {
    let tx = tx::tx_process(bus);
    let rx = rx::process_rx_frames(bus, 64) != 0;
    tx || rx
}
