//! Linux perf event numbers are translated at the OS boundary.
use ax_cpu::pmu::EventSupport;

pub(super) fn event_supported(event: u16) -> bool {
    ax_hal::pmu::info().is_some_and(|info| info.event_support(event) != EventSupport::Unsupported)
}

pub(super) fn hardware_event(id: u32) -> Option<u16> {
    let info = ax_hal::pmu::info()?;
    let event = match id {
        0 => 0x11,
        1 => 0x08,
        2 => 0x04,
        3 => 0x03,
        4 => {
            if info.event_support(0x21) == EventSupport::Supported {
                0x21
            } else if info.event_support(0x0c) == EventSupport::Supported {
                0x0c
            } else {
                return None;
            }
        }
        5 => 0x10,
        6 => 0x1d,
        7 => 0x23,
        8 => 0x24,
        _ => return None,
    };
    (info.event_support(event) == EventSupport::Supported).then_some(event)
}
