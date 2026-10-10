//! Observations responsibilities of the unique lifecycle owner.

use super::Owner;
use crate::{
    VmVcpuState,
    manager::{CpuObservation, DeviceObservation, MemoryObservation, VmConfigSnapshot, VmSnapshot},
    vm::VcpuSnapshot,
};

impl Owner {
    pub(super) fn publish(&self) {
        let description = VmConfigSnapshot {
            bsp_entry: self.vm.config().bsp_entry(),
            ap_entry: self.vm.config().ap_entry(),
            image: self.vm.config().image_config().clone(),
            address_space_policy: self.vm.config().address_space_policy(),
            vcpu_affinities: self.vm.get_vcpu_affinities_pcpu_ids(),
            passthrough_devices: self.vm.config().pass_through_devices().to_vec(),
            passthrough_addresses: self.vm.config().pass_through_addresses().to_vec(),
            passthrough_irqs: self.vm.config().pass_through_irqs().to_vec(),
        };
        let mut vcpu = self.vm.vcpu_snapshots();
        let mut entry_count = 0;
        let mut park_count = 0;
        let mut progress = Vec::new();
        if let Some(run) = &self.run {
            entry_count = run.retired_entries;
            park_count = run.retired_parks;
            for member in run.participants.values() {
                progress.push(member.port.progress.clone());
                if member.returned {
                    continue;
                }
                vcpu.push(VcpuSnapshot {
                    id: member.instance.vcpu_id,
                    state: if self.state == crate::VmStatus::Paused
                        || (self.state == crate::VmStatus::Pausing
                            && self.current_operation.is_some_and(|operation| {
                                member.parked.completed(member.instance, operation)
                            }))
                    {
                        VmVcpuState::Blocked
                    } else if member.started {
                        VmVcpuState::Ready
                    } else {
                        VmVcpuState::Starting
                    },
                    phys_cpu_set: description
                        .vcpu_affinities
                        .iter()
                        .find(|(id, ..)| *id == member.instance.vcpu_id)
                        .and_then(|(_, mask, _)| *mask),
                });
            }
        }
        vcpu.sort_by_key(|cpu| cpu.id);
        let regions = self.vm.memory_regions();
        self.shared.publish(
            VmSnapshot {
                key: self.shared.key(),
                vm_id: self.vm.id(),
                name: self.vm.name(),
                state: self.state,
                run: self.last_run,
                current_operation: self.current_operation,
                last_failure: self.last_failure.clone(),
                last_stop_reason: self.last_stop_reason.clone(),
                cpu: CpuObservation {
                    vcpu_num: self.vm.config().phys_cpu_ls.cpu_num(),
                    running_vcpu_count: self.run.as_ref().map_or(0, |run| {
                        run.participants
                            .values()
                            .filter(|member| member.started && !member.returned)
                            .count()
                    }),
                },
                memory: MemoryObservation {
                    nested_page_table_root: Some(self.vm.nested_page_table_root()),
                    total_bytes: regions.iter().map(|region| region.size()).sum(),
                    regions,
                },
                device: DeviceObservation {
                    device_count: self.vm.device_count(),
                },
                vcpu,
                description,
                entry_count,
                park_count,
            },
            progress,
        );
    }
}
