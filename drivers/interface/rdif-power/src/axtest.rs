use alloc::{vec, vec::Vec};

use axtest::prelude::*;

use crate::{DriverGeneric, Interface, Power, PowerDomainId, PowerError};

struct RecordingPower {
    powered: bool,
    calls: Vec<(&'static str, PowerDomainId)>,
}

impl DriverGeneric for RecordingPower {
    fn name(&self) -> &str {
        "recording-power"
    }
}

impl Interface for RecordingPower {
    fn power_on(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
        self.powered = true;
        self.calls.push(("on", id));
        Ok(())
    }

    fn power_off(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
        self.powered = false;
        self.calls.push(("off", id));
        Ok(())
    }

    fn is_powered(&self, _id: PowerDomainId) -> Result<bool, PowerError> {
        Ok(self.powered)
    }
}

#[axtest]
fn rdif_power_ids_wrapper_and_dispatch_rules_hold() {
    ax_assert_eq!(PowerDomainId::new(7).raw(), 7);
    ax_assert_eq!(PowerDomainId::from(8_u32).raw(), 8);
    ax_assert_eq!(PowerDomainId::from(9_usize).raw(), 9);
    ax_assert_eq!(PowerDomainId::from(10_u64).raw(), 10);

    let mut power = Power::new(RecordingPower {
        powered: false,
        calls: Vec::new(),
    });
    ax_assert_eq!(power.name(), "recording-power");
    power.power_on(PowerDomainId::new(3)).unwrap();
    ax_assert_eq!(power.is_powered(PowerDomainId::new(3)), Ok(true));
    power.power_off(PowerDomainId::new(3)).unwrap();
    ax_assert_eq!(power.is_powered(PowerDomainId::new(3)), Ok(false));
    ax_assert!(power.typed_mut::<RecordingPower>().is_some());
    ax_assert_eq!(
        power.typed_ref::<RecordingPower>().unwrap().calls,
        vec![
            ("on", PowerDomainId::new(3)),
            ("off", PowerDomainId::new(3))
        ]
    );
}

#[axtest]
fn rdif_power_default_is_powered_reports_unsupported() {
    struct MinimalPower;

    impl DriverGeneric for MinimalPower {
        fn name(&self) -> &str {
            "minimal-power"
        }
    }

    impl Interface for MinimalPower {
        fn power_on(&mut self, _id: PowerDomainId) -> Result<(), PowerError> {
            Ok(())
        }

        fn power_off(&mut self, _id: PowerDomainId) -> Result<(), PowerError> {
            Ok(())
        }
    }

    let power = MinimalPower;
    ax_assert_eq!(
        power.is_powered(PowerDomainId::new(1)),
        Err(PowerError::Unsupported)
    );
}
