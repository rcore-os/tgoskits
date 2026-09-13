use sdhci_host::{HostTimer, Sdhci};

struct AxKlibHostTimer;

static HOST_TIMER: AxKlibHostTimer = AxKlibHostTimer;

impl HostTimer for AxKlibHostTimer {
    fn now_ms(&self) -> u64 {
        self.now_ns() / 1_000_000
    }

    fn now_ns(&self) -> u64 {
        axklib::time::monotonic_nanos()
    }
}

pub(super) fn install_host_timer(host: &mut Sdhci) {
    host.set_timer(&HOST_TIMER);
}
