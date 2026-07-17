// Copyright 2026 The TGOSKits Authors
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

//! CPU-local platform dispatch must terminate at the architecture register
//! leaf instead of re-entering the client facade or a per-CPU consumer.

const PLATFORM_PROVIDER: &str = include_str!("../src/percpu.rs");
const PLATFORM_BOOT: &str = include_str!("../src/boot.rs");
const REGISTER_LEAF: &str = include_str!("../../../components/ax-cpu-local/src/register.rs");

#[test]
fn steady_state_binding_provider_terminates_at_the_raw_register_leaf() {
    let provider = function_body(PLATFORM_PROVIDER, "fn current_cpu_binding()", "fn get_tp()");

    assert!(
        provider.contains("ax_cpu_local::raw::current_binding(&pin)"),
        "the dynamic provider must read the already-installed architecture anchor directly"
    );
    for forbidden in [
        "ax_cpu_local::platform::",
        "ax_percpu::",
        "ax_hal::",
        "ax_plat::percpu::",
    ] {
        assert!(
            !provider.contains(forbidden),
            "the CPU-local provider must not re-enter higher-level consumer {forbidden}"
        );
    }
}

#[test]
fn offline_boot_binding_verifies_the_register_without_trait_ffi_reentry() {
    let binder = function_body(
        PLATFORM_BOOT,
        "fn bind_current_cpu(binding: CpuBindingV1)",
        "pub fn boot_stack_bounds",
    );

    assert!(
        binder.contains("ax_cpu_local::raw::current_binding(&pin)"),
        "the one-time offline binder must verify the committed raw anchor directly"
    );
    assert!(
        !binder.contains("ax_cpu_local::platform::current_cpu_binding()"),
        "the provider owner must not call back through its own linked client facade"
    );
}

#[test]
fn architecture_register_leaf_has_no_platform_client_dependency() {
    assert!(
        !REGISTER_LEAF.contains("crate::platform::")
            && !REGISTER_LEAF.contains("ax_cpu_local::platform::")
            && !REGISTER_LEAF.contains("ax_percpu::"),
        "the architecture register trust root must not depend on platform dispatch or per-CPU \
         layout"
    );
}

fn function_body<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing function start {start}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing function end {end}"))
        .0
}
