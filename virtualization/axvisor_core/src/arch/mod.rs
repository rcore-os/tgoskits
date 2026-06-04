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

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

#[cfg(target_arch = "loongarch64")]
pub mod loongarch64;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
pub(crate) fn clean_dcache_range(addr: ax_memory_addr::VirtAddr, size: usize) {
    #[cfg(target_arch = "aarch64")]
    aarch64::clean_dcache_range(addr, size);

    #[cfg(target_arch = "loongarch64")]
    loongarch64::clean_dcache_range(addr, size);
}

pub fn hardware_check() {
    #[cfg(target_arch = "aarch64")]
    aarch64::hardware_check();

    #[cfg(target_arch = "loongarch64")]
    loongarch64::hardware_check();

    #[cfg(target_arch = "riscv64")]
    riscv64::hardware_check();

    #[cfg(target_arch = "x86_64")]
    x86_64::hardware_check();
}
