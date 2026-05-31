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

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut stdout = axvisor_api::fs::stdout();
        let _ = write!(&mut stdout, $($arg)*);
    }};
}

macro_rules! println {
    () => {
        print!("\n")
    };
    ($fmt:expr $(, $($arg:tt)+)?) => {
        print!(concat!($fmt, "\n") $(, $($arg)+)?)
    };
}

pub mod shell;
pub mod vmm;
