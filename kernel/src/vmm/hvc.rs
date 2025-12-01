use axaddrspace::{GuestPhysAddr, MappingFlags};
use axerrno::{AxResult, ax_err, ax_err_type};
use axhvc::{HyperCallCode, HyperCallResult};

use crate::vmm::ivc::{self, IVCChannel};
use crate::vmm::{VCpuRef, VMRef};

pub struct HyperCall {
    _vcpu: VCpuRef,
    vm: VMRef,
    code: HyperCallCode,
    args: [u64; 6],
}

impl HyperCall {
    pub fn new(vcpu: VCpuRef, vm: VMRef, code: u64, args: [u64; 6]) -> AxResult<Self> {
        let code = HyperCallCode::try_from(code as u32).map_err(|e| {
            warn!("Invalid hypercall code: {code} e {e:?}");
            ax_err_type!(InvalidInput)
        })?;

        Ok(Self {
            _vcpu: vcpu,
            vm,
            code,
            args,
        })
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
                let shm_region_size = self.vm.read_from_guest_of::<usize>(shm_size_ptr)?;
                let (shm_base_gpa, shm_region_size) = self.vm.alloc_ivc_channel(shm_region_size)?;

                let ivc_channel =
                    IVCChannel::alloc(self.vm.id(), key, shm_region_size, shm_base_gpa)?;

                let actual_size = ivc_channel.size();

                self.vm.map_region(
                    shm_base_gpa,
                    ivc_channel.base_hpa(),
                    actual_size,
                    MappingFlags::READ | MappingFlags::WRITE,
                )?;

                self.vm
                    .write_to_guest_of(shm_base_gpa_ptr, &shm_base_gpa.as_usize())?;
                self.vm.write_to_guest_of(shm_size_ptr, &actual_size)?;

                ivc::insert_channel(self.vm.id(), ivc_channel)?;

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
                let (base_gpa, size) = ivc::unpublish_channel(self.vm.id(), key)?.unwrap();
                self.vm.unmap_region(base_gpa, size)?;

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

                let shm_size = ivc::get_channel_size(publisher_vm_id, key)?;
                let (shm_base_gpa, _) = self.vm.alloc_ivc_channel(shm_size)?;

                let (base_hpa, actual_size) = ivc::subscribe_to_channel_of_publisher(
                    publisher_vm_id,
                    key,
                    self.vm.id(),
                    shm_base_gpa,
                )?;

                // TODO: seperate the mapping flags of metadata and data.
                self.vm.map_region(
                    shm_base_gpa,
                    base_hpa,
                    actual_size,
                    MappingFlags::READ | MappingFlags::WRITE,
                )?;

                self.vm
                    .write_to_guest_of(shm_base_gpa_ptr, &shm_base_gpa.as_usize())?;
                self.vm.write_to_guest_of(shm_size_ptr, &actual_size)?;

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
                    ivc::unsubscribe_from_channel_of_publisher(publisher_vm_id, key, self.vm.id())?;
                self.vm.unmap_region(base_gpa, size)?;

                Ok(0)
            }
            _ => {
                warn!("Unsupported hypercall code: {:?}", self.code);
                ax_err!(Unsupported)?
            }
        }
    }
}
