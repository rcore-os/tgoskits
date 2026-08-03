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
fn vcpu_run_slice_defers_host_preemption_until_after_unbind() {
    let runtime = include_str!("../src/runtime/vcpus.rs");
    let run_slice = runtime
        .split_once("let run_result =")
        .expect("the vCPU runtime must name the architecture run result")
        .1
        .split_once("match run_result")
        .expect("the protected run slice must end before result handling")
        .0;

    assert!(
        runtime.contains("use ax_kernel_guard::NoPreempt;"),
        "the vCPU runtime must use the host preemption guard explicitly"
    );
    assert!(
        run_slice.contains("NoPreempt::new()")
            && run_slice.contains("CurrentArch::run_vcpu(&vm, &vcpu)"),
        "guest entry, VM exit handling, and backend unbind must stay in one non-preemptible run \
         slice"
    );
    assert!(
        !run_slice.contains("yield_now"),
        "the runtime must consume pending preemption through the scheduler guard, not yield while \
         bound"
    );
}
