use heapless::Vec;
use spin::Once;

const MAX_CACHED_CPUS: usize = 256;

type CpuIdList = Vec<usize, MAX_CACHED_CPUS>;

static CPU_IDS: Once<CpuIdList> = Once::new();

pub(super) fn cpu_id_list() -> impl Iterator<Item = usize> {
    CPU_IDS.call_once(discover_cpu_ids).iter().copied()
}

fn discover_cpu_ids() -> CpuIdList {
    let mut ids = CpuIdList::new();

    if let Some(iter) = crate::acpi::cpu_id_list() {
        push_cpu_ids(&mut ids, iter);
        if !ids.is_empty() {
            return ids;
        }
    }

    if let Some(iter) = crate::fdt::cpu_id_list() {
        push_cpu_ids(&mut ids, iter);
        if !ids.is_empty() {
            return ids;
        }
    }

    ids.push(0).ok();
    ids
}

fn push_cpu_ids(ids: &mut CpuIdList, iter: impl Iterator<Item = usize>) {
    for id in iter {
        if ids.push(id).is_err() {
            warn!("someboot CPU ID cache is full; ignoring CPU id {id:#x}");
            break;
        }
    }
}
