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

//! Source-level ownership contract for the RISC-V guest/host handoff.
//!
//! Guest `gp`, `tp`, and `sscratch` may not be observed by Rust or by any
//! host per-CPU access. Restore all three host-owned values as soon as the
//! guest register image is durable, while the vCPU remains CPU-pinned.

const TRAP_ASM: &str = include_str!("../src/trap.S");
const TRAP_RUST: &str = include_str!("../src/trap.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing `{start}`"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing `{end}` after `{start}`"))
        .0
}

fn assert_in_order(source: &str, markers: &[&str]) {
    let mut previous = 0;
    for (index, marker) in markers.iter().enumerate() {
        let offset = source
            .find(marker)
            .unwrap_or_else(|| panic!("missing `{marker}`"));
        if index != 0 {
            assert!(
                previous < offset,
                "`{}` must precede `{marker}`",
                markers[index - 1]
            );
        }
        previous = offset;
    }
}

#[test]
fn vm_exit_restores_host_anchors_immediately_after_guest_state_is_durable() {
    let exit = section(TRAP_ASM, "_guest_exit:", "ret");
    let restore = section(TRAP_ASM, ".macro RESTORE_HOST_ANCHORS", ".endm");

    assert_in_order(
        exit,
        &[
            "sd   gp, ({guest_gp})(a0)",
            "sd   tp, ({guest_tp})(a0)",
            "sd    t0, ({guest_a0})(a0)",
            "RESTORE_HOST_ANCHORS",
            "_restore_csrs:",
        ],
    );
    assert_in_order(
        restore,
        &[
            "ld    t1, ({hyp_sscratch})(a0)",
            "csrw  sscratch, t1",
            "ld    gp, ({hyp_gp})(a0)",
            "ld    tp, ({hyp_tp})(a0)",
        ],
    );

    let after_restore = exit
        .split_once("RESTORE_HOST_ANCHORS")
        .expect("host-anchor restore in the exit path")
        .1;
    assert_eq!(
        after_restore.matches("({hyp_sscratch})(a0)").count(),
        0,
        "sscratch must have one authoritative restore point"
    );
    assert_eq!(
        after_restore.matches("({hyp_gp})(a0)").count(),
        0,
        "gp must have one authoritative restore point"
    );
    assert_eq!(
        after_restore.matches("({hyp_tp})(a0)").count(),
        0,
        "tp must have one authoritative restore point"
    );
}

#[test]
fn host_anchor_offsets_come_from_the_typed_register_image() {
    for binding in [
        "hyp_gp = const hyp_gpr_offset(GprIndex::GP)",
        "hyp_tp = const hyp_gpr_offset(GprIndex::TP)",
        "hyp_sscratch = const hyp_csr_offset!(sscratch)",
    ] {
        assert!(
            TRAP_RUST.contains(binding),
            "guest assembly offset must be derived from Rust layout: {binding}"
        );
    }

    let exit = section(TRAP_ASM, "_guest_exit:", "ret");
    assert!(
        !exit.contains("call ") && !exit.contains("tail "),
        "the VM-exit window must not call Rust or helpers before host anchors are live"
    );
}
