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

use super::GeneralRegisters;
use alloc::format;
use core::fmt::Debug;

/// The comparison result of all general-purpose registers after a change.
pub struct GeneralRegistersDiff {
    old: GeneralRegisters,
    new: GeneralRegisters,
}

impl GeneralRegistersDiff {
    const INDEX_RANGE: core::ops::Range<u8> = 0..16;
    const RSP_INDEX: u8 = 4;

    /// Creates a new `GeneralRegistersDiff` instance by comparing two `GeneralRegisters` instances.
    pub fn new(old: GeneralRegisters, new: GeneralRegisters) -> Self {
        GeneralRegistersDiff { old, new }
    }

    /// Returns `true` if all general-purpose registers are unchanged.
    pub fn is_same(&self) -> bool {
        self.old == self.new
    }
}

impl Debug for GeneralRegistersDiff {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let mut debug = f.debug_struct("GeneralRegistersDiff");

        for i in Self::INDEX_RANGE {
            if i == Self::RSP_INDEX {
                continue;
            }

            let old = self.old.get_reg_of_index(i);
            let new = self.new.get_reg_of_index(i);

            if old != new {
                debug.field(
                    GeneralRegisters::register_name(i),
                    &format!("{old:#x} -> {new:#x}"),
                );
            }
        }

        debug.finish()
    }
}
