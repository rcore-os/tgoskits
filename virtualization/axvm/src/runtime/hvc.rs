// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{format, string::ToString};

use axhvc::{HyperCallCode, HyperCallError, HyperCallResult};

use crate::{
    runtime::{ivc::*, *},
    *,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HyperCallAbi {
    Generic,
    AArch64,
}

fn is_psci_code(code: HyperCallCode) -> bool {
    matches!(
        code,
        HyperCallCode::PSCIVersion
            | HyperCallCode::PSCIFeatures
            | HyperCallCode::PSCICpuSuspend
            | HyperCallCode::PSCICpuSuspend64
            | HyperCallCode::PSCICpuOff
            | HyperCallCode::PSCICpuOn
            | HyperCallCode::PSCICpuOn64
            | HyperCallCode::PSCIAffinityInfo
            | HyperCallCode::PSCIAffinityInfo64
            | HyperCallCode::PSCIMigrate
            | HyperCallCode::PSCIMigrate64
            | HyperCallCode::PSCIMigrateInfoType
            | HyperCallCode::PSCIMigrateInfoUpCpu
            | HyperCallCode::PSCIMigrateInfoUpCpu64
            | HyperCallCode::PSCISystemOff
            | HyperCallCode::PSCISystemReset
    )
}
const PSCI_RET_SUCCESS: usize = 0;
const PSCI_VERSION_0_2: usize = 0x0000_0002;
const ARM_SMCCC_VERSION_FUNC_ID: u64 = 0x8000_0000;
const PSCI_RET_NOT_SUPPORTED: usize = usize::MAX;
const PSCI_RET_INVALID_PARAMETERS: usize = (-2isize) as usize;
const PSCI_RET_DENIED: usize = (-3isize) as usize;
#[allow(dead_code)]
const PSCI_RET_ALREADY_ON: usize = (-4isize) as usize;
#[allow(dead_code)]
const PSCI_RET_ON_PENDING: usize = (-5isize) as usize;
#[allow(dead_code)]
const PSCI_RET_INTERNAL_FAILURE: usize = (-6isize) as usize;
const PSCI_AFFINITY_LEVEL_ON: usize = 0;
const PSCI_AFFINITY_LEVEL_OFF: usize = 1;
const PSCI_AFFINITY_LEVEL_ON_PENDING: usize = 2;
const PSCI_MIGRATE_TYPE_TOS_NOT_PRESENT: usize = 2;
const PSCI_POWER_STATE_TYPE_SHIFT: u64 = 16;
const PSCI_POWER_STATE_TYPE_MASK: u64 = 0x1;
const PSCI_POWER_STATE_TYPE_STANDBY: u64 = 0;
const PSCI_POWER_STATE_TYPE_POWERDOWN: u64 = 1;

fn psci_power_state_type(power_state: u64) -> u64 {
    (power_state >> PSCI_POWER_STATE_TYPE_SHIFT) & PSCI_POWER_STATE_TYPE_MASK
}

fn psci_affinity_info_result(state: crate::VmVcpuState) -> usize {
    match state {
        crate::VmVcpuState::Ready | crate::VmVcpuState::Running => PSCI_AFFINITY_LEVEL_ON,
        crate::VmVcpuState::Starting => PSCI_AFFINITY_LEVEL_ON_PENDING,
        _ => PSCI_AFFINITY_LEVEL_OFF,
    }
}

#[allow(dead_code)]
fn psci_find_vcpu_by_mpidr<I>(target_cpu: u64, vcpus: I) -> Option<usize>
where
    I: IntoIterator<Item = (usize, u64)>,
{
    vcpus.into_iter().find_map(|(vcpu_id, mpidr)| {
        psci_mpidr_matches_affinity_level(mpidr, target_cpu, 0).then_some(vcpu_id)
    })
}

fn psci_mpidr_affinity_mask(affinity_level: u64) -> Option<u64> {
    match affinity_level {
        0 => Some(0x0000_00ff_00ff_ffff),
        1 => Some(0x0000_00ff_00ff_ff00),
        2 => Some(0x0000_00ff_00ff_0000),
        3 => Some(0x0000_00ff_0000_0000),
        _ => None,
    }
}

fn psci_mpidr_matches_affinity_level(
    vcpu_mpidr: u64,
    target_affinity: u64,
    affinity_level: u64,
) -> bool {
    psci_mpidr_affinity_mask(affinity_level)
        .is_some_and(|mask| (vcpu_mpidr & mask) == (target_affinity & mask))
}

fn psci_affinity_info_result_for_domain<I>(
    target_affinity: u64,
    affinity_level: u64,
    vcpus: I,
) -> usize
where
    I: IntoIterator<Item = (u64, crate::VmVcpuState)>,
{
    if psci_mpidr_affinity_mask(affinity_level).is_none() {
        return PSCI_RET_INVALID_PARAMETERS;
    }

    let mut has_match = false;
    let mut has_on_pending = false;

    for (mpidr, state) in vcpus {
        if !psci_mpidr_matches_affinity_level(mpidr, target_affinity, affinity_level) {
            continue;
        }

        has_match = true;
        match psci_affinity_info_result(state) {
            PSCI_AFFINITY_LEVEL_ON => return PSCI_AFFINITY_LEVEL_ON,
            PSCI_AFFINITY_LEVEL_ON_PENDING => has_on_pending = true,
            _ => {}
        }
    }

    if !has_match {
        PSCI_RET_INVALID_PARAMETERS
    } else if has_on_pending {
        PSCI_AFFINITY_LEVEL_ON_PENDING
    } else {
        PSCI_AFFINITY_LEVEL_OFF
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HyperCallOutcome {
    Return(usize),
    CpuSuspendStandby { return_value: usize },
    CpuOff,
    SystemOff,
    SystemReset,
}

#[allow(dead_code)]
fn psci_cpu_on_result(result: Result<(), vcpus::VcpuOnError>) -> usize {
    match result {
        Ok(()) => PSCI_RET_SUCCESS,
        Err(vcpus::VcpuOnError::AlreadyOn) => PSCI_RET_ALREADY_ON,
        Err(vcpus::VcpuOnError::OnPending) => PSCI_RET_ON_PENDING,
        Err(vcpus::VcpuOnError::StartFailed) => PSCI_RET_INTERNAL_FAILURE,
    }
}

fn psci_cpu_off_result(has_multiple_running_vcpus: bool) -> HyperCallOutcome {
    if has_multiple_running_vcpus {
        HyperCallOutcome::CpuOff
    } else {
        HyperCallOutcome::Return(PSCI_RET_DENIED)
    }
}

fn decode_hypercall_code(raw_code: u64, abi: HyperCallAbi) -> HyperCallResult<HyperCallCode> {
    let code = HyperCallCode::try_from(raw_code as u32)?;
    if abi != HyperCallAbi::AArch64 && is_psci_code(code) {
        return Err(HyperCallError::Unsupported {
            code,
            detail: "PSCI hypercalls are only available on AArch64".to_string(),
        });
    }
    Ok(code)
}

fn psci_feature_result(function_id: u64) -> usize {
    if function_id == ARM_SMCCC_VERSION_FUNC_ID {
        return PSCI_RET_SUCCESS;
    }

    match decode_hypercall_code(function_id, HyperCallAbi::AArch64) {
        Ok(
            HyperCallCode::PSCIVersion
            | HyperCallCode::PSCIFeatures
            | HyperCallCode::PSCICpuSuspend
            | HyperCallCode::PSCICpuSuspend64
            | HyperCallCode::PSCICpuOff
            | HyperCallCode::PSCICpuOn
            | HyperCallCode::PSCICpuOn64
            | HyperCallCode::PSCIAffinityInfo
            | HyperCallCode::PSCIAffinityInfo64
            | HyperCallCode::PSCIMigrateInfoType
            | HyperCallCode::PSCISystemOff
            | HyperCallCode::PSCISystemReset,
        ) => PSCI_RET_SUCCESS,
        _ => PSCI_RET_NOT_SUPPORTED,
    }
}

fn dispatch_psci(code: HyperCallCode, args: [u64; 6]) -> Option<HyperCallResult> {
    match code {
        HyperCallCode::PSCIVersion => Some(Ok(PSCI_VERSION_0_2)),
        HyperCallCode::PSCIFeatures => Some(Ok(psci_feature_result(args[0]))),
        HyperCallCode::PSCIMigrateInfoType => Some(Ok(PSCI_MIGRATE_TYPE_TOS_NOT_PRESENT)),
        HyperCallCode::PSCIMigrate
        | HyperCallCode::PSCIMigrate64
        | HyperCallCode::PSCIMigrateInfoUpCpu
        | HyperCallCode::PSCIMigrateInfoUpCpu64 => Some(Ok(PSCI_RET_NOT_SUPPORTED)),

        HyperCallCode::PSCICpuOn
        | HyperCallCode::PSCICpuOn64
        | HyperCallCode::PSCIAffinityInfo
        | HyperCallCode::PSCIAffinityInfo64
        | HyperCallCode::PSCICpuSuspend
        | HyperCallCode::PSCICpuSuspend64
        | HyperCallCode::PSCICpuOff
        | HyperCallCode::PSCISystemOff
        | HyperCallCode::PSCISystemReset => None,

        _ => None,
    }
}

pub struct HyperCall {
    vm: VMRef,
    code: HyperCallCode,
    args: [u64; 6],
}

impl HyperCall {
    pub fn new(vm: VMRef, code: u64, args: [u64; 6], abi: HyperCallAbi) -> HyperCallResult<Self> {
        let code = decode_hypercall_code(code, abi)?;

        Ok(Self { vm, code, args })
    }

    pub(crate) fn execute(&self) -> Result<HyperCallOutcome, HyperCallError> {
        match self.code {
            HyperCallCode::PSCIVersion => {
                info!("VM[{}] PSCI_VERSION", self.vm.id());
                dispatch_psci(self.code, self.args)
                    .unwrap()
                    .map(HyperCallOutcome::Return)
            }
            HyperCallCode::PSCIFeatures => {
                info!(
                    "VM[{}] PSCI_FEATURES function_id={:#x}",
                    self.vm.id(),
                    self.args[0]
                );
                dispatch_psci(self.code, self.args)
                    .unwrap()
                    .map(HyperCallOutcome::Return)
            }
            HyperCallCode::PSCICpuOn | HyperCallCode::PSCICpuOn64 => {
                let target_cpu = self.args[0];
                let entry_point = GuestPhysAddr::from_usize(self.args[1] as usize);
                let context_id = self.args[2] as usize;

                info!(
                    "VM[{}] PSCI_CPU_ON target={target_cpu:#x} entry={:#x} context={context_id:#x}",
                    self.vm.id(),
                    self.args[1]
                );

                let Some(target_vcpu_id) = psci_find_vcpu_by_mpidr(
                    target_cpu,
                    self.vm
                        .get_vcpu_guest_mpidrs()
                        .iter()
                        .map(|(vcpu_id, mpidr)| (*vcpu_id, *mpidr)),
                ) else {
                    return Ok(HyperCallOutcome::Return(PSCI_RET_INVALID_PARAMETERS));
                };

                let result =
                    vcpus::vcpu_on(self.vm.clone(), target_vcpu_id, entry_point, context_id);
                Ok(HyperCallOutcome::Return(psci_cpu_on_result(result)))
            }
            HyperCallCode::PSCICpuSuspend | HyperCallCode::PSCICpuSuspend64 => {
                let power_state = self.args[0];
                let state_type = psci_power_state_type(power_state);

                match state_type {
                    PSCI_POWER_STATE_TYPE_STANDBY => {
                        info!("VM[{}] PSCI_CPU_SUSPEND standby", self.vm.id());
                        Ok(HyperCallOutcome::CpuSuspendStandby {
                            return_value: PSCI_RET_SUCCESS,
                        })
                    }
                    PSCI_POWER_STATE_TYPE_POWERDOWN => {
                        info!(
                            "VM[{}] PSCI_CPU_SUSPEND powerdown is not supported before wake \
                             lifecycle",
                            self.vm.id()
                        );
                        Ok(HyperCallOutcome::Return(PSCI_RET_NOT_SUPPORTED))
                    }
                    _ => Ok(HyperCallOutcome::Return(PSCI_RET_INVALID_PARAMETERS)),
                }
            }
            HyperCallCode::PSCICpuOff => {
                info!("VM[{}] PSCI_CPU_OFF", self.vm.id());
                let current = crate::host::task::current_task();
                let cpu_off_reserved = current
                    .try_as_vcpu_task()
                    .map(|task| task.vcpu.id())
                    .and_then(|vcpu_id| {
                        self.vm
                            .runtime_handle()
                            .map(|runtime| runtime.try_reserve_cpu_off(vcpu_id))
                            .ok()
                    })
                    .unwrap_or(false);
                Ok(psci_cpu_off_result(cpu_off_reserved))
            }
            HyperCallCode::PSCIAffinityInfo | HyperCallCode::PSCIAffinityInfo64 => {
                let target_affinity = self.args[0];
                let affinity_level = self.args[1];
                let vcpus = self.vm.vcpu_list();

                let affinity = psci_affinity_info_result_for_domain(
                    target_affinity,
                    affinity_level,
                    self.vm
                        .get_vcpu_guest_mpidrs()
                        .iter()
                        .map(|(vcpu_id, mpidr)| (*mpidr, vcpus[*vcpu_id].state())),
                );

                Ok(HyperCallOutcome::Return(affinity))
            }
            HyperCallCode::PSCIMigrate
            | HyperCallCode::PSCIMigrate64
            | HyperCallCode::PSCIMigrateInfoType
            | HyperCallCode::PSCIMigrateInfoUpCpu
            | HyperCallCode::PSCIMigrateInfoUpCpu64 => dispatch_psci(self.code, self.args)
                .unwrap()
                .map(HyperCallOutcome::Return),
            HyperCallCode::PSCISystemOff => {
                warn!("VM[{}] PSCI_SYSTEM_OFF", self.vm.id());
                Ok(HyperCallOutcome::SystemOff)
            }
            HyperCallCode::PSCISystemReset => {
                info!("VM[{}] PSCI_SYSTEM_RESET", self.vm.id());
                Ok(HyperCallOutcome::SystemReset)
            }
            HyperCallCode::HIVCPublishChannel => {
                let key = self.args[0] as usize;
                let shm_base_gpa_ptr = GuestPhysAddr::from_usize(self.args[1] as usize);
                let shm_size_ptr = GuestPhysAddr::from_usize(self.args[2] as usize);

                info!(
                    "VM[{}] HyperCall {:?} key {:#x}",
                    self.vm.id(),
                    self.code,
                    key
                );
                // User will pass the size of the shared memory region,
                // we will allocate the shared memory region based on this size.
                let shm_region_size =
                    self.vm
                        .read_from_guest_of::<usize>(shm_size_ptr)
                        .map_err(|error| {
                            self.guest_memory_error("read IVC channel size", shm_size_ptr, error)
                        })?;
                ivc::ensure_channel_absent(self.vm.id(), key).map_err(|error| {
                    self.operation_error("check IVC channel availability", error)
                })?;
                let requested_size = shm_region_size.min(ivc::MAX_IVC_CHANNEL_SIZE);
                let (shm_base_gpa, shm_region_size) =
                    self.vm.alloc_ivc_channel(requested_size).map_err(|error| {
                        self.operation_error("reserve IVC guest address range", error)
                    })?;

                let ivc_channel =
                    match IVCChannel::alloc(self.vm.id(), key, shm_region_size, shm_base_gpa)
                        .map_err(|error| self.operation_error("allocate IVC channel", error))
                    {
                        Ok(channel) => channel,
                        Err(err) => {
                            if let Err(release_err) =
                                self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                            {
                                warn!(
                                    "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after \
                                     channel allocation failure: {release_err:?}",
                                    self.vm.id()
                                );
                            }
                            return Err(err);
                        }
                    };

                let actual_size = ivc_channel.size();

                if let Err(err) = self.vm.map_region(
                    shm_base_gpa,
                    ivc_channel.base_hpa(),
                    actual_size,
                    shared_memory_mapping_flags(),
                ) {
                    if let Err(release_err) =
                        self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                    {
                        warn!(
                            "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after mapping \
                             failure: {release_err:?}",
                            self.vm.id()
                        );
                    }
                    return Err(self.operation_error("map publisher IVC channel", err));
                }

                if let Err(err) = self
                    .vm
                    .write_to_guest_of(shm_base_gpa_ptr, &shm_base_gpa.as_usize())
                    .and_then(|_| self.vm.write_to_guest_of(shm_size_ptr, &actual_size))
                {
                    if let Err(unmap_err) = self.vm.unmap_region(shm_base_gpa, actual_size) {
                        warn!(
                            "VM[{}] failed to unmap IVC GPA {shm_base_gpa:#x} after guest write \
                             failure: {unmap_err:?}",
                            self.vm.id()
                        );
                    }
                    if let Err(release_err) =
                        self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                    {
                        warn!(
                            "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after guest write \
                             failure: {release_err:?}",
                            self.vm.id()
                        );
                    }
                    return Err(self.guest_memory_error(
                        "write published IVC channel result",
                        shm_base_gpa_ptr,
                        err,
                    ));
                }

                if let Err(err) = ivc::insert_channel(self.vm.id(), ivc_channel) {
                    if let Err(unmap_err) = self.vm.unmap_region(shm_base_gpa, actual_size) {
                        warn!(
                            "VM[{}] failed to unmap IVC GPA {shm_base_gpa:#x} after channel \
                             insert failure: {unmap_err:?}",
                            self.vm.id()
                        );
                    }
                    if let Err(release_err) =
                        self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                    {
                        warn!(
                            "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after channel \
                             insert failure: {release_err:?}",
                            self.vm.id()
                        );
                    }
                    return Err(self.operation_error("register published IVC channel", err));
                }

                Ok(HyperCallOutcome::Return(0))
            }
            HyperCallCode::HIVCUnPublishChannel => {
                let key = self.args[0] as usize;

                info!(
                    "VM[{}] HyperCall {:?} with key {:#x}",
                    self.vm.id(),
                    self.code,
                    key
                );
                let teardown = ivc::unpublish_channel(self.vm.id(), key)
                    .map_err(|error| self.operation_error("unpublish IVC channel", error))?;
                if !crate::vm::release_ivc_teardown_for_vm(self.vm.id(), teardown, &self.vm) {
                    return Err(HyperCallError::Internal {
                        code: self.code,
                        operation: "release unpublished IVC channel",
                        detail: "failed to unmap guest GPA or release the IVC aperture range"
                            .into(),
                    });
                }

                Ok(HyperCallOutcome::Return(0))
            }
            HyperCallCode::HIVCSubscribChannel => {
                let publisher_vm_id = self.args[0] as usize;
                let key = self.args[1] as usize;
                let shm_base_gpa_ptr = GuestPhysAddr::from_usize(self.args[2] as usize);
                let shm_size_ptr = GuestPhysAddr::from_usize(self.args[3] as usize);

                info!(
                    "VM[{}] HyperCall {:?} to VM[{}]",
                    self.vm.id(),
                    self.code,
                    publisher_vm_id
                );

                let shm_size = ivc::prepare_subscribe_channel(publisher_vm_id, key, self.vm.id())
                    .map_err(|error| {
                    self.operation_error("prepare IVC channel subscription", error)
                })?;
                let (shm_base_gpa, shm_region_size) =
                    self.vm.alloc_ivc_channel(shm_size).map_err(|error| {
                        self.operation_error("reserve subscriber IVC guest address range", error)
                    })?;

                let subscribe_result = ivc::subscribe_to_channel_of_publisher(
                    publisher_vm_id,
                    key,
                    self.vm.id(),
                    shm_base_gpa,
                );
                let (base_hpa, actual_size) = match subscribe_result {
                    Ok(channel) => channel,
                    Err(err) => {
                        if let Err(release_err) =
                            self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                        {
                            warn!(
                                "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after \
                                 subscribe registration failure: {release_err:?}",
                                self.vm.id()
                            );
                        }
                        return Err(self.operation_error("register IVC channel subscriber", err));
                    }
                };

                if let Err(err) = self.vm.map_region(
                    shm_base_gpa,
                    base_hpa,
                    actual_size,
                    shared_memory_mapping_flags(),
                ) {
                    match ivc::unsubscribe_from_channel_of_publisher(
                        publisher_vm_id,
                        key,
                        self.vm.id(),
                    ) {
                        Ok(teardown) => {
                            if let Err(release_err) =
                                self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                            {
                                warn!(
                                    "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after \
                                     subscribe mapping failure: {release_err:?}",
                                    self.vm.id()
                                );
                            } else {
                                teardown.commit();
                            }
                        }
                        Err(unsub_err) => {
                            warn!(
                                "VM[{}] failed to rollback IVC subscription to VM[{}] key \
                                 {key:#x} after mapping failure: {unsub_err:?}",
                                self.vm.id(),
                                publisher_vm_id
                            );
                        }
                    }
                    return Err(self.operation_error("map subscriber IVC channel", err));
                }

                if let Err(err) = self
                    .vm
                    .write_to_guest_of(shm_base_gpa_ptr, &shm_base_gpa.as_usize())
                    .and_then(|_| self.vm.write_to_guest_of(shm_size_ptr, &actual_size))
                {
                    match ivc::unsubscribe_from_channel_of_publisher(
                        publisher_vm_id,
                        key,
                        self.vm.id(),
                    ) {
                        Ok(teardown) => {
                            crate::vm::release_ivc_teardown_for_vm(
                                self.vm.id(),
                                teardown,
                                &self.vm,
                            );
                        }
                        Err(unsub_err) => {
                            warn!(
                                "VM[{}] failed to rollback IVC subscription to VM[{}] key \
                                 {key:#x} after guest write failure: {unsub_err:?}",
                                self.vm.id(),
                                publisher_vm_id
                            );
                        }
                    }
                    return Err(self.guest_memory_error(
                        "write subscribed IVC channel result",
                        shm_base_gpa_ptr,
                        err,
                    ));
                }

                info!(
                    "VM[{}] HyperCall HIVC_REGISTER_SUBSCRIBER success, base GPA: {:#x}, size: {}",
                    self.vm.id(),
                    shm_base_gpa,
                    actual_size
                );

                Ok(HyperCallOutcome::Return(0))
            }
            HyperCallCode::HIVCUnSubscribChannel => {
                let publisher_vm_id = self.args[0] as usize;
                let key = self.args[1] as usize;

                info!(
                    "VM[{}] HyperCall {:?} from VM[{}]",
                    self.vm.id(),
                    self.code,
                    publisher_vm_id
                );
                let teardown =
                    ivc::unsubscribe_from_channel_of_publisher(publisher_vm_id, key, self.vm.id())
                        .map_err(|error| {
                            self.operation_error("unsubscribe from IVC channel", error)
                        })?;
                if !crate::vm::release_ivc_teardown_for_vm(self.vm.id(), teardown, &self.vm) {
                    return Err(HyperCallError::Internal {
                        code: self.code,
                        operation: "release unsubscribed IVC channel",
                        detail: "failed to unmap guest GPA or release the IVC aperture range"
                            .into(),
                    });
                }

                Ok(HyperCallOutcome::Return(0))
            }
            HyperCallCode::HIVCNotify => {
                let publisher_vm_id = self.args[0] as usize;
                let key = self.args[1] as usize;
                let target_vm_id = self.args[2] as usize;

                let route =
                    ivc::prepare_notify_channel(publisher_vm_id, key, self.vm.id(), target_vm_id)
                        .map_err(|error| self.operation_error("prepare IVC notify route", error))?;
                let target_vm = crate::get_vm_by_id(route.target_vm_id).ok_or_else(|| {
                    HyperCallError::ResourceNotFound {
                        code: self.code,
                        resource: format!("VM {}", route.target_vm_id),
                        detail: "IVC notify target VM does not exist".into(),
                    }
                })?;
                let target_runtime = target_vm
                    .runtime_handle()
                    .map_err(|error| self.operation_error("wake IVC notify target VM", error))?;
                target_runtime.notify_all();
                let target_devices = target_vm.get_devices().map_err(|error| {
                    self.operation_error("get IVC notify target devices", error)
                })?;
                let notify_irq = ivc::notify_peer(&target_devices).map_err(|error| {
                    self.operation_error("notify IVC peer interrupt", error.into())
                })?;
                info!(
                    "IVC notify source VM[{}] target VM[{}] publisher VM[{}] key {:#x} irq={:?}",
                    route.source_vm_id,
                    route.target_vm_id,
                    route.publisher_vm_id,
                    route.key,
                    notify_irq
                );

                Ok(HyperCallOutcome::Return(0))
            }
            _ => {
                warn!("Unsupported hypercall code: {:?}", self.code);
                Err(HyperCallError::Unsupported {
                    code: self.code,
                    detail: "the hypervisor does not implement this control hypercall".into(),
                })
            }
        }
    }

    fn operation_error(&self, operation: &'static str, error: AxVmError) -> HyperCallError {
        let detail = format!("{operation}: {error}");
        match error {
            AxVmError::InvalidInput { .. } | AxVmError::HostOwnedDevice { .. } => {
                HyperCallError::InvalidParameter {
                    code: self.code,
                    parameter: "arguments",
                    detail,
                }
            }
            AxVmError::InvalidState { .. } | AxVmError::InvalidTransition { .. } => {
                HyperCallError::InvalidState {
                    code: self.code,
                    detail,
                }
            }
            AxVmError::VmNotFound { vm_id } => HyperCallError::ResourceNotFound {
                code: self.code,
                resource: format!("VM {vm_id}"),
                detail,
            },
            AxVmError::ResourceUnavailable { resource, .. } => HyperCallError::ResourceNotFound {
                code: self.code,
                resource: resource.into(),
                detail,
            },
            AxVmError::ResourceConflict { resource, .. } => HyperCallError::ResourceConflict {
                code: self.code,
                resource: resource.into(),
                detail,
            },
            AxVmError::Unsupported { .. } => HyperCallError::Unsupported {
                code: self.code,
                detail,
            },
            AxVmError::OutOfMemory { .. } => HyperCallError::OutOfMemory {
                code: self.code,
                operation,
            },
            AxVmError::InvalidConfig { .. }
            | AxVmError::LifecycleRollback { .. }
            | AxVmError::Boot { .. }
            | AxVmError::Memory { .. }
            | AxVmError::Device { .. }
            | AxVmError::DeviceResourcePlanning(_)
            | AxVmError::GuestGicProfile(_)
            | AxVmError::GuestPlicProfile(_)
            | AxVmError::Vcpu { .. }
            | AxVmError::Interrupt { .. }
            | AxVmError::Host { .. } => HyperCallError::Internal {
                code: self.code,
                operation,
                detail,
            },
        }
    }

    fn guest_memory_error(
        &self,
        operation: &'static str,
        address: GuestPhysAddr,
        error: AxVmError,
    ) -> HyperCallError {
        HyperCallError::GuestMemoryAccess {
            code: self.code,
            operation,
            address: address.as_usize(),
            detail: format!("{error}"),
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn hvc_psci_features_cpu_on_matches_execute_contract() {
        assert_eq!(
            dispatch_psci(HyperCallCode::PSCIFeatures, [0x8400_0003, 0, 0, 0, 0, 0]),
            Some(Ok(PSCI_RET_SUCCESS))
        );
        assert_ne!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::StartFailed)),
            PSCI_RET_NOT_SUPPORTED
        );
        assert_ne!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::AlreadyOn)),
            PSCI_RET_NOT_SUPPORTED
        );
    }

    #[test]
    fn hvc_cpu_on_start_failure_is_not_reported_as_success() {
        assert_eq!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::StartFailed)),
            PSCI_RET_INTERNAL_FAILURE
        );
        assert_ne!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::StartFailed)),
            PSCI_RET_SUCCESS
        );
    }

    #[test]
    fn hvc_cpu_on_busy_states_keep_psci_status() {
        assert_eq!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::AlreadyOn)),
            PSCI_RET_ALREADY_ON
        );
        assert_eq!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::OnPending)),
            PSCI_RET_ON_PENDING
        );
    }

    #[test]
    fn hvc_cpu_off_last_vcpu_denial_uses_psci_denied() {
        assert_eq!(
            super::psci_cpu_off_result(false),
            super::HyperCallOutcome::Return(super::PSCI_RET_DENIED)
        );
        assert_eq!(
            super::psci_cpu_off_result(true),
            super::HyperCallOutcome::CpuOff
        );
    }

    use super::*;

    #[test]
    fn hvc_decodes_psci_version_and_dispatches_0_2() {
        let code = decode_hypercall_code(0x8400_0000, HyperCallAbi::AArch64).unwrap();

        assert_eq!(code, HyperCallCode::PSCIVersion);
        assert_eq!(dispatch_psci(code, [0; 6]), Some(Ok(0x0000_0002)));
    }

    #[test]
    fn hvc_decodes_psci_calls_and_returns_standard_errors() {
        for raw_code in [0x8400_000a, 0x8400_0006] {
            let code = decode_hypercall_code(raw_code, HyperCallAbi::AArch64).unwrap();

            assert!(dispatch_psci(code, [0; 6]).is_some());
        }

        let cpu_on = decode_hypercall_code(0xc400_0003, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(cpu_on, [1, 0x80000, 0, 0, 0, 0]), None);

        let cpu_on32 = decode_hypercall_code(0x8400_0003, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(cpu_on32, [1, 0x80000, 0, 0, 0, 0]), None);

        let cpu_off = decode_hypercall_code(0x8400_0002, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(cpu_off, [0; 6]), None);

        let affinity_info = decode_hypercall_code(0xc400_0004, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(affinity_info, [0, 0, 0, 0, 0, 0]), None);

        let features = decode_hypercall_code(0x8400_000a, HyperCallAbi::AArch64).unwrap();
        assert_eq!(
            dispatch_psci(features, [0x8400_0000, 0, 0, 0, 0, 0]),
            Some(Ok(PSCI_RET_SUCCESS))
        );
        assert_eq!(
            dispatch_psci(features, [0x8400_ffff, 0, 0, 0, 0, 0]),
            Some(Ok(PSCI_RET_NOT_SUPPORTED))
        );
    }

    #[test]
    fn generic_hvc_rejects_psci_function_ids() {
        assert!(decode_hypercall_code(0x8400_0009, HyperCallAbi::Generic).is_err());
        assert!(decode_hypercall_code(0x8400_0003, HyperCallAbi::Generic).is_err());
        assert!(decode_hypercall_code(0xc400_0003, HyperCallAbi::Generic).is_err());
    }

    #[test]
    fn hvc_decodes_cpu_suspend_extended_state_type() {
        assert_eq!(psci_power_state_type(0), PSCI_POWER_STATE_TYPE_STANDBY);
        assert_eq!(
            psci_power_state_type(1 << 16),
            PSCI_POWER_STATE_TYPE_POWERDOWN
        );
        assert_eq!(
            psci_power_state_type(1 << 30),
            PSCI_POWER_STATE_TYPE_STANDBY
        );
    }

    #[test]
    fn hvc_affinity_info_reports_cpu_on_pending_before_first_run() {
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Starting),
            PSCI_AFFINITY_LEVEL_ON_PENDING
        );
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Ready),
            PSCI_AFFINITY_LEVEL_ON
        );
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Running),
            PSCI_AFFINITY_LEVEL_ON
        );
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Free),
            PSCI_AFFINITY_LEVEL_OFF
        );
    }

    #[test]
    fn hvc_cpu_on_matches_guest_mpidr_not_host_placement() {
        let guest_mpidrs = [(0, 0x0), (1, 0x100)];

        assert_eq!(psci_find_vcpu_by_mpidr(0x100, guest_mpidrs), Some(1));
        assert_eq!(psci_find_vcpu_by_mpidr(5, guest_mpidrs), None);
    }

    #[test]
    fn hvc_affinity_info_matches_requested_mpidr_level() {
        let cpu0 = 0x0000_00ab_0002_0100;
        let cpu1 = 0x0000_00ab_0002_0101;
        let other_cluster = 0x0000_00ab_0002_0200;
        let other_aff2 = 0x0000_00ab_0003_0100;

        assert!(psci_mpidr_matches_affinity_level(cpu0, cpu0, 0));
        assert!(!psci_mpidr_matches_affinity_level(cpu1, cpu0, 0));

        assert!(psci_mpidr_matches_affinity_level(cpu0, cpu0, 1));
        assert!(psci_mpidr_matches_affinity_level(cpu1, cpu0, 1));
        assert!(!psci_mpidr_matches_affinity_level(other_cluster, cpu0, 1));

        assert!(psci_mpidr_matches_affinity_level(other_cluster, cpu0, 2));
        assert!(!psci_mpidr_matches_affinity_level(other_aff2, cpu0, 2));

        assert!(!psci_mpidr_matches_affinity_level(cpu0, cpu0, 4));
    }

    #[test]
    fn hvc_affinity_info_aggregates_nonzero_level_domain() {
        let cpu0 = 0x0000_00ab_0002_0100;
        let cpu1 = 0x0000_00ab_0002_0101;
        let other_cluster = 0x0000_00ab_0002_0200;

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [
                    (cpu0, crate::VmVcpuState::Free),
                    (cpu1, crate::VmVcpuState::Starting),
                    (other_cluster, crate::VmVcpuState::Ready),
                ],
            ),
            PSCI_AFFINITY_LEVEL_ON_PENDING
        );

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [
                    (cpu0, crate::VmVcpuState::Free),
                    (cpu1, crate::VmVcpuState::Ready),
                ],
            ),
            PSCI_AFFINITY_LEVEL_ON
        );

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [
                    (cpu0, crate::VmVcpuState::Free),
                    (cpu1, crate::VmVcpuState::Free),
                ],
            ),
            PSCI_AFFINITY_LEVEL_OFF
        );

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [(other_cluster, crate::VmVcpuState::Ready)],
            ),
            PSCI_RET_INVALID_PARAMETERS
        );
    }

    #[test]
    fn hvc_decodes_unsupported_psci_migration_calls() {
        for raw_code in [0x8400_0005, 0x8400_0007, 0xc400_0005, 0xc400_0007] {
            let code = decode_hypercall_code(raw_code, HyperCallAbi::AArch64).unwrap();

            assert_eq!(
                dispatch_psci(code, [0; 6]),
                Some(Ok(PSCI_RET_NOT_SUPPORTED))
            );
            assert_eq!(psci_feature_result(raw_code), PSCI_RET_NOT_SUPPORTED);
        }
    }

    #[test]
    fn hvc_advertises_system_reset_as_required_psci_0_2_call() {
        assert_eq!(psci_feature_result(0x8400_0009), PSCI_RET_SUCCESS);
    }

    #[test]
    fn hvc_psci_features_cover_implemented_0_2_surface() {
        let features = HyperCallCode::PSCIFeatures;
        let supported = [
            0x8400_0000, // PSCI_VERSION
            0x8400_0001, // PSCI_CPU_SUSPEND
            0x8400_0002, // PSCI_CPU_OFF
            0x8400_0003, // PSCI_CPU_ON
            0x8400_0004, // PSCI_AFFINITY_INFO
            0x8400_0006, // PSCI_MIGRATE_INFO_TYPE
            0x8400_0008, // PSCI_SYSTEM_OFF
            0x8400_0009, // PSCI_SYSTEM_RESET
            0x8400_000a, // PSCI_FEATURES
            0xc400_0001, // PSCI_CPU_SUSPEND64
            0xc400_0003, // PSCI_CPU_ON64
            0xc400_0004, // PSCI_AFFINITY_INFO64
        ];

        for raw_code in supported {
            assert_eq!(psci_feature_result(raw_code), PSCI_RET_SUCCESS);
            assert_eq!(
                dispatch_psci(features, [raw_code, 0, 0, 0, 0, 0]),
                Some(Ok(PSCI_RET_SUCCESS))
            );
        }

        let unsupported = [
            0x8400_0005, // PSCI_MIGRATE
            0x8400_0007, // PSCI_MIGRATE_INFO_UP_CPU
            0xc400_0005, // PSCI_MIGRATE64
            0xc400_0007, // PSCI_MIGRATE_INFO_UP_CPU64
        ];

        for raw_code in unsupported {
            assert_eq!(psci_feature_result(raw_code), PSCI_RET_NOT_SUPPORTED);
            assert_eq!(
                dispatch_psci(features, [raw_code, 0, 0, 0, 0, 0]),
                Some(Ok(PSCI_RET_NOT_SUPPORTED))
            );
        }
    }

    #[test]
    fn hvc_psci_features_advertises_smccc_version_query() {
        assert_eq!(
            psci_feature_result(ARM_SMCCC_VERSION_FUNC_ID),
            PSCI_RET_SUCCESS
        );
    }

    #[test]
    fn hvc_rejects_unknown_psci_function_ids() {
        assert!(decode_hypercall_code(0x8400_ffff, HyperCallAbi::AArch64).is_err());
    }
}
