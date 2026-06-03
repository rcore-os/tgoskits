// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicUsize, Ordering};

use axvisor_api::host;
use axvm::AxVMPerCpu;

use crate::vmm;

/// Startup banners printed before the hypervisor begins initialization.
///
/// A banner is selected at runtime using the wall clock. This keeps boot output
/// slightly varied without introducing any state or configuration dependency.
const LOGO: [&str; 2] = [
    r#"
       d8888            888     888  d8b
      d88888            888     888  Y8P
     d88P888            888     888
    d88P 888  888  888  Y88b   d88P  888  .d8888b    .d88b.   888d888
   d88P  888  `Y8bd8P'   Y88b d88P   888  88K       d88""88b  888P"
  d88P   888    X88K      Y88o88P    888  "Y8888b.  888  888  888
 d8888888888  .d8""8b.     Y888P     888       X88  Y88..88P  888
d88P     888  888  888      Y8P      888   88888P'   "Y88P"   888
"#,
    r#"
    _         __     ___
   / \   __  _\ \   / (_)___  ___  _ __
  / _ \  \ \/ /\ \ / /| / __|/ _ \| '__|
 / ___ \  >  <  \ V / | \__ \ (_) | |
/_/   \_\/_/\_\  \_/  |_|___/\___/|_|
"#,
];

#[ax_percpu::def_percpu]
static mut AXVM_PER_CPU: AxVMPerCpu = AxVMPerCpu::new_uninit();

/// Run the host-neutral AxVisor boot flow.
pub fn run() {
    print_logo();

    info!("Starting virtualization...");
    info!("Hardware support: {:?}", axvm::has_hardware_support());
    ensure_hardware_support();
    host::prepare_virtualization();
    enable_virtualization_on_all_cores();

    vmm::init();
    vmm::start();

    info!("[OK] Default guest initialized");

    #[cfg(feature = "shell")]
    crate::shell::console_init();
}

fn print_logo() {
    let elapsed = (axvisor_api::time::current_time_nanos() / 1_000) as usize;
    let logo = LOGO[elapsed % LOGO.len()];

    crate::println!();
    crate::println!("{}", logo);
    crate::println!();
    crate::println!("by AxVisor Team");
    crate::println!();
}

fn ensure_hardware_support() {
    if axvm::has_hardware_support() {
        return;
    }

    #[cfg(target_arch = "loongarch64")]
    panic!(
        "LoongArch virtualization extensions are unavailable. Use a virtualization-capable \
         LoongArch QEMU build such as QEMU-LVZ instead of stock qemu-system-loongarch64."
    );

    #[cfg(not(target_arch = "loongarch64"))]
    panic!("Hardware does not support virtualization");
}

fn enable_virtualization_on_all_cores() {
    static CORES: AtomicUsize = AtomicUsize::new(0);

    info!("Enabling hardware virtualization support on all cores...");

    crate::arch::hardware_check();

    let cpu_count = host::get_host_cpu_num();

    for cpu_id in 0..cpu_count {
        host::spawn_cpu_init_task(
            cpu_id,
            Box::new(move || {
                info!("Core {cpu_id} is initializing hardware virtualization support...");
                info!("Enabling hardware virtualization support on core {cpu_id}");

                vmm::init_timer_percpu();

                // SAFETY: This closure runs on the target CPU exactly once during
                // early bring-up, so initializing its per-CPU virtualization state
                // here is safe.
                #[allow(static_mut_refs)]
                let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
                percpu
                    .init(cpu_id)
                    .expect("Failed to initialize per-CPU state");
                percpu
                    .hardware_enable()
                    .expect("Failed to enable virtualization");

                info!("Hardware virtualization support enabled on core {cpu_id}");

                let _ = CORES.fetch_add(1, Ordering::Release);
            }),
        );
    }

    info!("Waiting for all cores to enable hardware virtualization...");

    while CORES.load(Ordering::Acquire) != cpu_count {
        axvisor_api::task::yield_now();
    }

    info!("All cores have enabled hardware virtualization support.");
}
