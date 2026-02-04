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

fn main() {
    println!("cargo:rerun-if-env-changed=AXVISOR_SMP");
    println!("cargo:rerun-if-changed=linker.lds.S");

    let mut smp = 1;
    if let Ok(s) = std::env::var("AXVISOR_SMP") {
        smp = s.parse::<usize>().unwrap_or(1);
    }

    let ld_content = include_str!("linker.lds.S");
    let ld_content = ld_content.replace("%ARCH%", "i386:x86-64");
    let ld_content =
        ld_content.replace("%KERNEL_BASE%", &format!("{:#x}", 0xffff800000200000usize));
    let ld_content = ld_content.replace("%SMP%", &format!("{smp}",));

    // target/<target_triple>/<mode>/build/axvisor-xxxx/out
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = std::path::Path::new(&out_dir).join("link.x");
    println!("cargo:rustc-link-search={out_dir}");
    std::fs::write(out_path, ld_content).unwrap();
}
