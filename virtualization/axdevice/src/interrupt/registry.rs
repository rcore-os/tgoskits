//! Internal controller registry and cascade-graph validation.

use alloc::{format, vec, vec::Vec};

use axdevice_base::{ControllerInputId, InterruptControllerId, WiredIrqInput};

use super::{ControllerRef, ControllerRegistration, ControllerRole};
use crate::{DeviceManagerError, DeviceManagerResult};

pub(super) struct ControllerEntry {
    pub(super) registration: ControllerRegistration,
    pub(super) wired_inputs: Vec<(ControllerInputId, WiredIrqInput)>,
}

pub(super) fn registration_order(
    controllers: &[ControllerEntry],
) -> DeviceManagerResult<Vec<InterruptControllerId>> {
    let mut parent_indices = vec![None; controllers.len()];
    for (child_index, entry) in controllers.iter().enumerate() {
        let Some(cascade) = entry.registration.cascade() else {
            continue;
        };
        let parent_id = resolve_controller(controllers, cascade.parent().controller())?;
        parent_indices[child_index] = Some(controller_index(controllers, parent_id)?);
    }

    let mut indegree = vec![0usize; controllers.len()];
    for (child, parent) in parent_indices.iter().enumerate() {
        if parent.is_some() {
            indegree[child] = 1;
        }
    }
    let mut ready: Vec<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(controllers.len());
    while let Some(parent) = ready.pop() {
        order.push(controllers[parent].registration.id());
        for (child, child_parent) in parent_indices.iter().enumerate() {
            if *child_parent == Some(parent) {
                indegree[child] -= 1;
                if indegree[child] == 0 {
                    ready.push(child);
                }
            }
        }
    }
    if order.len() != controllers.len() {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "finalize interrupt topology",
            detail: "interrupt-controller cascade contains a cycle".into(),
        });
    }
    Ok(order)
}

pub(super) fn resolve_controller(
    controllers: &[ControllerEntry],
    reference: ControllerRef,
) -> DeviceManagerResult<InterruptControllerId> {
    match reference {
        ControllerRef::Id(id) => {
            find_controller(controllers, id)?;
            Ok(id)
        }
        ControllerRef::Default => controllers
            .iter()
            .find(|entry| entry.registration.role() == ControllerRole::Default)
            .map(|entry| entry.registration.id())
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation: "resolve default interrupt controller",
                resource: "default interrupt controller".into(),
            }),
    }
}

pub(super) fn find_controller(
    controllers: &[ControllerEntry],
    id: InterruptControllerId,
) -> DeviceManagerResult<&ControllerEntry> {
    controller_index(controllers, id).map(|index| &controllers[index])
}

pub(super) fn find_controller_mut(
    controllers: &mut [ControllerEntry],
    id: InterruptControllerId,
) -> DeviceManagerResult<&mut ControllerEntry> {
    let index = controller_index(controllers, id)?;
    Ok(&mut controllers[index])
}

fn controller_index(
    controllers: &[ControllerEntry],
    id: InterruptControllerId,
) -> DeviceManagerResult<usize> {
    controllers
        .iter()
        .position(|entry| entry.registration.id() == id)
        .ok_or_else(|| DeviceManagerError::ResourceNotFound {
            operation: "resolve interrupt controller",
            resource: format!("interrupt controller {id:?}"),
        })
}
