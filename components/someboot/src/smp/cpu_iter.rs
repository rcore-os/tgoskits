#[derive(Clone, Copy)]
enum CpuIdSource {
    Unknown,
    Acpi,
    Fdt,
    Default,
    Done,
}

pub(super) fn cpu_id_list() -> impl Iterator<Item = usize> {
    CpuIdIter::new()
}

struct CpuIdIter {
    source: CpuIdSource,
    next_index: usize,
    pending_first: Option<usize>,
    default_emitted: bool,
}

impl CpuIdIter {
    fn new() -> Self {
        Self {
            source: CpuIdSource::Unknown,
            next_index: 0,
            pending_first: None,
            default_emitted: false,
        }
    }

    fn select_source(&mut self) {
        if let Some(mut iter) = crate::acpi::cpu_id_list()
            && let Some(first) = iter.next()
        {
            self.source = CpuIdSource::Acpi;
            self.pending_first = Some(first);
            self.next_index = 1;
            return;
        }

        if let Some(mut iter) = crate::fdt::cpu_id_list()
            && let Some(first) = iter.next()
        {
            self.source = CpuIdSource::Fdt;
            self.pending_first = Some(first);
            self.next_index = 1;
            return;
        }

        self.source = CpuIdSource::Default;
    }

    fn next_from_acpi(&mut self) -> Option<usize> {
        if let Some(id) = self.pending_first.take() {
            return Some(id);
        }

        let mut iter = crate::acpi::cpu_id_list()?;
        let id = iter.nth(self.next_index)?;
        self.next_index += 1;
        Some(id)
    }

    fn next_from_fdt(&mut self) -> Option<usize> {
        if let Some(id) = self.pending_first.take() {
            return Some(id);
        }

        let mut iter = crate::fdt::cpu_id_list()?;
        let id = iter.nth(self.next_index)?;
        self.next_index += 1;
        Some(id)
    }
}

impl Iterator for CpuIdIter {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.source {
                CpuIdSource::Unknown => self.select_source(),
                CpuIdSource::Acpi => {
                    if let Some(id) = self.next_from_acpi() {
                        return Some(id);
                    }
                    self.source = CpuIdSource::Done;
                }
                CpuIdSource::Fdt => {
                    if let Some(id) = self.next_from_fdt() {
                        return Some(id);
                    }
                    self.source = CpuIdSource::Done;
                }
                CpuIdSource::Default => {
                    if self.default_emitted {
                        self.source = CpuIdSource::Done;
                    } else {
                        self.default_emitted = true;
                        return Some(0);
                    }
                }
                CpuIdSource::Done => return None,
            }
        }
    }
}
