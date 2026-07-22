use alloc::{collections::BTreeMap, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use ax_lazyinit::LazyInit;
use axvmconfig::AxVMCrateConfig;

use super::UEFI_FIRMWARE_FDT_BASE;
use crate::{AxVMRef, AxVmResult, config::AxVMConfig};

static LOONGARCH_GUEST_IRQ_ROUTES: LazyInit<Mutex<BTreeMap<usize, Vec<LoongArchGuestIrqRoute>>>> =
    LazyInit::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoongArchGuestIrqRoute {
    pub physical_irq: usize,
    pub guest_vector: usize,
}

pub fn init() {
    LOONGARCH_GUEST_IRQ_ROUTES.init_once(Mutex::new(BTreeMap::new()));
}

pub fn store_guest_irq_routes(vm_id: usize, routes: Vec<LoongArchGuestIrqRoute>) {
    let mut cache_lock = LOONGARCH_GUEST_IRQ_ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock();
    cache_lock.insert(vm_id, routes);
}

pub fn get_guest_irq_routes(vm_id: usize) -> Vec<LoongArchGuestIrqRoute> {
    LOONGARCH_GUEST_IRQ_ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .get(&vm_id)
        .cloned()
        .unwrap_or_default()
}

pub fn prepare_uefi_fdt_config(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxVmResult {
    info!(
        "VM[{}] uses LoongArch UEFI boot protocol, keeping firmware FDT at {:#x}",
        vm_config.id(),
        UEFI_FIRMWARE_FDT_BASE
    );
    vm_config.update_dtb_load_gpa(Some(UEFI_FIRMWARE_FDT_BASE.into()));
    vm_create_config.kernel.dtb_load_addr = Some(UEFI_FIRMWARE_FDT_BASE);
    Ok(())
}

pub fn prepare_uefi_runtime_config(vm: &AxVMRef) -> AxVmResult {
    store_guest_irq_routes(vm.id(), super::guest_irq_routes(vm)?);
    Ok(())
}
