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

use alloc::format;

use axhvc::{HyperCallCode, HyperCallError, HyperCallResult};

use crate::{
    AxVmError, GuestPhysAddr, MappingFlags,
    runtime::{
        VMRef,
        ivc::{self, IVCChannel},
    },
};

pub struct HyperCall {
    vm: VMRef,
    code: HyperCallCode,
    args: [u64; 6],
}

impl HyperCall {
    pub fn new(vm: VMRef, code: u64, args: [u64; 6]) -> HyperCallResult<Self> {
        let code = HyperCallCode::try_from(code as u32)?;

        Ok(Self { vm, code, args })
    }

    pub fn execute(&self) -> HyperCallResult {
        match self.code {
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
                    MappingFlags::READ | MappingFlags::WRITE,
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

                Ok(0)
            }
            HyperCallCode::HIVCUnPublishChannel => {
                let key = self.args[0] as usize;

                info!(
                    "VM[{}] HyperCall {:?} with key {:#x}",
                    self.vm.id(),
                    self.code,
                    key
                );
                let (base_gpa, size) = ivc::unpublish_channel(self.vm.id(), key)
                    .map_err(|error| self.operation_error("unpublish IVC channel", error))?;
                // The publisher's GPA mapping is always unmapped; subscribers keep their own
                // GPA views. The shared HPA frame is freed when the last subscriber leaves.
                self.vm.unmap_region(base_gpa, size).map_err(|error| {
                    self.operation_error("unmap unpublished IVC channel", error)
                })?;
                self.vm
                    .release_ivc_channel(base_gpa, size)
                    .map_err(|error| {
                        self.operation_error("release unpublished IVC channel", error)
                    })?;

                Ok(0)
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

                // TODO: separate the mapping flags of metadata and data.
                if let Err(err) = self.vm.map_region(
                    shm_base_gpa,
                    base_hpa,
                    actual_size,
                    MappingFlags::READ | MappingFlags::WRITE,
                ) {
                    if let Err(unsub_err) = ivc::unsubscribe_from_channel_of_publisher(
                        publisher_vm_id,
                        key,
                        self.vm.id(),
                    ) {
                        warn!(
                            "VM[{}] failed to rollback IVC subscription to VM[{}] key {key:#x} \
                             after mapping failure: {unsub_err:?}",
                            self.vm.id(),
                            publisher_vm_id
                        );
                    }
                    if let Err(release_err) =
                        self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                    {
                        warn!(
                            "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after subscribe \
                             mapping failure: {release_err:?}",
                            self.vm.id()
                        );
                    }
                    return Err(self.operation_error("map subscriber IVC channel", err));
                }

                if let Err(err) = self
                    .vm
                    .write_to_guest_of(shm_base_gpa_ptr, &shm_base_gpa.as_usize())
                    .and_then(|_| self.vm.write_to_guest_of(shm_size_ptr, &actual_size))
                {
                    if let Err(unmap_err) = self.vm.unmap_region(shm_base_gpa, actual_size) {
                        warn!(
                            "VM[{}] failed to unmap IVC GPA {shm_base_gpa:#x} after subscribe \
                             guest write failure: {unmap_err:?}",
                            self.vm.id()
                        );
                    }
                    if let Err(unsub_err) = ivc::unsubscribe_from_channel_of_publisher(
                        publisher_vm_id,
                        key,
                        self.vm.id(),
                    ) {
                        warn!(
                            "VM[{}] failed to rollback IVC subscription to VM[{}] key {key:#x} \
                             after guest write failure: {unsub_err:?}",
                            self.vm.id(),
                            publisher_vm_id
                        );
                    }
                    if let Err(release_err) =
                        self.vm.release_ivc_channel(shm_base_gpa, shm_region_size)
                    {
                        warn!(
                            "VM[{}] failed to release IVC GPA {shm_base_gpa:#x} after subscribe \
                             guest write failure: {release_err:?}",
                            self.vm.id()
                        );
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

                Ok(0)
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
                let (base_gpa, size) =
                    ivc::unsubscribe_from_channel_of_publisher(publisher_vm_id, key, self.vm.id())
                        .map_err(|error| {
                            self.operation_error("unsubscribe from IVC channel", error)
                        })?;
                self.vm.unmap_region(base_gpa, size).map_err(|error| {
                    self.operation_error("unmap unsubscribed IVC channel", error)
                })?;
                self.vm
                    .release_ivc_channel(base_gpa, size)
                    .map_err(|error| {
                        self.operation_error("release unsubscribed IVC channel", error)
                    })?;

                Ok(0)
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
            AxVmError::InvalidInput { .. } => HyperCallError::InvalidParameter {
                code: self.code,
                parameter: "arguments",
                detail,
            },
            AxVmError::CurrentVcpuInterruptOutOfRange { .. } => HyperCallError::InvalidParameter {
                code: self.code,
                parameter: "interrupt vector",
                detail,
            },
            AxVmError::InvalidState { .. } | AxVmError::InvalidTransition { .. } => {
                HyperCallError::InvalidState {
                    code: self.code,
                    detail,
                }
            }
            AxVmError::CurrentVcpuUnavailable => HyperCallError::InvalidState {
                code: self.code,
                detail,
            },
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
            | AxVmError::Boot { .. }
            | AxVmError::Memory { .. }
            | AxVmError::Device { .. }
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
