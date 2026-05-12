pub use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

pub use rdif_base::irq::IrqConfig;

use crate::custom_id;

custom_id!(DeviceId, u64);
custom_id!(DriverId, u64);

#[derive(Default, Debug, Clone)]
pub struct Descriptor {
    pub(crate) device_id: DeviceId,
    pub name: &'static str,
    pub irq_parent: Option<DeviceId>,
    // pub irqs: Vec<IrqConfig>,
}

impl Descriptor {
    pub fn new() -> Self {
        Self {
            device_id: DeviceId::new(),
            ..Default::default()
        }
    }
}

impl Descriptor {
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }
}

static ITER: AtomicU64 = AtomicU64::new(0);

impl DeviceId {
    pub fn new() -> Self {
        Self(ITER.fetch_add(1, Ordering::SeqCst))
    }
}

macro_rules! impl_driver_id_for {
    ($t:ty) => {
        impl From<$t> for DriverId {
            fn from(value: $t) -> Self {
                Self(value as _)
            }
        }
    };
}

impl_driver_id_for!(usize);
impl_driver_id_for!(u32);
