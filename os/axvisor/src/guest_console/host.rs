//! Physical host-console ownership.

use anyhow::{Result, bail};
use ax_std::os::arceos::modules::ax_task;
use axvm::AxVMRef;

fn console_reader_isolation_cpu(
    host_cpu_count: usize,
    vcpu_masks: impl IntoIterator<Item = Option<usize>>,
) -> Option<usize> {
    let tracked_cpu_count = host_cpu_count.min(usize::BITS as usize);
    if tracked_cpu_count == 0 {
        return None;
    }

    let online_bits = if tracked_cpu_count == usize::BITS as usize {
        usize::MAX
    } else {
        (1usize << tracked_cpu_count) - 1
    };
    let guest_bits = vcpu_masks.into_iter().fold(0usize, |used, mask| {
        let requested = mask.unwrap_or(online_bits) & online_bits;
        // A missing, empty, or offline-only mask lets the runtime choose a
        // fallback CPU, so no CPU can be proven host-only in that case.
        used | if requested == 0 {
            online_bits
        } else {
            requested
        }
    });
    let host_only_bits = online_bits & !guest_bits;
    (host_only_bits != 0).then(|| host_only_bits.trailing_zeros() as usize)
}

/// Configures the polling owner for physical host-console input.
///
/// The console multiplexer remains the only physical UART reader. Input IRQs
/// stay disabled until the host UART IRQ contract can transfer received bytes
/// to that owner without introducing a second reader.
pub(crate) fn configure_host_console_reader(vms: &[AxVMRef]) -> Result<()> {
    axvm::host::console::set_input_irq_enabled(false);

    let isolation_cpu = console_reader_isolation_cpu(
        axvm::host::cpu::count(),
        vms.iter()
            .flat_map(|vm| vm.vcpu_snapshots())
            .map(|vcpu| vcpu.phys_cpu_set),
    );

    // Temporary polling and scheduler-isolation workaround.
    //
    // A pinned, continuously runnable vCPU can starve the polling console
    // reader on the cooperative FIFO scheduler. The raw UART IRQ contract does
    // not drain RX into a mux-owned queue or wake this task, so enabling it can
    // leave a level source asserted without making the sole reader runnable.
    // When the validated VM topology leaves a CPU outside every explicit vCPU
    // mask, place the reader there before any vCPU task starts.
    //
    // This never rewrites a vCPU mask or guesses when a mask is absent. Remove
    // the placement after same-CPU FIFO fairness guarantees host-service
    // progress; re-enable RX IRQs only after the top half drains into a bounded
    // mux-owned queue and performs an IRQ-safe task wake.
    let Some(owner_cpu) = isolation_cpu else {
        return Ok(());
    };
    let owner_affinity = ax_task::AxCpuMask::one_shot(owner_cpu);
    if !ax_task::set_current_affinity(owner_affinity) {
        bail!("failed to pin the host console reader to CPU {owner_cpu}");
    }
    let actual_owner_cpu = axvm::host::cpu::current_id();
    if actual_owner_cpu != owner_cpu {
        bail!(
            "host console reader affinity selected CPU {owner_cpu}, but migration ended on CPU \
             {actual_owner_cpu}"
        );
    }

    Ok(())
}

/// Reads at most one byte from the physical host console.
///
/// No other Axvisor component may call the platform console input API.
pub(crate) fn read_host_byte() -> Option<u8> {
    let mut byte = [0u8; 1];
    (axvm::host::console::read_bytes(&mut byte) == 1).then_some(byte[0])
}

/// Yields between polling attempts so other runnable host tasks can progress.
pub(crate) fn wait_for_host_input() {
    std::thread::yield_now();
}

pub(super) fn write_host_bytes(bytes: &[u8]) {
    axvm::host::console::write_bytes(bytes);
}

#[cfg(test)]
mod tests {
    use super::console_reader_isolation_cpu;

    #[test]
    fn isolation_requires_a_cpu_excluded_by_every_vcpu() {
        assert_eq!(
            console_reader_isolation_cpu(4, [Some(0b0010), Some(0b1000)]),
            Some(0)
        );
        assert_eq!(console_reader_isolation_cpu(4, [None]), None);
        assert_eq!(console_reader_isolation_cpu(4, [Some(0)]), None);
        assert_eq!(console_reader_isolation_cpu(4, [Some(usize::MAX)]), None);
    }
}
