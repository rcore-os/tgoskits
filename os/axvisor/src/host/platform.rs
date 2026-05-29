use core::sync::atomic::{AtomicUsize, Ordering};

use ax_std::thread;

use crate::{host, vmm};

pub fn hardware_check() {
    crate::host::arch::hardware_check();
}

pub fn enable_virtualization_on_all_cpus() {
    static CORES: AtomicUsize = AtomicUsize::new(0);

    info!("Enabling hardware virtualization support on all cores...");

    hardware_check();

    let cpu_count = host::cpu::cpu_count();

    for cpu_id in 0..cpu_count {
        thread::spawn(move || {
            info!("Core {cpu_id} is initializing hardware virtualization support...");
            assert!(
                host::cpu::bind_current_to_cpu(cpu_id).is_ok(),
                "Initialize CPU affinity failed!"
            );

            info!("Enabling hardware virtualization support on core {cpu_id}");

            vmm::init_timer_percpu();
            host::percpu::init_current_cpu_vmx_state().expect("Failed to initialize per-CPU state");
            host::percpu::hardware_enable_current_cpu().expect("Failed to enable virtualization");

            info!("Hardware virtualization support enabled on core {cpu_id}");

            let _ = CORES.fetch_add(1, Ordering::Release);
        });
    }

    info!("Waiting for all cores to enable virtualization...");

    while CORES.load(Ordering::Acquire) != cpu_count {
        thread::yield_now();
    }

    info!("All cores have enabled hardware virtualization support.");
}
