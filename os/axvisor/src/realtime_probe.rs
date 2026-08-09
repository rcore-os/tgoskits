//! Fixed-input host injector for the OpenRace realtime A/B experiment.

use core::time::Duration;

use axvm::{AxvmRuntime, PeriodicVirqConfig};

const VM_ID: usize = 2;
/// E1 mode: single injector targeting vCPU1 (cross-vCPU spurious-wake test).
/// Standard mode keeps the dual-stream vCPU0 A/B scenario.
const E1_MODE: bool = true;
const VCPU_ID: usize = if E1_MODE { 1 } else { 0 };
const SOFTWARE_VIRQS: [usize; 2] = [48, 49];
/// E1 uses a quieter 10 ms period so the parked-vCPU run keeps up and avoids
/// the rare list-register saturation race; the standard scenario keeps 2 ms.
const PERIOD: Duration = Duration::from_millis(if E1_MODE { 10 } else { 2 });
const SAMPLES: usize = 300;
const INJECTOR_CPU_IDS: [usize; 2] = [0, 1];

/// Start the same two-stream injector in A and B.
pub(crate) fn start() {
    let stream_count = if E1_MODE { 1 } else { 2 };
    for (stream, (&vector, &injector_cpu_id)) in SOFTWARE_VIRQS
        .iter()
        .zip(INJECTOR_CPU_IDS.iter())
        .take(stream_count)
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
