use axvisor_api::arch::ArchIf;

struct ArchImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchImpl {
    fn host_tsc_frequency_mhz() -> Option<u32> {
        u32::try_from(ax_hal::time::nanos_to_ticks(1_000))
            .ok()
            .filter(|&freq| freq > 0)
    }
}
