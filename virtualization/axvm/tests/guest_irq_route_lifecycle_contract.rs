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
fn guest_irq_route_lifecycle_is_not_a_filesystem_capability() {
    let host_modules = include_str!("../src/host/mod.rs");
    let exports = include_str!("../src/lib.rs");
    let architecture = include_str!("../src/architecture/ops.rs");

    assert!(
        host_modules.contains("mod irq_routes;"),
        "guest IRQ ownership must live outside the host-storage module"
    );
    assert!(
        exports.contains("host::irq_routes::{")
            && exports.contains("GuestIrqRouteLease")
            && exports.contains("GuestIrqRoutesRevoked"),
        "the route lease and its revocation proof must exist without fs/host-fs"
    );

    let activate = architecture
        .find("fn activate_guest_irq_routes")
        .expect("every architecture must expose an always-on route activation hook");
    let revoke = architecture
        .find("fn revoke_guest_irq_routes")
        .expect("every architecture must expose an always-on route revocation hook");
    let preceding_activate = &architecture[activate.saturating_sub(160)..activate];
    let preceding_revoke = &architecture[revoke.saturating_sub(160)..revoke];
    assert!(
        !preceding_activate.contains("feature = \"fs\"")
            && !preceding_activate.contains("feature = \"host-fs\""),
        "IRQ activation must not disappear with the filesystem feature"
    );
    assert!(
        !preceding_revoke.contains("feature = \"fs\"")
            && !preceding_revoke.contains("feature = \"host-fs\""),
        "IRQ revocation must not disappear with the filesystem feature"
    );
}

#[test]
fn axvisor_manager_retains_route_ownership_with_or_without_fs() {
    let manager = include_str!("../../../os/axvisor/src/manager.rs");
    let field = manager
        .find("guest_irq_route_lease: Option<axvm::GuestIrqRouteLease>")
        .expect("the Axvisor manager must retain the live route lease");
    let preceding = &manager[field.saturating_sub(80)..field];
    assert!(
        !preceding.contains("#[cfg(feature = \"fs\")]")
            && !preceding.contains(r#"#[cfg(any(feature = "fs""#),
        "route ownership is required even when host filesystem support is disabled"
    );
    assert!(manager.contains("axvm::activate_guest_irq_routes"));
    assert!(manager.contains("axvm::revoke_guest_irq_route_lease"));
}

#[test]
fn dropping_an_active_route_lease_enters_a_named_quarantine() {
    let routes = include_str!("../src/host/irq_routes.rs");

    assert!(routes.contains("impl Drop for GuestIrqRouteLease"));
    assert!(routes.contains("quarantine_active_route_lease"));
    assert!(routes.contains("GUEST_IRQ_ROUTE_QUARANTINE"));
    assert!(
        !routes.contains("mem::forget") && !routes.contains("Box::leak"),
        "fail-closed Drop must retain the owned allocation in a named registry"
    );
}
