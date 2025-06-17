use axerrno::AxResult;

/// The interfaces which the underlying software (kernel or hypervisor) must implement.
pub trait AxVMHal: Sized {
    /// Current time in nanoseconds.
    fn current_time_nanos() -> u64;

    /// Current VM ID.
    fn current_vm_id() -> usize;

    /// Current Virtual CPU ID.
    fn current_vcpu_id() -> usize;

    /// Current Physical CPU ID.
    fn current_pcpu_id() -> usize;

    fn vcpu_num() -> usize;

    fn active_vcpus() -> usize;

    fn vm_apicid2vcpu_id() -> usize;

    /// Get the Physical CPU ID where the specified VCPU of the current VM resides.
    ///
    /// Returns an error if the VCPU is not found.
    fn vcpu_resides_on(vm_id: usize, vcpu_id: usize) -> AxResult<usize>;

    /// Inject an IRQ to the specified VCPU.
    ///
    /// This method should find the physical CPU where the specified VCPU resides and inject the IRQ
    /// to it on that physical CPU with [`axvcpu::AxVCpu::inject_interrupt`].
    ///
    /// Returns an error if the VCPU is not found.
    fn inject_irq_to_vcpu(vm_id: usize, vcpu_id: usize, irq: usize) -> AxResult;
}
