//! Wired interrupt-controller input pools.

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use core::ops::Range;

use axdevice_base::*;

use super::{range::*, *};
use crate::{DeviceManagerError, DeviceManagerResult};

impl ResourcePools {
    /// Adds controller inputs used only for automatic allocation.
    pub fn add_auto_controller_inputs(
        &mut self,
        controller: InterruptControllerId,
        range: Range<ControllerInputId>,
    ) -> DeviceManagerResult {
        insert_input_range(
            &mut self.wired_irqs.automatic,
            controller,
            range,
            "automatic IRQ",
        )
    }

    /// Allows fixed requests for controller inputs inside `range`.
    pub fn allow_fixed_controller_inputs(
        &mut self,
        controller: InterruptControllerId,
        range: Range<ControllerInputId>,
    ) -> DeviceManagerResult {
        insert_input_range(&mut self.wired_irqs.fixed, controller, range, "fixed IRQ")
    }

    /// Reserves one input for architecture or controller internals.
    pub fn reserve_controller_input(
        &mut self,
        owner: impl Into<String>,
        controller: InterruptControllerId,
        input: ControllerInputId,
        trigger: InterruptTrigger,
        sharing: InterruptSharing,
    ) -> DeviceManagerResult {
        let owner = nonempty_owner(owner.into())?;
        let owners = self.wired_irqs.reserved.entry(controller).or_default();
        if let Some(existing) = owners.iter().find(|existing| existing.input == input) {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "reserve controller input",
                detail: format!(
                    "controller {} input {} is already reserved by {}",
                    controller.value(),
                    input.value(),
                    existing.owner
                ),
            });
        }
        owners.push(IrqOwner {
            input,
            trigger,
            sharing,
            owner,
        });
        Ok(())
    }

    pub(crate) fn auto_inputs(&self, controller: InterruptControllerId) -> Option<&[Range<usize>]> {
        self.wired_irqs
            .automatic
            .get(&controller)
            .map(Vec::as_slice)
    }

    pub(crate) fn fixed_inputs(
        &self,
        controller: InterruptControllerId,
    ) -> Option<&[Range<usize>]> {
        self.wired_irqs.fixed.get(&controller).map(Vec::as_slice)
    }

    pub(crate) fn reserved_inputs(&self) -> &BTreeMap<InterruptControllerId, Vec<IrqOwner>> {
        &self.wired_irqs.reserved
    }
}

fn insert_input_range(
    ranges: &mut BTreeMap<InterruptControllerId, Vec<Range<usize>>>,
    controller: InterruptControllerId,
    range: Range<ControllerInputId>,
    kind: &'static str,
) -> DeviceManagerResult {
    insert_range(
        ranges.entry(controller).or_default(),
        range.start.value()..range.end.value(),
        kind,
    )
}
