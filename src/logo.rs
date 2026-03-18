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

use std::os::arceos::api::time::ax_wall_time;
use std::println;

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

/// Chooses a logo based on the current time.
fn choose_logo() -> &'static str {
    let elapsed = ax_wall_time().as_micros() as usize;
    LOGO[elapsed % LOGO.len()]
}

/// Prints the logo to the console.
pub fn print_logo() {
    println!();
    println!("{}", choose_logo());
    println!();
    println!("by AxVisor Team");
    println!();
}
