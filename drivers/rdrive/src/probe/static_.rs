use alloc::{collections::btree_set::BTreeSet, vec::Vec};

use spin::{Mutex, Once};

use crate::{
    Descriptor, PlatformDevice,
    error::DriverError,
    probe::{OnProbeError, ProbeError},
    register::{DriverRegister, ProbeKind},
};

static SYSTEM: Once<System> = Once::new();

pub type FnOnProbe = fn(plat_dev: PlatformDevice) -> Result<(), OnProbeError>;

pub fn init() -> Result<(), DriverError> {
    SYSTEM.call_once(System::new);
    Ok(())
}

pub(crate) fn try_probe_register(
    register: &DriverRegister,
) -> Option<Result<Vec<Result<(), OnProbeError>>, ProbeError>> {
    SYSTEM.get().map(|system| system.probe_register(register))
}

struct System {
    probed_names: Mutex<BTreeSet<&'static str>>,
}

impl System {
    fn new() -> Self {
        Self {
            probed_names: Mutex::new(BTreeSet::new()),
        }
    }

    fn probe_register(
        &self,
        register: &DriverRegister,
    ) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
        let mut out = Vec::new();
        for probe in register.probe_kinds {
            let ProbeKind::Static { on_probe } = probe else {
                continue;
            };
            out.push(self.probe_one(register, *on_probe));
        }

        Ok(out)
    }

    fn probe_one(
        &self,
        register: &DriverRegister,
        on_probe: FnOnProbe,
    ) -> Result<(), OnProbeError> {
        if self.probed_names.lock().contains(&register.name) {
            return Err(OnProbeError::NotMatch);
        }

        let mut descriptor = Descriptor::new();
        descriptor.name = register.name;
        let res = on_probe(PlatformDevice::new(descriptor));

        if res.is_ok() {
            self.probed_names.lock().insert(register.name);
        }

        res
    }
}
