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
fn vcpu_irq_owner_retains_placement_until_same_thread_close() {
    let architecture = include_str!("../src/architecture/ops.rs");
    let runtime = include_str!("../src/runtime/vcpus.rs");
    let passthrough = include_str!("../src/vm/passthrough_access.rs");
    let route_lease = include_str!("../src/host/irq_routes.rs");

    assert!(
        architecture.contains("prepare_vcpu_irq_owner"),
        "an architecture must explicitly opt into the long-lived IRQ-owner session"
    );
    let acquire = runtime
        .find("CurrentArch::prepare_vcpu_irq_owner")
        .expect("the vCPU task must acquire its architecture IRQ-owner session");
    let activate = runtime
        .find("CurrentArch::before_first_run")
        .expect("the vCPU task must run its first-run activation hook");
    assert!(
        acquire < activate,
        "the non-migratable owner session must exist before IRQ registration"
    );
    assert!(runtime.contains("wait_for(runtime, || owner.release_requested())"));
    assert!(runtime.contains("owner.close()"));
    assert!(runtime.contains("quarantine_vcpu_irq_owner"));

    assert!(
        passthrough.contains("join_after_passthrough_irq_revocation"),
        "vCPU task join must be a distinct post-route-revocation phase"
    );
    let request = route_lease
        .find("CurrentArch::revoke_guest_irq_routes")
        .expect("the manager must request architecture route revocation");
    let join = route_lease
        .find("join_after_passthrough_irq_revocation")
        .expect("the manager must join vCPU owners only after their actions close");
    assert!(request < join);
}

#[test]
fn loongarch_manager_requests_but_owner_releases_the_action() {
    let route = include_str!("../src/arch/loongarch64/irq/mod.rs");

    assert!(route.contains("request_guest_irq_route_revocation"));
    assert!(route.contains("owner_release_guest_irq_routes"));
    assert!(route.contains("ROUTE_COMPLETION"));
    assert!(route.contains("ThreadWakeHandle"));
    assert!(route.contains("wake_route_owner"));
    assert!(
        !route.contains("notify_all_vcpus"),
        "manager and guest EOI paths must use the retained owner wake instead of a VM lookup"
    );

    let manager = route
        .split_once("pub fn revoke_guest_irq_routes")
        .expect("LoongArch must expose manager-side route revocation")
        .1
        .split_once("fn request_guest_irq_route_revocation")
        .expect("manager entry must remain separate from its request helper")
        .0;
    for forbidden in [
        "disable_irq",
        "synchronize_irq",
        "release_irq_quench",
        "free_irq",
    ] {
        assert!(
            !manager.contains(forbidden),
            "the manager must not perform owner-local IRQ operation {forbidden}"
        );
    }
    assert!(manager.contains("wake_route_owner(&wake"));

    let eoi_publisher = route
        .split_once("pub(super) fn complete_guest_irq_route")
        .expect("guest PCH-PIC EOI must publish a route rearm fact")
        .1
        .split_once("pub(super) fn service_guest_irq_owner")
        .expect("EOI publication and owner service must remain separate")
        .0;
    assert!(eoi_publisher.contains("request_rearm"));
    assert!(eoi_publisher.contains("wake_route_owner(&wake"));
    for forbidden in ["synchronize_irq", "release_irq_quench", "free_irq"] {
        assert!(
            !eoi_publisher.contains(forbidden),
            "a foreign vCPU EOI must not perform owner-local operation {forbidden}"
        );
    }

    let owner_rearm = route
        .split_once("pub(super) fn service_guest_irq_owner")
        .expect("the fixed owner must service generation-bearing EOI facts")
        .1
        .split_once("pub fn revoke_guest_irq_routes")
        .expect("owner rearm and manager revoke must remain separate")
        .0;
    assert!(owner_rearm.contains("synchronize_irq(route.handle)"));
    assert!(owner_rearm.contains("release_irq_quench(route.handle)"));

    let owner = route
        .split_once("fn owner_release_guest_irq_routes")
        .expect("the registering vCPU must own the close implementation")
        .1;
    for required in [
        "disable_irq(route.handle)",
        "synchronize_irq(route.handle)",
        "release_irq_quench(route.handle)",
        "free_irq(route.handle)",
    ] {
        assert!(
            owner.contains(required),
            "the owner close path must retain {required}"
        );
    }
}

#[test]
fn riscv_platform_route_is_released_by_its_fixed_vcpu_owner() {
    let architecture = include_str!("../src/arch/riscv64/mod.rs");
    let route = include_str!("../src/arch/riscv64/irq.rs");

    assert!(
        architecture.contains("fn prepare_vcpu_irq_owner"),
        "RISC-V must retain a CurrentCpuLease for the complete physical PLIC route lifetime"
    );
    assert!(
        route.contains("request_guest_irq_route_revocation")
            && route.contains("owner_release_guest_irq_route")
            && route.contains("ROUTE_RELEASE_COMPLETION"),
        "RISC-V route teardown needs an explicit manager-to-owner transaction"
    );

    let manager = route
        .split_once("pub(crate) fn revoke_guest_irq_routes")
        .expect("RISC-V must expose manager-side route revocation")
        .1
        .split_once("fn request_guest_irq_route_revocation")
        .expect("manager request and owner close must remain separate")
        .0;
    for forbidden in [
        "begin_guest_irq_route_revocation",
        "poll_guest_irq_route_revocation",
        "finish_revocation",
        "revoke_forwarded_route_batch",
    ] {
        assert!(
            !manager.contains(forbidden),
            "the manager must not execute owner-local RISC-V route operation {forbidden}"
        );
    }

    let owner = route
        .split_once("fn owner_release_guest_irq_route")
        .expect("the fixed vCPU must own RISC-V route teardown")
        .1;
    for required in [
        "begin_guest_irq_route_revocation",
        "poll_guest_irq_route_revocation",
        "finish_revocation",
        "revoke_forwarded_route_batch",
    ] {
        assert!(
            owner.contains(required),
            "the fixed vCPU close path must retain {required}"
        );
    }
}

#[test]
fn riscv_owner_session_exists_only_for_a_configured_physical_route() {
    let route = include_str!("../src/arch/riscv64/irq.rs");
    let prepare = route
        .split_once("pub(super) fn prepare_guest_irq_owner_session")
        .expect("RISC-V must prepare its fixed owner before route publication")
        .1
        .split_once("fn guest_irq_owner_session_required")
        .expect("owner preparation must use one route-presence predicate")
        .0;
    assert!(prepare.contains("!guest_irq_owner_session_required"));
    assert!(prepare.contains("return Ok(None)"));

    let required = route
        .split_once("fn guest_irq_owner_session_required")
        .expect("RISC-V must distinguish passthrough mode from an actual PLIC route")
        .1
        .split_once("fn arm_guest_irq_route_release")
        .expect("the route-presence predicate must precede release arming")
        .0;
    assert!(required.contains("VMInterruptMode::Passthrough"));
    assert!(required.contains("pass_through_irqs"));
    assert!(required.contains("!irq_sources.is_empty()"));
}

#[test]
fn riscv_hard_irq_wake_is_restricted_to_the_fixed_owner_cpu() {
    let route = include_str!("../src/arch/riscv64/irq.rs");
    let hard_irq = route
        .split_once("fn forward_unbound_physical_irq")
        .expect("RISC-V must expose its hard-IRQ publication boundary")
        .1
        .split_once("fn encode_claim")
        .expect("the hard-IRQ publication boundary must remain focused")
        .0;
    let cpu_check = hard_irq
        .find("this_cpu_id()")
        .expect("hard IRQ publication must read its actual local CPU");
    let publish = hard_irq
        .find("forward_physical_irq(claim)")
        .expect("hard IRQ publication must wake the fixed vCPU owner");
    assert!(hard_irq.contains("route.route.target_cpu"));
    assert!(
        cpu_check < publish,
        "the fixed-owner CPU check must precede scheduler wake publication"
    );
}

#[test]
fn riscv_owner_close_fails_closed_on_a_foreign_monitor_route() {
    let route = include_str!("../src/arch/riscv64/irq.rs");
    let close = route
        .split_once("fn owner_release_guest_irq_route_inner")
        .expect("RISC-V fixed owner close implementation must exist")
        .1
        .split_once("fn forward_unbound_physical_irq")
        .expect("owner close must remain outside the hard-IRQ publication path")
        .0;
    let mismatch = close
        .split_once("if route_key.vm_id != vm_id")
        .expect("owner close must validate the canonical VM identity")
        .1
        .split_once('}')
        .expect("identity mismatch branch must be bounded")
        .0;
    assert!(mismatch.contains("return Err"));
    assert!(!mismatch.contains("return Ok"));
}

#[test]
fn loongarch_owner_session_exists_only_when_a_route_was_prepared() {
    let route = include_str!("../src/arch/loongarch64/irq/mod.rs");
    let prepare = route
        .split_once("pub(super) fn prepare_guest_irq_owner_session")
        .expect("LoongArch must prepare its owner before action registration")
        .1
        .split_once("fn guest_irq_owner_session_required")
        .expect("owner preparation must use a route-presence predicate")
        .0;
    assert!(prepare.contains("!guest_irq_owner_session_required"));
    assert!(prepare.contains("return Ok(None)"));

    let required = route
        .split_once("fn guest_irq_owner_session_required")
        .expect("LoongArch must determine whether this VM actually owns a route")
        .1
        .split_once("pub(super) fn activate_guest_irq_owner")
        .expect("the route-presence predicate must remain separate from activation")
        .0;
    assert!(required.contains("configuration()"));
    assert!(required.contains("config.vm_id == vm_id"));
    assert!(required.contains("config.vcpu_id == vcpu_id"));
}
