use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_hal::percpu::this_cpu_id;
use axbus::VcpuKicker;

use super::vcpus;

const VCPU_PCPU_UNBOUND: usize = usize::MAX;

pub struct AxVmKicker {
    vm_id: usize,
    num_vcpus: usize,
    pcpu_map: Arc<[AtomicUsize]>,
}

impl AxVmKicker {
    pub fn new(vm_id: usize, num_vcpus: usize, pcpu_map: Arc<[AtomicUsize]>) -> Self {
        Self {
            vm_id,
            num_vcpus,
            pcpu_map,
        }
    }
}

impl VcpuKicker for AxVmKicker {
    fn kick(&self, vcpu_id: usize) {
        let pcpu = self.pcpu_map[vcpu_id].load(Ordering::Acquire);

        if pcpu == VCPU_PCPU_UNBOUND {
            vcpus::notify_all_vcpus(self.vm_id);
            return;
        }

        if pcpu == this_cpu_id() as usize {
            return;
        }

        ax_ipi::run_on_cpu(pcpu, || {});
    }

    fn vcpu_count(&self) -> usize {
        self.num_vcpus
    }
}

pub fn build_irq_runtime(vm: &super::VMRef) -> axbus::IrqRuntime {
    let router = vm.router();

    let resolved_controllers = router.intc_map().clone();

    let default_intc_id = router.default_intc_id();
    let default_intc = default_intc_id.and_then(|id| router.find_intc(id).cloned());

    // Populate IrqRoutingTable from device configs.
    let mut routing = axbus::irq::IrqRoutingTable::new();
    if let Some(intc_id) = default_intc_id {
        vm.with_config(|config| {
            for dev in config.emu_devices() {
                if dev.irq_id != 0 {
                    routing.add_legacy(
                        axbus::IrqLine(dev.irq_id as u32),
                        intc_id,
                        dev.irq_id as u32,
                        axbus::TriggerMode::Edge,
                        None,
                        &dev.name,
                    );
                }
            }
            for dev in config.pass_through_devices() {
                if dev.irq_id != 0 {
                    routing.add_legacy(
                        axbus::IrqLine(dev.irq_id as u32),
                        intc_id,
                        dev.irq_id as u32,
                        axbus::TriggerMode::Edge,
                        None,
                        &dev.name,
                    );
                }
            }
        });
    }

    let pcpu_map = vm.vcpu_pcpu_map().clone();
    let kicker = Box::new(AxVmKicker::new(
        vm.id(),
        vm.vcpu_num(),
        pcpu_map,
    ));

    axbus::IrqRuntime::new(
        routing,
        default_intc,
        resolved_controllers,
        kicker,
    )
}
