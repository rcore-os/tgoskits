#[path = "../src/arch/loongarch64/ipi_command.rs"]
mod ipi_command;

#[test]
fn loongarch_ipi_command_uses_blocking_transport() {
    let command = ipi_command::make_ipi_send_value(3, 7).expect("valid IPI command");

    assert_ne!(command & ipi_command::IOCSR_IPI_SEND_BLOCKING, 0);
    assert_eq!(command & 0xffff, 7);
    assert_eq!(
        (command >> ipi_command::IOCSR_IPI_SEND_CPU_SHIFT) & 0x7fff,
        3
    );
}

#[test]
fn loongarch_ipi_command_rejects_cpu_ids_that_overlap_the_blocking_bit() {
    assert_eq!(ipi_command::make_ipi_send_value(1 << 15, 0), None);
}
