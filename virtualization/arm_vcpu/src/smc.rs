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

use core::arch::asm;

#[inline(never)]
/// invoke a secure monitor call
/// # Safety:
/// It is unsafe to call this function directly.
/// The caller must ensure that
/// x0 is defined as the SMC function number referenced in the SMC Calling Convention
/// than the args later must be valid for the specified SMC function.
pub unsafe fn smc_call(x0: u64, x1: u64, x2: u64, x3: u64) -> (u64, u64, u64, u64) {
    let r0;
    let r1;
    let r2;
    let r3;
    unsafe {
        asm!(
            "smc #0",
            inout("x0") x0 => r0,
            inout("x1") x1 => r1,
            inout("x2") x2 => r2,
            inout("x3") x3 => r3,
            options(nomem, nostack)
        );
    }
    (r0, r1, r2, r3)
}
