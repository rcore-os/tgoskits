use crate::{ArchTrait, arch::Arch};

enum CpuIdIterState {
    Unknown,
    Acpi(CpuIdOrder),
    Fdt(CpuIdOrder),
    Default,
    Done,
}

pub(super) fn cpu_id_list() -> impl Iterator<Item = usize> {
    CpuIdIter::new()
}

struct CpuIdIter {
    state: CpuIdIterState,
}

impl CpuIdIter {
    fn new() -> Self {
        Self {
            state: CpuIdIterState::Unknown,
        }
    }

    fn select_source() -> CpuIdIterState {
        let boot_cpu_id = Arch::cpu_current_hartid();
        if let Some(cpu_ids) = crate::acpi::cpu_id_list()
            && let Some(order) = CpuIdOrder::new(cpu_ids, boot_cpu_id)
        {
            return CpuIdIterState::Acpi(order);
        }

        if let Some(cpu_ids) = crate::fdt::cpu_id_list()
            && let Some(order) = CpuIdOrder::new(cpu_ids, boot_cpu_id)
        {
            return CpuIdIterState::Fdt(order);
        }

        CpuIdIterState::Default
    }
}

impl Iterator for CpuIdIter {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let next_cpu_id = match &mut self.state {
                CpuIdIterState::Unknown => {
                    self.state = Self::select_source();
                    continue;
                }
                CpuIdIterState::Acpi(order) => {
                    crate::acpi::cpu_id_list().and_then(|cpu_ids| order.next(cpu_ids))
                }
                CpuIdIterState::Fdt(order) => {
                    crate::fdt::cpu_id_list().and_then(|cpu_ids| order.next(cpu_ids))
                }
                CpuIdIterState::Default => {
                    self.state = CpuIdIterState::Done;
                    return Some(0);
                }
                CpuIdIterState::Done => return None,
            };

            if next_cpu_id.is_some() {
                return next_cpu_id;
            }
            self.state = CpuIdIterState::Done;
        }
    }
}

enum CpuIdOrder {
    EmitBoot {
        boot_cpu_id: usize,
    },
    WalkFirmware {
        next_index: usize,
        skip_cpu_id: Option<usize>,
    },
}

impl CpuIdOrder {
    fn new(mut cpu_ids: impl Iterator<Item = usize>, boot_cpu_id: usize) -> Option<Self> {
        let first_cpu_id = cpu_ids.next()?;
        let contains_boot_cpu = first_cpu_id == boot_cpu_id || cpu_ids.any(|id| id == boot_cpu_id);

        if contains_boot_cpu {
            Some(Self::EmitBoot { boot_cpu_id })
        } else {
            Some(Self::WalkFirmware {
                next_index: 0,
                skip_cpu_id: None,
            })
        }
    }

    fn next(&mut self, cpu_ids: impl Iterator<Item = usize>) -> Option<usize> {
        match self {
            Self::EmitBoot { boot_cpu_id } => {
                let boot_cpu_id = *boot_cpu_id;
                *self = Self::WalkFirmware {
                    next_index: 0,
                    skip_cpu_id: Some(boot_cpu_id),
                };
                Some(boot_cpu_id)
            }
            Self::WalkFirmware {
                next_index,
                skip_cpu_id,
            } => {
                for (firmware_index, cpu_id) in cpu_ids.enumerate().skip(*next_index) {
                    *next_index = firmware_index + 1;
                    if Some(cpu_id) != *skip_cpu_id {
                        return Some(cpu_id);
                    }
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::CpuIdOrder;

    #[test]
    fn nonzero_boot_cpu_becomes_logical_cpu_zero() {
        let firmware_cpu_ids = [0, 1, 2, 0x103];
        let mut order = CpuIdOrder::new(firmware_cpu_ids.into_iter(), 0x103).unwrap();
        let ordered_cpu_ids: Vec<_> =
            core::iter::from_fn(|| order.next(firmware_cpu_ids.into_iter())).collect();

        assert_eq!(ordered_cpu_ids, [0x103, 0, 1, 2]);
    }

    #[test]
    fn firmware_order_is_preserved_when_boot_cpu_is_missing() {
        let firmware_cpu_ids = [1, 2, 3, 4];
        let mut order = CpuIdOrder::new(firmware_cpu_ids.into_iter(), 0).unwrap();
        let ordered_cpu_ids: Vec<_> =
            core::iter::from_fn(|| order.next(firmware_cpu_ids.into_iter())).collect();

        assert_eq!(ordered_cpu_ids, firmware_cpu_ids);
    }
}
