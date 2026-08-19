extern crate alloc;

use alloc::{vec, vec::Vec};

use rdif_reset::{DriverGeneric, Interface, Reset, ResetError, ResetId};

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

#[test]
fn rdif_reset_ids_wrapper_and_sequence_rules_hold() {
    assert_eq!(ResetId::new(7).raw(), 7);
    assert_eq!(ResetId::from(8_u32).raw(), 8);
    assert_eq!(ResetId::from(9_usize).raw(), 9);
    assert_eq!(ResetId::from(10_u64).raw(), 10);

    let mut reset = Reset::new(RecordingReset {
        calls: Vec::new(),
        fail_deassert: false,
    });
    assert_eq!(reset.name(), "recording-reset");
    reset.reset(ResetId::new(3)).unwrap();
    assert!(reset.typed_ref::<RecordingReset>().is_some());
    assert!(reset.typed_mut::<RecordingReset>().is_some());
    assert_eq!(
        reset.typed_ref::<RecordingReset>().unwrap().calls,
        vec![("assert", ResetId::new(3)), ("deassert", ResetId::new(3))]
    );
}

#[test]
fn rdif_reset_sequence_propagates_deassert_error() {
    let mut reset = RecordingReset {
        calls: Vec::new(),
        fail_deassert: true,
    };

    assert_eq!(reset.reset(ResetId::new(4)), Err(ResetError::Controller));
    assert_eq!(
        reset.calls,
        vec![("assert", ResetId::new(4)), ("deassert", ResetId::new(4))]
    );
}
