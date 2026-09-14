use std::{thread, vec::Vec};

use ax_std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_hal::{percpu::this_cpu_id, topology::cpu_capacity},
    task::sched::cpu_topology_len,
};

pub fn run() -> crate::TestResult {
    let cpu_num = cpu_topology_len().expect("runtime topology must be published");
    assert!(cpu_num > 0);
    let mut workers = Vec::with_capacity(cpu_num);
    for cpu_id in 0..cpu_num {
        workers.push(thread::spawn(move || {
            ax_set_current_affinity(AxCpuMask::one_shot(cpu_id))
                .expect("capacity reader must run on the selected CPU");
            assert_eq!(this_cpu_id(), cpu_id);
            // Suite QEMU firmware has no asymmetric capacity properties.
            // Read both local and remote metadata from every online CPU.
            for target in 0..cpu_num {
                assert_eq!(cpu_capacity(target), Some(1024));
            }
            assert_eq!(cpu_capacity(usize::MAX), None);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    Ok(())
}
