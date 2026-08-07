//! ITS DeviceID/EventID and controller-global LPI pools.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::ops::Range;

use axdevice_base::*;

use super::{range::*, *};
use crate::DeviceManagerResult;

impl ResourcePools {
    /// Adds one ITS domain used only for automatic MSI allocation.
    pub fn add_auto_msi_domain(
        &mut self,
        controller: InterruptControllerId,
        its: ItsId,
        devices: Range<MsiDeviceId>,
        events: Range<MsiEventId>,
        lpis: Range<LpiId>,
    ) -> DeviceManagerResult {
        add_msi_ranges(
            &mut self.msi.automatic,
            controller,
            its,
            devices,
            events,
            lpis,
            "automatic MSI",
        )
    }

    /// Allows fixed DeviceID/EventID/LPI requests in one ITS domain.
    pub fn allow_fixed_msi_domain(
        &mut self,
        controller: InterruptControllerId,
        its: ItsId,
        devices: Range<MsiDeviceId>,
        events: Range<MsiEventId>,
        lpis: Range<LpiId>,
    ) -> DeviceManagerResult {
        add_msi_ranges(
            &mut self.msi.fixed,
            controller,
            its,
            devices,
            events,
            lpis,
            "fixed MSI",
        )
    }

    /// Reserves a DeviceID range in one ITS namespace.
    pub fn reserve_msi_devices(
        &mut self,
        owner: impl Into<String>,
        controller: InterruptControllerId,
        its: ItsId,
        devices: Range<MsiDeviceId>,
    ) -> DeviceManagerResult {
        reserve_range(
            self.msi
                .reserved
                .devices
                .entry((controller, its))
                .or_default(),
            nonempty_owner(owner.into())?,
            devices.start.value()..devices.end.value(),
            "MSI DeviceID",
        )
    }

    /// Reserves an EventID range for one DeviceID in one ITS namespace.
    pub fn reserve_msi_events(
        &mut self,
        owner: impl Into<String>,
        controller: InterruptControllerId,
        its: ItsId,
        device: MsiDeviceId,
        events: Range<MsiEventId>,
    ) -> DeviceManagerResult {
        reserve_range(
            self.msi
                .reserved
                .events
                .entry((controller, its, device))
                .or_default(),
            nonempty_owner(owner.into())?,
            events.start.value()..events.end.value(),
            "MSI EventID",
        )
    }

    /// Reserves an LPI range in one controller namespace.
    pub fn reserve_lpis(
        &mut self,
        owner: impl Into<String>,
        controller: InterruptControllerId,
        lpis: Range<LpiId>,
    ) -> DeviceManagerResult {
        reserve_range(
            self.msi.reserved.lpis.entry(controller).or_default(),
            nonempty_owner(owner.into())?,
            lpis.start.value()..lpis.end.value(),
            "LPI",
        )
    }

    pub(crate) const fn auto_msi(&self) -> &MsiRanges {
        &self.msi.automatic
    }

    pub(crate) const fn fixed_msi(&self) -> &MsiRanges {
        &self.msi.fixed
    }

    pub(crate) fn reserved_msi_devices(
        &self,
    ) -> &BTreeMap<(InterruptControllerId, ItsId), Vec<RangeOwner<u32>>> {
        &self.msi.reserved.devices
    }

    pub(crate) fn reserved_msi_events(
        &self,
    ) -> &BTreeMap<(InterruptControllerId, ItsId, MsiDeviceId), Vec<RangeOwner<u32>>> {
        &self.msi.reserved.events
    }

    pub(crate) fn reserved_lpis(&self) -> &BTreeMap<InterruptControllerId, Vec<RangeOwner<u32>>> {
        &self.msi.reserved.lpis
    }
}

fn add_msi_ranges(
    ranges: &mut MsiRanges,
    controller: InterruptControllerId,
    its: ItsId,
    devices: Range<MsiDeviceId>,
    events: Range<MsiEventId>,
    lpis: Range<LpiId>,
    kind: &'static str,
) -> DeviceManagerResult {
    let domain = (controller, its);
    insert_range(
        ranges.devices.entry(domain).or_default(),
        devices.start.value()..devices.end.value(),
        kind,
    )?;
    insert_range(
        ranges.events.entry(domain).or_default(),
        events.start.value()..events.end.value(),
        kind,
    )?;
    insert_range(
        ranges.lpis.entry(controller).or_default(),
        lpis.start.value()..lpis.end.value(),
        kind,
    )
}
