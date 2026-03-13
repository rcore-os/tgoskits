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

use memory_addr::VirtAddr;

use crate::hal::CacheOp;

impl From<CacheOp> for aarch64_cpu_ext::cache::CacheOp {
    fn from(op: CacheOp) -> Self {
        match op {
            CacheOp::Clean => aarch64_cpu_ext::cache::CacheOp::Clean,
            CacheOp::Invalidate => aarch64_cpu_ext::cache::CacheOp::Invalidate,
            CacheOp::CleanAndInvalidate => aarch64_cpu_ext::cache::CacheOp::CleanAndInvalidate,
        }
    }
}

pub fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
    aarch64_cpu_ext::cache::dcache_range(op.into(), addr.as_usize(), size);
}
