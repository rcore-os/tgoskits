//! Loaded per-CPU template metadata.

/// Returns the loaded address of the single CPU-area template.
pub(crate) fn template_base() -> usize {
    cpu_local::cpu_area_template_base()
}

/// Returns the initialized byte size of one runtime CPU area.
pub(crate) fn template_size() -> usize {
    cpu_local::cpu_area_template_size()
        .expect("CPU-area template end sentinel must follow the fixed prefix")
}
