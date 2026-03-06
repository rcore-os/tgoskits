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

extern crate axklib;

use core::ptr::NonNull;

use rdrive::probe::OnProbeError;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[allow(unused_imports)]
#[macro_use]
extern crate log;

mod blk;
mod soc;
// mod serial;

#[allow(unused)]
fn iomap(base: u64, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mem::iomap((base as usize).into(), size)
        .map(|ptr| unsafe { NonNull::new_unchecked(ptr.as_mut_ptr()) })
        .map_err(|e| OnProbeError::Other(format!("{e}:?").into()))
}
