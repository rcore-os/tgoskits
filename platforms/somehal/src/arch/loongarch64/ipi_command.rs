pub(crate) const IOCSR_IPI_SEND_CPU_SHIFT: u32 = 16;
pub(crate) const IOCSR_IPI_SEND_BLOCKING: u32 = 1 << 31;

const MAX_CPU_ID: usize = ((IOCSR_IPI_SEND_BLOCKING - 1) >> IOCSR_IPI_SEND_CPU_SHIFT) as usize;
const MAX_VECTOR: u32 = (1 << IOCSR_IPI_SEND_CPU_SHIFT) - 1;

/// Builds the IOCSR IPI transport command used by native Linux as well as
/// LoongArch firmware environments. Bit 31 makes the IOCSR write wait until
/// the transport accepts the command.
pub(crate) fn make_ipi_send_value(cpu_id: usize, vector: u32) -> Option<u32> {
    if cpu_id > MAX_CPU_ID || vector > MAX_VECTOR {
        return None;
    }
    Some(IOCSR_IPI_SEND_BLOCKING | ((cpu_id as u32) << IOCSR_IPI_SEND_CPU_SHIFT) | vector)
}
