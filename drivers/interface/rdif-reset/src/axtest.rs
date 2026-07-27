use alloc::{vec, vec::Vec};

use axtest::prelude::*;

use crate::{DriverGeneric, Interface, Reset, ResetError, ResetId};

struct RecordingReset {
    calls: Vec<(&'static str, ResetId)>,
    fail_deassert: bool,
}

impl DriverGeneric for RecordingReset {
    fn name(&self) -> &str {
        "recording-reset"
    }
}

impl Interface for RecordingReset {
    fn assert(&mut self, id: ResetId) -> Result<(), ResetError> {
        self.calls.push(("assert", id));
        Ok(())
    }

    fn deassert(&mut self, id: ResetId) -> Result<(), ResetError> {
        self.calls.push(("deassert", id));
        if self.fail_deassert {
            Err(ResetError::Controller)
        } else {
            Ok(())
        }
    }
}

#[axtest]
fn rdif_reset_ids_wrapper_and_sequence_rules_hold() {
    ax_assert_eq!(ResetId::new(7).raw(), 7);
    ax_assert_eq!(ResetId::from(8_u32).raw(), 8);
    ax_assert_eq!(ResetId::from(9_usize).raw(), 9);
    ax_assert_eq!(ResetId::from(10_u64).raw(), 10);

    let mut reset = Reset::new(RecordingReset {
        calls: Vec::new(),
        fail_deassert: false,
    });
    ax_assert_eq!(reset.name(), "recording-reset");
    reset.reset(ResetId::new(3)).unwrap();
    ax_assert!(reset.typed_ref::<RecordingReset>().is_some());
    ax_assert!(reset.typed_mut::<RecordingReset>().is_some());
    ax_assert_eq!(
        reset.typed_ref::<RecordingReset>().unwrap().calls,
        vec![("assert", ResetId::new(3)), ("deassert", ResetId::new(3))]
    );
}

#[axtest]
fn rdif_reset_sequence_propagates_deassert_error() {
    let mut reset = RecordingReset {
        calls: Vec::new(),
        fail_deassert: true,
    };

    ax_assert_eq!(reset.reset(ResetId::new(4)), Err(ResetError::Controller));
    ax_assert_eq!(
        reset.calls,
        vec![("assert", ResetId::new(4)), ("deassert", ResetId::new(4))]
    );
}
