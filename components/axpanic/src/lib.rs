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

#![no_std]

use core::sync::atomic::{AtomicUsize, Ordering};

const PANIC_CPU_INVALID: usize = usize::MAX;

static PANIC_CPU: AtomicUsize = AtomicUsize::new(PANIC_CPU_INVALID);
static OOPS_IN_PROGRESS: AtomicUsize = AtomicUsize::new(0);

/// Classifies how the current CPU is entering the panic path.
///
/// `Primary` means this CPU won ownership of the panic main path.
/// `Recursive` means the same CPU re-entered panic while already owning it.
/// `Concurrent` means another CPU already owns the panic main path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanicDisposition {
    Primary,
    Recursive,
    Concurrent,
}

#[must_use]
pub struct OopsGuard;

impl OopsGuard {
    fn new() -> Self {
        OOPS_IN_PROGRESS.fetch_add(1, Ordering::Release);
        Self
    }
}

impl Drop for OopsGuard {
    fn drop(&mut self) {
        OOPS_IN_PROGRESS.fetch_sub(1, Ordering::Release);
    }
}

pub fn classify_panic(current_cpu: usize) -> PanicDisposition {
    match PANIC_CPU.compare_exchange(
        PANIC_CPU_INVALID,
        current_cpu,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => PanicDisposition::Primary,
        Err(owner_cpu) if owner_cpu == current_cpu => PanicDisposition::Recursive,
        Err(_) => PanicDisposition::Concurrent,
    }
}

/// Returns whether the current system is already in an oops/panic-like path.
///
/// This is intended as a conservative global hint for output and debug paths to
/// avoid complex or lock-heavy behavior while the kernel is unwinding a fatal
/// path.
pub fn oops_in_progress() -> bool {
    OOPS_IN_PROGRESS.load(Ordering::Acquire) != 0
}

/// Marks the current scope as running inside an oops/panic-like path.
pub fn enter_oops() -> OopsGuard {
    OopsGuard::new()
}
