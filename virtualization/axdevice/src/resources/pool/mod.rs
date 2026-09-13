//! Architecture-provided pools, fixed allowlists, and reservations.

mod address;
mod host_irq;
mod msi;
mod range;
mod wired;

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::ops::Range;

use axdevice_base::*;
pub(crate) use range::ranges_overlap;

/// Resources available to one architecture-owned VM plan.
///
/// Automatic pools, fixed allowlists, and reservations are independent so a
/// fixed machine ABI cannot accidentally enlarge an automatic pool.
#[derive(Clone, Debug, Default)]
pub struct ResourcePools {
    addresses: AddressPools,
    wired_irqs: WiredIrqPools,
    host_irqs: HostIrqPools,
    msi: MsiPools,
}

#[derive(Clone, Debug, Default)]
struct AddressPools {
    automatic: AddressRanges,
    fixed: AddressRanges,
    reserved: AddressReservations,
}

#[derive(Clone, Debug, Default)]
struct AddressRanges {
    mmio: Vec<Range<u64>>,
    pio: Vec<Range<u16>>,
}

#[derive(Clone, Debug, Default)]
struct AddressReservations {
    mmio: Vec<RangeOwner<u64>>,
    pio: Vec<RangeOwner<u16>>,
}

#[derive(Clone, Debug, Default)]
struct WiredIrqPools {
    automatic: BTreeMap<InterruptControllerId, Vec<Range<usize>>>,
    fixed: BTreeMap<InterruptControllerId, Vec<Range<usize>>>,
    reserved: BTreeMap<InterruptControllerId, Vec<IrqOwner>>,
}

#[derive(Clone, Debug, Default)]
struct HostIrqPools {
    automatic: Vec<Range<usize>>,
    fixed: Vec<Range<usize>>,
    reserved: Vec<RangeOwner<usize>>,
}

#[derive(Clone, Debug, Default)]
struct MsiPools {
    automatic: MsiRanges,
    fixed: MsiRanges,
    reserved: MsiReservations,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MsiRanges {
    pub(crate) devices: BTreeMap<(InterruptControllerId, ItsId), Vec<Range<u32>>>,
    pub(crate) events: BTreeMap<(InterruptControllerId, ItsId), Vec<Range<u32>>>,
    pub(crate) lpis: BTreeMap<InterruptControllerId, Vec<Range<u32>>>,
}

#[derive(Clone, Debug, Default)]
struct MsiReservations {
    devices: BTreeMap<(InterruptControllerId, ItsId), Vec<RangeOwner<u32>>>,
    events: BTreeMap<(InterruptControllerId, ItsId, MsiDeviceId), Vec<RangeOwner<u32>>>,
    lpis: BTreeMap<InterruptControllerId, Vec<RangeOwner<u32>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct RangeOwner<T> {
    pub(crate) range: Range<T>,
    pub(crate) owner: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IrqOwner {
    pub(crate) input: ControllerInputId,
    pub(crate) trigger: InterruptTrigger,
    pub(crate) sharing: InterruptSharing,
    pub(crate) owner: String,
}

impl ResourcePools {
    /// Creates empty pools for one VM.
    pub const fn new() -> Self {
        Self {
            addresses: AddressPools {
                automatic: AddressRanges {
                    mmio: Vec::new(),
                    pio: Vec::new(),
                },
                fixed: AddressRanges {
                    mmio: Vec::new(),
                    pio: Vec::new(),
                },
                reserved: AddressReservations {
                    mmio: Vec::new(),
                    pio: Vec::new(),
                },
            },
            wired_irqs: WiredIrqPools {
                automatic: BTreeMap::new(),
                fixed: BTreeMap::new(),
                reserved: BTreeMap::new(),
            },
            host_irqs: HostIrqPools {
                automatic: Vec::new(),
                fixed: Vec::new(),
                reserved: Vec::new(),
            },
            msi: MsiPools {
                automatic: MsiRanges {
                    devices: BTreeMap::new(),
                    events: BTreeMap::new(),
                    lpis: BTreeMap::new(),
                },
                fixed: MsiRanges {
                    devices: BTreeMap::new(),
                    events: BTreeMap::new(),
                    lpis: BTreeMap::new(),
                },
                reserved: MsiReservations {
                    devices: BTreeMap::new(),
                    events: BTreeMap::new(),
                    lpis: BTreeMap::new(),
                },
            },
        }
    }

    /// Atomically reserves one physical wired interrupt at both boundaries.
    pub fn reserve_wired_host_irq(
        &mut self,
        owner: impl Into<String>,
        controller: InterruptControllerId,
        input: ControllerInputId,
        host_irq: HostIrqId,
        trigger: InterruptTrigger,
    ) -> crate::DeviceManagerResult {
        let owner = owner.into();
        let mut updated = self.clone();
        updated.reserve_controller_input(
            owner.clone(),
            controller,
            input,
            trigger,
            InterruptSharing::Exclusive,
        )?;
        updated.reserve_host_irq(owner, host_irq)?;
        *self = updated;
        Ok(())
    }
}
