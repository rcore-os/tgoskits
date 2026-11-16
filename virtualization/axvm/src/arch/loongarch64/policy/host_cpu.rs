pub(crate) fn host_cpucfg(index: usize) -> usize {
    ax_cpu::capability::read_cpucfg(index)
}
