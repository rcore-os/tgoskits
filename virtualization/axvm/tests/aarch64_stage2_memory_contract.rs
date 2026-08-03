// Copyright 2026 The Axvisor Team
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

#[test]
fn cacheable_guest_ram_is_inner_shareable_at_stage_two() {
    let npt = include_str!("../src/arch/aarch64/npt.rs");
    let normal_mapping = npt
        .split_once("MemType::Normal =>")
        .expect("AArch64 stage-2 mappings must define normal memory attributes")
        .1
        .split_once("MemType::NormalNonCache")
        .expect("normal-memory attributes must precede non-cacheable attributes")
        .0;

    assert!(
        normal_mapping.contains("Self::INNER.bits()")
            && normal_mapping.contains("Self::SHAREABLE.bits()"),
        "cacheable guest RAM must encode SH=0b11 (Inner Shareable) so vCPUs on different physical \
         cores observe atomic synchronization"
    );
}
