//! Fixed-input host injector for the OpenRace realtime A/B experiment.

use core::time::Duration;

use axvm::{AxvmRuntime, PeriodicVirqConfig};

const VM_ID: usize = 2;
const VCPU_ID: usize = 0;
const SOFTWARE_VIRQS: [usize; 2] = [48, 49];
const PERIOD: Duration = Duration::from_millis(2);
const SAMPLES: usize = 300;
const INJECTOR_CPU_IDS: [usize; 2] = [0, 1];

/// Start the same two-stream injector in A and B.
pub(crate) fn start() {
    for (stream, (&vector, &injector_cpu_id)) in SOFTWARE_VIRQS
        .iter()
        .zip(INJECTOR_CPU_IDS.iter())
        .enumerate()
    {
        let config = PeriodicVirqConfig {
            vcpu_id: VCPU_ID,
            vector,
            period: PERIOD,
            samples: SAMPLES,
            injector_cpu_id: Some(injector_cpu_id),
        };
        match AxvmRuntime::start_periodic_virq_injector(VM_ID, config) {
            Ok(()) => info!(
                "OpenRace realtime injector armed stream={} vm={} vcpu={} vector={} \
                 period_ms={} samples={} injector_cpu={}",
                stream,
                VM_ID,
                VCPU_ID,
                vector,
                PERIOD.as_millis(),
                SAMPLES,
                injector_cpu_id,
            ),
            Err(err) => warn!(
                "OpenRace realtime injector stream={} was not armed: {err:?}",
                stream
            ),
        }
    }
}
