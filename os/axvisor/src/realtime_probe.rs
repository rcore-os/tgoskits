//! Fixed-input host injector for the OpenRace realtime A/B experiment.

use core::time::Duration;

use axvm::{AxvmRuntime, PeriodicVirqConfig};

const VM_ID: usize = 2;
const VCPU_ID: usize = 0;
const SOFTWARE_VIRQ: usize = 48;
const PERIOD: Duration = Duration::from_millis(10);
const SAMPLES: usize = 300;
const INJECTOR_CPU_ID: usize = 0;

/// Start the exact same bounded injector in A and B.
pub(crate) fn start() {
    let config = PeriodicVirqConfig {
        vcpu_id: VCPU_ID,
        vector: SOFTWARE_VIRQ,
        period: PERIOD,
        samples: SAMPLES,
        injector_cpu_id: Some(INJECTOR_CPU_ID),
    };
    match AxvmRuntime::start_periodic_virq_injector(VM_ID, config) {
        Ok(()) => info!(
            "OpenRace realtime injector armed vm={} vcpu={} vector={} period_ms={} samples={} \
             injector_cpu={}",
            VM_ID,
            VCPU_ID,
            SOFTWARE_VIRQ,
            PERIOD.as_millis(),
            SAMPLES,
            INJECTOR_CPU_ID,
        ),
        Err(err) => warn!("OpenRace realtime injector was not armed: {err:?}"),
    }
}
