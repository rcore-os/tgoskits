//! Transaction boundary for claiming one enabled per-CPU timer interrupt.

/// Host operations needed to configure and claim one per-CPU interrupt.
pub(crate) trait PerCpuIrqControl {
    type Claim;
    type Error;

    /// Configures the interrupt as level-triggered on one target CPU.
    fn configure_level(&mut self, cpu_id: usize, hwirq: u32) -> Result<(), Self::Error>;

    /// Exclusively claims and enables the interrupt on all target CPUs.
    fn request_enabled(
        &mut self,
        hwirq: u32,
        cpu_ids: &[usize],
    ) -> Result<Self::Claim, Self::Error>;
}

/// Configures every target CPU before publishing one enabled IRQ claim.
///
/// Level configuration is intentionally completed first. This keeps a failed
/// setup from exposing a partially configured interrupt through the host IRQ
/// registry.
pub(crate) fn claim_enabled_percpu_irq<C: PerCpuIrqControl>(
    control: &mut C,
    hwirq: u32,
    cpu_ids: &[usize],
) -> Result<C::Claim, C::Error> {
    for &cpu_id in cpu_ids {
        control.configure_level(cpu_id, hwirq)?;
    }
    control.request_enabled(hwirq, cpu_ids)
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::{PerCpuIrqControl, claim_enabled_percpu_irq};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Operation {
        Configure { cpu_id: usize, hwirq: u32 },
        Claim { hwirq: u32, cpu_count: usize },
    }

    #[derive(Default)]
    struct RecordingControl {
        operations: Vec<Operation>,
        fail_configure_cpu: Option<usize>,
    }

    impl PerCpuIrqControl for RecordingControl {
        type Claim = u32;
        type Error = &'static str;

        fn configure_level(&mut self, cpu_id: usize, hwirq: u32) -> Result<(), Self::Error> {
            self.operations.push(Operation::Configure { cpu_id, hwirq });
            if self.fail_configure_cpu == Some(cpu_id) {
                return Err("configure failed");
            }
            Ok(())
        }

        fn request_enabled(
            &mut self,
            hwirq: u32,
            cpu_ids: &[usize],
        ) -> Result<Self::Claim, Self::Error> {
            self.operations.push(Operation::Claim {
                hwirq,
                cpu_count: cpu_ids.len(),
            });
            Ok(hwirq)
        }
    }

    #[test]
    fn configures_every_target_before_claiming_and_enabling_the_ppi() {
        let mut control = RecordingControl::default();

        let claim = claim_enabled_percpu_irq(&mut control, 27, &[0, 2, 3]).unwrap();

        assert_eq!(claim, 27);
        assert_eq!(
            control.operations,
            [
                Operation::Configure {
                    cpu_id: 0,
                    hwirq: 27,
                },
                Operation::Configure {
                    cpu_id: 2,
                    hwirq: 27,
                },
                Operation::Configure {
                    cpu_id: 3,
                    hwirq: 27,
                },
                Operation::Claim {
                    hwirq: 27,
                    cpu_count: 3,
                },
            ]
        );
    }

    #[test]
    fn configuration_failure_does_not_publish_an_irq_claim() {
        let mut control = RecordingControl {
            fail_configure_cpu: Some(2),
            ..RecordingControl::default()
        };

        assert_eq!(
            claim_enabled_percpu_irq(&mut control, 27, &[0, 2, 3]),
            Err("configure failed")
        );
        assert_eq!(
            control.operations,
            [
                Operation::Configure {
                    cpu_id: 0,
                    hwirq: 27,
                },
                Operation::Configure {
                    cpu_id: 2,
                    hwirq: 27,
                },
            ]
        );
    }
}
